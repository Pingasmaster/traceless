use std::path::{Path, PathBuf};

use crate::error::CoreError;
use crate::metadata::MetadataSet;

/// States that a file entry can be in during its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    Initializing,
    ErrorWhileInitializing,
    Unsupported,
    Supported,
    CheckingMetadata,
    ErrorWhileCheckingMetadata,
    HasNoMetadata,
    HasMetadata,
    RemovingMetadata,
    ErrorWhileRemovingMetadata,
    Cleaned,
}

impl FileState {
    /// Map to a simplified state string for UI display logic.
    pub fn simple_state(&self) -> &'static str {
        match self {
            Self::Initializing | Self::Supported | Self::CheckingMetadata | Self::RemovingMetadata => "working",
            Self::Unsupported | Self::ErrorWhileInitializing
            | Self::ErrorWhileCheckingMetadata | Self::ErrorWhileRemovingMetadata => "error",
            Self::HasNoMetadata => "warning",
            Self::HasMetadata => "has-metadata",
            Self::Cleaned => "clean",
        }
    }

    pub fn is_cleanable(&self) -> bool {
        matches!(self, Self::HasMetadata | Self::HasNoMetadata)
    }

    pub fn is_working(&self) -> bool {
        matches!(
            self,
            Self::Initializing | Self::Supported | Self::CheckingMetadata | Self::RemovingMetadata
        )
    }
}

/// Represents a file being processed by the application.
#[derive(Debug)]
pub struct FileEntry {
    pub path: PathBuf,
    pub filename: String,
    pub directory: String,
    pub mime_type: String,
    pub state: FileState,
    pub metadata: Option<MetadataSet>,
    pub error: Option<String>,
}

impl FileEntry {
    pub fn new(path: &Path) -> Self {
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let directory = simplify_dir_path(path);

        let mime_type = mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string();

        Self {
            path: path.to_path_buf(),
            filename,
            directory,
            mime_type,
            state: FileState::Initializing,
            metadata: None,
            error: None,
        }
    }

    pub fn total_metadata(&self) -> usize {
        self.metadata.as_ref().map_or(0, |m| m.total_count())
    }

    pub fn set_error(&mut self, state: FileState, err: CoreError) {
        self.error = Some(err.to_string());
        self.state = state;
    }
}

/// Simplify a directory path for display: replace home dir with ~.
fn simplify_dir_path(path: &Path) -> String {
    let dir = path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    if let Some(home) = dirs_home() && let Some(rest) = dir.strip_prefix(&home) {
        return format!("~{rest}");
    }
    dir
}

fn dirs_home() -> Option<String> {
    std::env::var("HOME").ok()
}
