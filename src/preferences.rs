//! Small, versioned user preferences, independent of eframe's window storage.
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::backend::OutputSettings;
use crate::scene3d::camera::{Camera, CameraState};

pub const APP_ID: &str = "com.macinrender.macindecode-ac4-player";
const VERSION: u32 = 1;

pub struct DataDirectory {
    pub path: PathBuf,
    _lock: File,
}
impl DataDirectory {
    /// Runs before the GUI or any output stream is created.
    pub fn acquire() -> Result<Arc<Self>, String> {
        let path = std::env::var_os("MACINDECODE_PLAYER_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| eframe::storage_dir(APP_ID))
            .ok_or("Cannot locate the application data directory")?;
        Self::at(path)
    }
    pub fn at(path: PathBuf) -> Result<Arc<Self>, String> {
        fs::create_dir_all(&path).map_err(|e| format!("Cannot create {}: {e}", path.display()))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.join("player.lock"))
            .map_err(|e| format!("Cannot open application lock: {e}"))?;
        lock.try_lock().map_err(|e| {
            format!(
                "This data directory is already in use, or cannot be locked: {e}\n{}",
                path.display()
            )
        })?;
        fs::create_dir_all(path.join("sofa"))
            .map_err(|e| format!("Cannot create SOFA directory: {e}"))?;
        Ok(Arc::new(Self { path, _lock: lock }))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppPreferences {
    pub output: OutputSettings,
    pub volume: f32,
    pub muted: bool,
    pub camera: CameraState,
    pub object_numbers: bool,
    pub manual_head: [f32; 3],
    #[serde(with = "crate::playlist::native_path")]
    pub last_directory: PathBuf,
}
impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            output: OutputSettings::default(),
            volume: 0.8,
            muted: false,
            camera: Camera::default().state(),
            object_numbers: true,
            manual_head: [0.0; 3],
            last_directory: PathBuf::new(),
        }
    }
}
impl AppPreferences {
    pub fn validated(mut self) -> Self {
        self.output = self.output.validated();
        self.volume = if self.volume.is_finite() {
            self.volume.clamp(0.0, 1.0)
        } else {
            0.8
        };
        self.camera = Camera::from_state(self.camera).state();
        for (i, value) in self.manual_head.iter_mut().enumerate() {
            let limit = if i == 1 { 85.0 } else { 180.0 };
            *value = if value.is_finite() {
                value.clamp(-limit, limit)
            } else {
                0.0
            };
        }
        self
    }
}

#[derive(Serialize, Deserialize)]
struct Document {
    version: u32,
    preferences: AppPreferences,
}

pub struct PreferencesStore {
    path: PathBuf,
    writable: bool,
}
impl PreferencesStore {
    pub fn load(
        directory: &Path,
        legacy: AppPreferences,
    ) -> (Self, AppPreferences, Option<String>) {
        let path = directory.join("settings.json");
        let mut store = Self {
            path,
            writable: true,
        };
        if !store.path.exists() {
            let prefs = legacy.validated();
            let warning = store.save(&prefs).err();
            return (store, prefs, warning);
        }
        match read_document(&store.path) {
            Ok(prefs) => (store, prefs, None),
            Err(error) => {
                // A newer application's document is not corruption. Never roll
                // it back to an older backup or rewrite unknown fields.
                if error.starts_with("Unsupported preferences version") {
                    store.writable = false;
                    return (store, legacy.validated(), Some(error));
                }
                if let Ok(prefs) = read_document(&store.path.with_extension("json.bak")) {
                    let suffix = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos();
                    let preserved = store.path.with_extension(format!("json.corrupt-{suffix}"));
                    if let Err(copy_error) = fs::copy(&store.path, &preserved) {
                        store.writable = false;
                        return (
                            store,
                            prefs,
                            Some(format!("{error}; backup loaded read-only: {copy_error}")),
                        );
                    }
                    // Do not rotate the corrupt primary over the good backup.
                    let result = store.write_primary(&prefs);
                    (
                        store,
                        prefs,
                        Some(format!(
                            "{error}; loaded the previous preferences; damaged file preserved.{}",
                            result.err().map_or(String::new(), |e| format!(" {e}"))
                        )),
                    )
                } else {
                    store.writable = false;
                    (
                        store,
                        legacy.validated(),
                        Some(format!(
                            "{error}; original preferences preserved. Saving is disabled until the file is repaired."
                        )),
                    )
                }
            }
        }
    }
    fn write_primary(&self, preferences: &AppPreferences) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&Document {
            version: VERSION,
            preferences: preferences.clone().validated(),
        })
        .map_err(|e| e.to_string())?;
        atomic_write(&self.path, &bytes)
    }
    pub fn save(&mut self, preferences: &AppPreferences) -> Result<(), String> {
        if !self.writable {
            return Err("Preferences are read-only; the original file has been preserved".into());
        }
        if self.path.exists() {
            // Validate before rotating, so a damaged file cannot replace the backup.
            read_document(&self.path)?;
            let previous = fs::read(&self.path).map_err(|e| e.to_string())?;
            atomic_write(&self.path.with_extension("json.bak"), &previous)?;
        }
        self.write_primary(preferences)
    }
}

