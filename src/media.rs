//! A request-scoped, immutable file snapshot shared by inspection and decoding.
//! Only worker threads call `read`; the first reader performs IO and the other
//! shares its allocation as soon as the read completes, without waiting for parsing.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

#[derive(Debug, Clone)]
pub struct MediaSource(Arc<SourceInner>);

#[derive(Debug)]
struct SourceInner {
    path: PathBuf,
    bytes: OnceLock<Result<Arc<Vec<u8>>, String>>,
}

impl MediaSource {
    #[must_use]
    pub fn new(path: &Path) -> Self {
        Self(Arc::new(SourceInner {
            path: path.to_path_buf(),
            bytes: OnceLock::new(),
        }))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0.path
    }

    pub fn read(&self) -> Result<Arc<Vec<u8>>, String> {
        self.0
            .bytes
            .get_or_init(|| {
                std::fs::read(self.path()).map(Arc::new).map_err(|error| {
                    format!(
                        "Failed to read AC-4 media {}: {error}",
                        self.path().display()
                    )
                })
            })
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Barrier,
        atomic::{AtomicU64, Ordering},
    };

    struct TestFile(PathBuf);
    impl TestFile {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "ac4-shared-media-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::write(&path, b"original snapshot").unwrap();
            Self(path)
        }
    }
    impl Drop for TestFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn concurrent_readers_share_one_snapshot_and_release_it_with_the_request() {
        let file = TestFile::new();
        let source = MediaSource::new(&file.0);
        let barrier = Arc::new(Barrier::new(2));
        let worker_source = source.clone();
        let worker_barrier = Arc::clone(&barrier);
        let worker = std::thread::spawn(move || {
            worker_barrier.wait();
            worker_source.read().unwrap()
        });
        barrier.wait();
        let first = source.read().unwrap();
        let second = worker.join().unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        std::fs::remove_file(&file.0).unwrap();
        assert!(Arc::ptr_eq(&first, &source.read().unwrap()));
        let allocation = Arc::downgrade(&first);
        drop(first);
        drop(second);
        drop(source);
        assert!(allocation.upgrade().is_none());
    }

    #[test]
    fn a_new_selection_observes_disk_changes_without_changing_the_old_snapshot() {
        let file = TestFile::new();
        let source = MediaSource::new(&file.0);
        assert_eq!(source.read().unwrap().as_slice(), b"original snapshot");
        std::fs::write(&file.0, b"replacement").unwrap();
        let next = MediaSource::new(&file.0);
        assert_eq!(next.read().unwrap().as_slice(), b"replacement");
        assert_eq!(source.read().unwrap().as_slice(), b"original snapshot");
    }
}
