//! Derived SOFA inventory; no renderer or database connection enters this worker.
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::JoinHandle;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    #[serde(with = "crate::playlist::native_path")]
    pub path: PathBuf,
    pub sha256: Option<String>,
    pub status: String,
}

pub struct Catalog {
    pub root: PathBuf,
    pub files: Vec<Entry>,
    pub message: String,
    pub started: bool,
    worker: Option<JoinHandle<()>>,
    receiver: mpsc::Receiver<Result<(Vec<Entry>, Option<PathBuf>)>>,
    sender: mpsc::Sender<Result<(Vec<Entry>, Option<PathBuf>)>>,
    cancelled: Arc<AtomicBool>,
}
impl Catalog {
    pub fn new(root: PathBuf) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            root,
            files: Vec::new(),
            message: String::new(),
            started: false,
            worker: None,
            receiver,
            sender,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn busy(&self) -> bool {
        self.worker.is_some()
    }
    pub fn refresh(&mut self, import: Option<PathBuf>, context: &eframe::egui::Context) {
        if self.busy() {
            return;
        }
        self.started = true;
        self.message = "Scanning / importing SOFA…".into();
        let (root, previous, sender, cancelled, context) = (
            self.root.clone(),
            self.files.clone(),
            self.sender.clone(),
            self.cancelled.clone(),
            context.clone(),
        );
        match std::thread::Builder::new()
            .name("sofa-catalog".into())
            .spawn(move || {
                let result = (|| {
                    let imported = import
                        .map(|source| import_file(&root, &source, &cancelled))
                        .transpose()?;
                    Ok((scan(&root, previous, &cancelled)?, imported))
                })();
                let _ = sender.send(result);
                context.request_repaint();
            }) {
            Ok(worker) => self.worker = Some(worker),
            Err(error) => self.message = error.to_string(),
        }
    }
    pub fn poll(&mut self) -> Option<(Vec<Entry>, Option<PathBuf>)> {
        let result = self.receiver.try_recv().ok()?;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        match result {
            Ok((entries, imported)) => {
                self.files.clone_from(&entries);
                self.message = format!(
                    "{} SOFA entries; format validation occurs when loading audio",
                    entries.len()
                );
                Some((entries, imported))
            }
            Err(error) => {
                self.message = error.to_string();
                None
            }
        }
    }
    pub fn shutdown(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Drop for Catalog {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn check(cancelled: &AtomicBool) -> Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        return Err("SOFA operation cancelled".into());
    }
    Ok(())
}
fn copy_hash(
    input: &mut impl Read,
    output: &mut impl Write,
    cancelled: &AtomicBool,
) -> Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        check(cancelled)?;
        let size = input.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        output.write_all(&buffer[..size])?;
        hash.update(&buffer[..size]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn fingerprint(path: &Path, cancelled: &AtomicBool) -> Result<String> {
    copy_hash(&mut File::open(path)?, &mut std::io::sink(), cancelled)
}
fn scan(root: &Path, old: Vec<Entry>, cancelled: &AtomicBool) -> Result<Vec<Entry>> {
    if !fs::symlink_metadata(root)?.is_dir() {
        return Err("SOFA root must be a directory, not a symlink".into());
    }
    let mut files: BTreeMap<_, _> = old
        .into_iter()
        .map(|mut entry| {
            entry.status = "missing".into();
            (entry.path.clone(), entry)
        })
        .collect();
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)? {
            check(cancelled)?;
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                directories.push(entry.path());
            }
            if !kind.is_file()
                || !entry
                    .path()
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("sofa"))
            {
                continue;
            }
            let relative = entry.path().strip_prefix(root)?.to_path_buf();
            let hash = fingerprint(&entry.path(), cancelled).ok();
            let status = if hash.is_some() {
                "unverified"
            } else {
                "unreadable"
            };
            files.insert(
                relative.clone(),
                Entry {
                    path: relative,
                    sha256: hash,
                    status: status.into(),
                },
            );
        }
    }
    check(cancelled)?;
    Ok(files.into_values().collect())
}
fn import_file(root: &Path, source: &Path, cancelled: &AtomicBool) -> Result<PathBuf> {
    if !fs::symlink_metadata(root)?.is_dir() || !fs::symlink_metadata(source)?.is_file() {
        return Err("Select a regular SOFA file and directory".into());
    }
    let name = source.file_name().ok_or("Missing file name")?;
    if !source
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("sofa"))
    {
        return Err("Select a .sofa file".into());
    }
    let mut staged = tempfile::Builder::new()
        .prefix(".sofa-import-")
        .suffix(".tmp")
        .tempfile_in(root)?;
    let hash = copy_hash(&mut File::open(source)?, &mut staged, cancelled)?;
    staged.as_file().sync_all()?;
    let mut target = root.join(name);
    for index in 0_u32.. {
        check(cancelled)?;
        if let Ok(metadata) = fs::symlink_metadata(&target) {
            if metadata.is_file()
                && fingerprint(&target, cancelled).is_ok_and(|existing| existing == hash)
            {
                return Ok(target);
            }
        } else {
            match staged.persist_noclobber(&target) {
                Ok(_) => return Ok(target),
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    staged = error.file;
                }
                Err(error) => return Err(error.error.into()),
            }
        }
        let mut next = source.file_stem().unwrap_or(name).to_os_string();
        next.push(format!("-{}-{index}.sofa", &hash[..12]));
        target = root.join(next);
    }
    Err("Too many file name collisions".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn import_preserves_conflicts_and_index_marks_missing() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("SOFA 文件");
        fs::create_dir(&root).unwrap();
        let source = directory.path().join("个人.sofa");
        fs::write(&source, b"first").unwrap();
        let cancelled = AtomicBool::new(false);
        let first = import_file(&root, &source, &cancelled).unwrap();
        assert_eq!(first, import_file(&root, &source, &cancelled).unwrap());
        fs::write(&source, b"second").unwrap();
        assert_ne!(first, import_file(&root, &source, &cancelled).unwrap());
        assert_eq!(fs::read(&first).unwrap(), b"first");
        let files = scan(&root, Vec::new(), &cancelled).unwrap();
        assert_eq!(files.len(), 2);
        fs::remove_file(first).unwrap();
        assert!(
            scan(&root, files, &cancelled)
                .unwrap()
                .iter()
                .any(|file| file.status == "missing")
        );
        cancelled.store(true, Ordering::Relaxed);
        assert!(import_file(&root, &source, &cancelled).is_err());
        assert!(fs::read_dir(&root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".sofa-import-")
        }));
    }
}
