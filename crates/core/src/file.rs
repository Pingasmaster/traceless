use std::path::{Path, PathBuf};

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
    #[must_use]
    pub const fn simple_state(&self) -> &'static str {
        match self {
            Self::Initializing | Self::Supported | Self::CheckingMetadata | Self::RemovingMetadata => "working",
            Self::Unsupported | Self::ErrorWhileInitializing
            | Self::ErrorWhileCheckingMetadata | Self::ErrorWhileRemovingMetadata => "error",
            Self::HasNoMetadata => "warning",
            Self::HasMetadata => "has-metadata",
            Self::Cleaned => "clean",
        }
    }

    #[must_use]
    pub const fn is_cleanable(&self) -> bool {
        matches!(self, Self::HasMetadata | Self::HasNoMetadata)
    }

    #[must_use]
    pub const fn is_working(&self) -> bool {
        matches!(
            self,
            Self::Initializing | Self::Supported | Self::CheckingMetadata | Self::RemovingMetadata
        )
    }
}

/// Stable, monotonically-increasing identifier assigned to a `FileEntry`.
///
/// Workers refer to rows by `FileId` across the channel boundary so that
/// in-flight events remain correctly routed after user-driven `remove_file`
/// or `clear` calls mutate the underlying `Vec<FileEntry>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(pub u64);

/// Represents a file being processed by the application.
#[derive(Debug)]
pub struct FileEntry {
    pub id: FileId,
    pub path: PathBuf,
    pub filename: String,
    pub directory: String,
    pub mime_type: String,
    pub state: FileState,
    pub metadata: Option<MetadataSet>,
    pub error: Option<String>,
}

impl FileEntry {
    #[must_use]
    pub fn new(id: FileId, path: &Path) -> Self {
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let directory = simplify_dir_path(path);

        let mime_type = mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string();

        Self {
            id,
            path: path.to_path_buf(),
            filename,
            directory,
            mime_type,
            state: FileState::Initializing,
            metadata: None,
            error: None,
        }
    }

    #[must_use]
    pub fn total_metadata(&self) -> usize {
        self.metadata.as_ref().map_or(0, MetadataSet::total_count)
    }
}

/// Simplify a directory path for display: replace home dir with `~`.
///
/// Uses `Path::strip_prefix` rather than string `strip_prefix` so that
/// a directory like `/home/alice-backups/project` with `$HOME =
/// /home/alice` does *not* come out as `~-backups/project`. The stdlib
/// check operates on whole path components, so the prefix only matches
/// at a component boundary.
fn simplify_dir_path(path: &Path) -> String {
    simplify_dir_path_with_home(path, dirs_home().as_deref())
}

/// Same as `simplify_dir_path` but with an explicit home path override.
/// Factored out for the unit tests, which must not mutate the global
/// `$HOME` environment variable (the core crate forbids `unsafe`, and
/// `env::set_var` is `unsafe` on edition 2024).
fn simplify_dir_path_with_home(path: &Path, home: Option<&Path>) -> String {
    let Some(dir) = path.parent() else {
        return String::new();
    };
    if let Some(home) = home
        && let Ok(rest) = dir.strip_prefix(home)
    {
        if rest.as_os_str().is_empty() {
            return "~".to_string();
        }
        return format!("~/{}", rest.display());
    }
    dir.to_string_lossy().into_owned()
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn simplify_dir_under_home_is_tilde_rel() {
        let got = simplify_dir_path_with_home(
            Path::new("/home/alice/docs/report.pdf"),
            Some(Path::new("/home/alice")),
        );
        assert_eq!(got, "~/docs");
    }

    #[test]
    fn simplify_dir_exactly_home_is_tilde() {
        let got = simplify_dir_path_with_home(
            Path::new("/home/alice/report.pdf"),
            Some(Path::new("/home/alice")),
        );
        assert_eq!(got, "~");
    }

    #[test]
    fn simplify_dir_lookalike_not_collapsed() {
        // Regression: a directory that happens to start with the home
        // bytes but isn't a subdirectory must not be collapsed into `~`.
        // Before the fix, `/home/alice-backups/project/report.pdf`
        // collapsed to `~-backups/project`.
        let got = simplify_dir_path_with_home(
            Path::new("/home/alice-backups/project/report.pdf"),
            Some(Path::new("/home/alice")),
        );
        assert_eq!(got, "/home/alice-backups/project");
    }

    #[test]
    fn simplify_dir_outside_home_passes_through() {
        let got = simplify_dir_path_with_home(
            Path::new("/var/log/messages"),
            Some(Path::new("/home/alice")),
        );
        assert_eq!(got, "/var/log");
    }

    #[test]
    fn simplify_dir_no_home_returns_raw() {
        // `$HOME` unset → fall back to the raw parent path.
        let got = simplify_dir_path_with_home(Path::new("/var/log/messages"), None);
        assert_eq!(got, "/var/log");
    }
}
