//! Managed skin imports. Only a successfully decoded and durably copied PNG
//! becomes selectable; decoding and file I/O stay off the UI thread.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use eframe::egui;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::scene3d::skin::{BodyType, MAX_PNG_BYTES, Skin};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub model: Option<BodyType>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub entries: Vec<Entry>,
    pub selected: Option<String>,
}

struct Loaded {
    entry: Entry,
    skin: Skin,
}

pub struct Catalog {
    pub root: PathBuf,
    pub active: Option<Skin>,
    pub error: Option<String>,
    pending: Option<Receiver<Result<Loaded, String>>>,
}

impl Catalog {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            active: None,
            error: None,
            pending: None,
        }
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub fn import(&mut self, path: PathBuf, context: &egui::Context) {
        let root = self.root.clone();
        self.start(context, move || import_file(&root, &path));
    }

    pub fn select(&mut self, entry: Entry, context: &egui::Context) {
        let root = self.root.clone();
        self.start(context, move || {
            let mut skin = Skin::decode(&read_png(&managed_path(&root, &entry.id)?)?)?;
            skin.set_model(entry.model);
            Ok(Loaded { entry, skin })
        });
    }

    pub fn clear(&mut self) {
        self.active = None;
        self.error = None;
    }

    fn start(
        &mut self,
        context: &egui::Context,
        work: impl FnOnce() -> Result<Loaded, String> + Send + 'static,
    ) {
        if self.busy() {
            return;
        }
        self.error = None;
        let (sender, receiver) = mpsc::channel();
        let context = context.clone();
        match std::thread::Builder::new()
            .name("skin-import".into())
            .spawn(move || {
                let _ = sender.send(work());
                context.request_repaint();
            }) {
            Ok(_) => self.pending = Some(receiver),
            Err(error) => self.error = Some(format!("Cannot load skin: {error}")),
        }
    }

    pub fn poll(&mut self) -> Option<Entry> {
        let result = match self.pending.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("Skin loading stopped unexpectedly".into())
            }
        };
        self.pending = None;
        match result {
            Ok(loaded) => {
                self.active = Some(loaded.skin);
                Some(loaded.entry)
            }
            Err(error) => {
                self.error = Some(error);
                None
            }
        }
    }
}

fn managed_path(root: &Path, id: &str) -> Result<PathBuf, String> {
    if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Invalid saved skin identifier".into());
    }
    Ok(root.join(format!("{id}.png")))
}

fn read_png(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(MAX_PNG_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_PNG_BYTES {
        return Err("Skin PNG must be smaller than 1 MiB".into());
    }
    Ok(bytes)
}

fn import_file(root: &Path, source: &Path) -> Result<Loaded, String> {
    let bytes = read_png(source)?;
    let skin = Skin::decode(&bytes)?;
    let id = format!("{:x}", Sha256::digest(&bytes));
    let path = managed_path(root, &id)?;
    fs::create_dir_all(root).map_err(|error| format!("Cannot create skin directory: {error}"))?;
    if read_png(&path).ok().as_deref() != Some(bytes.as_slice()) {
        let write = || -> std::io::Result<()> {
            let mut temporary = tempfile::NamedTempFile::new_in(root)?;
            temporary.write_all(&bytes)?;
            temporary.as_file().sync_all()?;
            temporary.persist(&path).map_err(|error| error.error)?;
            Ok(())
        };
        write().map_err(|error| format!("Cannot save imported skin: {error}"))?;
    }
    let name = source
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    Ok(Loaded {
        entry: Entry {
            id,
            name,
            model: None,
        },
        skin,
    })
}

#[cfg(test)]
mod tests;
