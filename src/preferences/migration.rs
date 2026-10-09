//! The one-time adoption of the data directory written under the player's
//! previous name and application ID.
//!
//! The application ID names the directory, so renaming the application would
//! otherwise start every existing user on an empty library. The old directory
//! is *copied*, never moved: it stays intact as the backup and keeps working
//! for an older build, and the copy becomes visible through one rename only
//! once it is complete, so an interrupted copy is never mistaken for adopted
//! data. A failure is an error rather than a fresh start, because a fresh start
//! would create the new directory and no later launch would migrate again.
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

pub const LEGACY_APP_ID: &str = "com.macinrender.macindecode-ac4-player";
const LOCK: &str = "player.lock";
const NOTE: &str = "MOVED.txt";

/// Copies `legacy` to `target` when `target` does not exist yet. Returns
/// whether anything was adopted.
pub fn adopt(legacy: &Path, target: &Path) -> Result<bool, String> {
    if target.exists() || !legacy.is_dir() || legacy == target {
        return Ok(false);
    }
    let fail = |e: io::Error| {
        format!(
            "Cannot carry the library and settings over from {}: {e}\n\
             Nothing was changed there; free space or fix permissions and start again.",
            legacy.display()
        )
    };
    // The old build holds this lock for its whole run. Holding it for the copy
    // keeps the database, its WAL and the settings from changing underneath.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(legacy.join(LOCK))
        .map_err(fail)?;
    lock.try_lock().map_err(|e| {
        format!(
            "MacinDecode AC-4 Player is still using its data directory. Close it, then \
             start MacinDecode Spatial Player again to carry your library over: {e}\n{}",
            legacy.display()
        )
    })?;
    let staging = staging_path(target);
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(fail)?;
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(fail)?;
    }
    copy_tree(legacy, &staging, true).map_err(fail)?;
    fs::rename(&staging, target).map_err(fail)?;
    #[cfg(unix)]
    if let Some(parent) = target.parent() {
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(fail)?;
    }
    drop(lock);
    // Advisory only: the copy is already complete and is what counts.
    let _ = fs::write(
        legacy.join(NOTE),
        format!(
            "MacinDecode Spatial Player copied this directory to\n{}\n\
             and uses that copy from now on. This one is left untouched as a backup;\n\
             delete it once you no longer need to go back to MacinDecode AC-4 Player.\n",
            target.display()
        ),
    );
    Ok(true)
}

/// Settings store the chosen SOFA and headphone profile as absolute paths,
/// and the managed ones live inside the data directory. Rewritten on every
/// load rather than once, so a settings file restored from the old directory
/// later still finds the copies.
pub fn relocate(path: &mut String, legacy: &Path, target: &Path) {
    if let Ok(rest) = Path::new(path.as_str()).strip_prefix(legacy) {
        *path = target.join(rest).to_string_lossy().into_owned();
    }
}

fn staging_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".migrating");
    target.with_file_name(name)
}

fn copy_tree(from: &Path, to: &Path, root: bool) -> io::Result<()> {
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if root && (name == LOCK || name == NOTE) {
            continue;
        }
        let source = entry.path();
        let destination = to.join(&name);
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&source, &destination, false)?;
        } else if kind.is_file() || (kind.is_symlink() && source.is_file()) {
            // A link is carried over as the file it points to; a dangling one
            // has nothing to carry.
            fs::copy(&source, &destination)?;
            File::open(&destination)?.sync_all()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_directory(root: &Path) -> PathBuf {
        let legacy = root.join("old").join("data");
        fs::create_dir_all(legacy.join("sofa")).unwrap();
        fs::write(legacy.join("library.sqlite3"), b"db").unwrap();
        fs::write(legacy.join("settings.json"), b"{}").unwrap();
        fs::write(legacy.join("sofa").join("personal.sofa"), b"hrir").unwrap();
        fs::write(legacy.join(LOCK), b"").unwrap();
        legacy
    }

    #[test]
    fn copies_the_old_directory_and_leaves_it_as_a_backup() {
        let root = tempfile::tempdir().unwrap();
        let legacy = legacy_directory(root.path());
        let target = root.path().join("new").join("data");
        assert!(adopt(&legacy, &target).unwrap());
        assert_eq!(fs::read(target.join("library.sqlite3")).unwrap(), b"db");
        assert_eq!(
            fs::read(target.join("sofa").join("personal.sofa")).unwrap(),
            b"hrir"
        );
        assert!(!target.join(LOCK).exists());
        assert!(!target.join(NOTE).exists());
        assert!(!staging_path(&target).exists());
        assert_eq!(fs::read(legacy.join("library.sqlite3")).unwrap(), b"db");
        assert!(legacy.join(NOTE).exists());
    }

    #[test]
    fn an_existing_directory_is_never_replaced() {
        let root = tempfile::tempdir().unwrap();
        let legacy = legacy_directory(root.path());
        let target = root.path().join("new");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("settings.json"), b"current").unwrap();
        assert!(!adopt(&legacy, &target).unwrap());
        assert_eq!(fs::read(target.join("settings.json")).unwrap(), b"current");
        assert!(!adopt(&root.path().join("missing"), &root.path().join("fresh")).unwrap());
        assert!(!root.path().join("fresh").exists());
    }

    #[test]
    fn a_running_old_build_blocks_the_copy_without_creating_anything() {
        let root = tempfile::tempdir().unwrap();
        let legacy = legacy_directory(root.path());
        let target = root.path().join("new");
        let held = super::super::DataDirectory::at(legacy.clone()).unwrap();
        let error = adopt(&legacy, &target).unwrap_err();
        assert!(error.contains("still using"), "{error}");
        assert!(!target.exists());
        drop(held);
        assert!(adopt(&legacy, &target).unwrap());
    }

    #[test]
    fn an_interrupted_copy_is_discarded_and_redone() {
        let root = tempfile::tempdir().unwrap();
        let legacy = legacy_directory(root.path());
        let target = root.path().join("new");
        let staging = staging_path(&target);
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("library.sqlite3"), b"partial").unwrap();
        assert!(adopt(&legacy, &target).unwrap());
        assert_eq!(fs::read(target.join("library.sqlite3")).unwrap(), b"db");
        assert!(!staging.exists());
    }

    #[test]
    fn only_paths_inside_the_old_directory_are_relocated() {
        let legacy = Path::new("data").join("old");
        let target = Path::new("data").join("new");
        let mut managed = legacy
            .join("sofa")
            .join("a.sofa")
            .to_string_lossy()
            .into_owned();
        relocate(&mut managed, &legacy, &target);
        assert_eq!(Path::new(&managed), target.join("sofa").join("a.sofa"));
        let outside = Path::new("music").join("b.sofa");
        let mut external = outside.to_string_lossy().into_owned();
        relocate(&mut external, &legacy, &target);
        assert_eq!(Path::new(&external), outside);
        let mut empty = String::new();
        relocate(&mut empty, &legacy, &target);
        assert!(empty.is_empty());
    }
}
