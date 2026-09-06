use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

const SUPPORTED_EXTENSIONS: [&str; 3] = ["m4a", "mp4", "ac4"];

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SelectedSource {
    #[serde(with = "crate::playlist::native_path")]
    path: PathBuf,
    display_name: String,
}

impl SelectedSource {
    pub fn from_path(path: PathBuf) -> Result<Self, SourceSelectionError> {
        if !supports_path(&path) {
            return Err(SourceSelectionError::UnsupportedExtension);
        }
        let display_name = path
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or(SourceSelectionError::MissingFileName)?
            .to_string_lossy()
            .into_owned();
        Ok(Self { path, display_name })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceSelectionError {
    MissingFileName,
    UnsupportedExtension,
}

impl fmt::Display for SourceSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingFileName => formatter.write_str("The selected path has no file name"),
            Self::UnsupportedExtension => formatter.write_str("Select an .m4a, .mp4, or .ac4 file"),
        }
    }
}

fn supports_path(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            SUPPORTED_EXTENSIONS
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_supported_container_extensions_without_opening_the_file() {
        for path in ["music.m4a", "movie.MP4", "stream.Ac4"] {
            assert!(SelectedSource::from_path(PathBuf::from(path)).is_ok());
        }
    }

    #[test]
    fn rejects_unrelated_or_missing_extensions() {
        for path in ["music.wav", "movie.mkv", "stream"] {
            assert_eq!(
                SelectedSource::from_path(PathBuf::from(path)),
                Err(SourceSelectionError::UnsupportedExtension)
            );
        }
    }
}