fn read_document(path: &Path) -> Result<AppPreferences, String> {
    let bytes = fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("Invalid preferences: {e}"))?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or("Invalid preferences: missing version")?;
    if version != u64::from(VERSION) {
        return Err(format!("Unsupported preferences version {version}"));
    }
    let document: Document =
        serde_json::from_value(value).map_err(|e| format!("Invalid preferences: {e}"))?;
    Ok(document.preferences.validated())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("Missing preferences directory")?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|()| file.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    file.persist(path)
        .map_err(|e| format!("Preferences were not saved: {e}"))?;
    #[cfg(unix)]
    File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn future_version_with_unknown_settings_never_rolls_back_to_an_old_backup() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, prefs, _) = PreferencesStore::load(dir.path(), AppPreferences::default());
        store.save(&prefs).unwrap();
        let future = br#"{"version":2,"preferences":{"output":{"mode":"FutureRenderer"}}}"#;
        std::fs::write(dir.path().join("settings.json"), future).unwrap();
        let (mut store, prefs, warning) =
            PreferencesStore::load(dir.path(), AppPreferences::default());
        assert!(
            warning
                .unwrap()
                .starts_with("Unsupported preferences version")
        );
        assert!(store.save(&prefs).is_err());
        assert_eq!(
            std::fs::read(dir.path().join("settings.json")).unwrap(),
            future
        );
    }
    #[test]
    fn migration_backup_and_corruption_recovery_preserve_original() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = AppPreferences {
            volume: 0.37,
            ..Default::default()
        };
        let (mut store, loaded, warning) = PreferencesStore::load(dir.path(), legacy);
        assert!(warning.is_none());
        assert!((loaded.volume - 0.37).abs() < f32::EPSILON);
        store
            .save(&AppPreferences {
                volume: 0.6,
                ..loaded
            })
            .unwrap();
        fs::write(dir.path().join("settings.json"), b"interrupted").unwrap();
        let (_, recovered, warning) = PreferencesStore::load(dir.path(), AppPreferences::default());
        assert!((recovered.volume - 0.37).abs() < f32::EPSILON);
        assert!(warning.is_some());
        assert!(
            fs::read_dir(dir.path()).unwrap().any(|p| p
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("corrupt"))
        );
    }
    #[test]
    fn invalid_preferences_without_backup_are_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, b"bad").unwrap();
        let (mut store, prefs, warning) =
            PreferencesStore::load(dir.path(), AppPreferences::default());
        assert!(warning.is_some());
        assert!(store.save(&prefs).is_err());
        assert_eq!(fs::read(path).unwrap(), b"bad");
    }
    #[test]
    fn second_instance_cannot_acquire_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        let first = DataDirectory::at(dir.path().into()).unwrap();
        assert!(DataDirectory::at(dir.path().into()).is_err());
        drop(first);
        assert!(DataDirectory::at(dir.path().into()).is_ok());
    }
}
