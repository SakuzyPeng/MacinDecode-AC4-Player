//! Request-scoped open file and shared MP4 metadata. Payload is read on demand.
//! Each worker has a small independent cursor/buffer; only actual file IO shares
//! a lock. Neither opening nor reading runs on the GUI or audio callback thread.

use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use macindecode_ac4_mp4::reader::{MAX_METADATA_BYTES, Mp4MetadataBytes, read_mp4_metadata};

pub const READER_BUFFER_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone)]
pub struct MediaSource(Arc<SourceInner>);

#[derive(Debug)]
struct SourceInner {
    path: PathBuf,
    opened: OnceLock<Result<Arc<OpenedMedia>, String>>,
}

impl MediaSource {
    pub(crate) fn cached_open_failed(&self) -> bool {
        matches!(self.0.opened.get(), Some(Err(_)))
    }
    /// Read an already-opened file's identity without doing IO on the UI thread.
    pub(crate) fn cached_stamp(&self) -> Option<FileStamp> {
        self.0
            .opened
            .get()?
            .as_ref()
            .ok()
            .map(|opened| opened.stamp.clone())
    }
    #[must_use]
    pub fn new(path: &Path) -> Self {
        Self(Arc::new(SourceInner {
            path: path.to_path_buf(),
            opened: OnceLock::new(),
        }))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0.path
    }

    pub fn open(&self) -> Result<Arc<OpenedMedia>, String> {
        self.0
            .opened
            .get_or_init(|| {
                OpenedMedia::open(self.path())
                    .map(Arc::new)
                    .map_err(|error| {
                        format!(
                            "Failed to open AC-4 media {}: {error}",
                            self.path().display()
                        )
                    })
            })
            .clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct FileStamp {
    length: u64,
    modified: SystemTime,
}

impl FileStamp {
    fn from_metadata(metadata: &Metadata) -> io::Result<Self> {
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
        })
    }
}

#[derive(Debug)]
pub struct OpenedMedia {
    file: Mutex<File>,
    stamp: FileStamp,
    raw: bool,
    metadata: OnceLock<Result<Arc<Mp4MetadataBytes>, String>>,
}

impl OpenedMedia {
    fn open(path: &Path) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Keep rename/delete possible while preventing in-place writers
            // from replacing bytes underneath this playback request.
            options.share_mode(0x0000_0001 | 0x0000_0004);
        }
        let mut file = options.open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::other("Media input must be a regular file"));
        }
        let stamp = FileStamp::from_metadata(&metadata)?;
        if stamp.length == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Media file is empty",
            ));
        }
        let mut prefix = [0u8; 2];
        let count = usize::try_from(stamp.length.min(2)).expect("two-byte prefix");
        file.read_exact(&mut prefix[..count])?;
        let opened = Self {
            file: Mutex::new(file),
            stamp,
            raw: count == 2 && matches!(prefix, [0xac, 0x40 | 0x41]),
            metadata: OnceLock::new(),
        };
        opened.check_stamp(
            &opened
                .file
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )?;
        Ok(opened)
    }

    #[must_use]
    pub const fn is_raw(&self) -> bool {
        self.raw
    }

    #[must_use]
    pub const fn file_len(&self) -> u64 {
        self.stamp.length
    }

    #[must_use]
    pub fn reader(self: &Arc<Self>) -> BufReader<MediaCursor> {
        BufReader::with_capacity(
            READER_BUFFER_BYTES,
            MediaCursor {
                source: Arc::clone(self),
                position: 0,
            },
        )
    }

    pub fn mp4_metadata(self: &Arc<Self>) -> Result<Arc<Mp4MetadataBytes>, String> {
        self.metadata
            .get_or_init(|| {
                read_mp4_metadata(&mut self.reader(), MAX_METADATA_BYTES)
                    .map(Arc::new)
                    .map_err(|error| error.to_string())
            })
            .clone()
    }

    fn check_stamp(&self, file: &File) -> io::Result<()> {
        if FileStamp::from_metadata(&file.metadata()?)? != self.stamp {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "The media file changed on disk; remove it and add it again to reload",
            ));
        }
        Ok(())
    }

    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        let count = usize::try_from(self.file_len().saturating_sub(offset))
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        if buffer.is_empty() {
            return Ok(0);
        }
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_stamp(&file)?;
        if count == 0 {
            return Ok(0);
        }
        file.seek(SeekFrom::Start(offset))?;
        let result = file.read_exact(&mut buffer[..count]);
        self.check_stamp(&file)?;
        result?;
        Ok(count)
    }
}

#[derive(Debug)]
pub struct MediaCursor {
    source: Arc<OpenedMedia>,
    position: u64,
}

impl Read for MediaCursor {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.source.read_at(self.position, buffer)?;
        self.position = self.position.saturating_add(count as u64);
        Ok(count)
    }
}

impl Seek for MediaCursor {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.position = match from {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
            SeekFrom::End(offset) => self.source.file_len().checked_add_signed(offset),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Media seek offset overflow"))?;
        Ok(self.position)
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
                "ac4-file-source-{}-{}",
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
    fn readers_share_an_open_file_but_have_independent_positions() {
        let file = TestFile::new();
        let source = MediaSource::new(&file.0);
        let barrier = Arc::new(Barrier::new(2));
        let worker_source = source.clone();
        let worker_barrier = Arc::clone(&barrier);
        let worker = std::thread::spawn(move || {
            worker_barrier.wait();
            worker_source.open().unwrap()
        });
        barrier.wait();
        let first = source.open().unwrap();
        let second = worker.join().unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        let mut a = first.reader();
        let mut b = second.reader();
        a.seek(SeekFrom::Start(9)).unwrap();
        let mut a_text = String::new();
        let mut b_text = String::new();
        a.read_to_string(&mut a_text).unwrap();
        b.read_to_string(&mut b_text).unwrap();
        assert_eq!(a_text, "snapshot");
        assert_eq!(b_text, "original snapshot");
        let handle = Arc::downgrade(&first);
        drop(a);
        drop(b);
        drop(first);
        drop(second);
        drop(source);
        assert!(handle.upgrade().is_none());
    }

    #[test]
    fn renaming_the_path_does_not_change_the_open_file() {
        let file = TestFile::new();
        let source = MediaSource::new(&file.0);
        let opened = source.open().unwrap();
        let renamed = file.0.with_extension("renamed");
        std::fs::rename(&file.0, &renamed).unwrap();
        std::fs::write(&file.0, b"replacement").unwrap();
        let mut old = String::new();
        opened.reader().read_to_string(&mut old).unwrap();
        let mut new = String::new();
        MediaSource::new(&file.0)
            .open()
            .unwrap()
            .reader()
            .read_to_string(&mut new)
            .unwrap();
        assert_eq!(old, "original snapshot");
        assert_eq!(new, "replacement");
        drop(opened);
        drop(source);
        std::fs::remove_file(renamed).unwrap();
    }

    #[test]
    fn a_removed_path_remains_readable_through_the_open_handle() {
        let file = TestFile::new();
        let opened = MediaSource::new(&file.0).open().unwrap();
        std::fs::remove_file(&file.0).unwrap();
        let mut text = String::new();
        opened.reader().read_to_string(&mut text).unwrap();
        assert_eq!(text, "original snapshot");
    }

    #[test]
    fn in_place_changes_are_rejected_instead_of_mixing_file_versions() {
        let file = TestFile::new();
        let opened = MediaSource::new(&file.0).open().unwrap();
        let write = std::fs::write(&file.0, b"changed");
        #[cfg(target_os = "windows")]
        assert!(write.is_err(), "the open handle denies concurrent writers");
        #[cfg(not(target_os = "windows"))]
        {
            write.unwrap();
            let mut buffer = [0; 2];
            let error = opened.reader().read_exact(&mut buffer).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().contains("changed on disk"));
        }
        drop(opened);
    }
}
