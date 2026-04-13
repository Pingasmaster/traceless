use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use async_channel::Sender;

use crate::file::{FileEntry, FileId, FileState};
use crate::format_support::{detect_mime, get_handler_for_mime};

static NEXT_FILE_ID: AtomicU64 = AtomicU64::new(1);

fn next_file_id() -> FileId {
    FileId(NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed))
}

/// Events sent from background worker threads to the UI.
///
/// Events carry a stable `FileId` rather than a positional index so that
/// in-flight events remain correctly routed after `remove_file` /
/// `clear` mutate the underlying `Vec<FileEntry>`.
#[derive(Debug)]
pub enum FileStoreEvent {
    /// A file's lifecycle state changed.
    FileStateChanged {
        id: FileId,
        state: FileState,
        mime_type: Option<String>,
    },
    /// Metadata was read for the file with the given id.
    MetadataReady {
        id: FileId,
        metadata: crate::metadata::MetadataSet,
    },
    /// An error occurred for the file with the given id.
    FileError {
        id: FileId,
        state: FileState,
        message: String,
    },
}

/// Owns the list of files and orchestrates background processing.
pub struct FileStore {
    files: Vec<FileEntry>,
}

impl FileStore {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            files: Vec::new(),
        }
    }

    #[must_use]
    pub fn files(&self) -> &[FileEntry] {
        &self.files
    }

    pub fn files_mut(&mut self) -> &mut [FileEntry] {
        &mut self.files
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.files.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    #[must_use]
    pub fn get(&self, index: usize) -> Option<&FileEntry> {
        self.files.get(index)
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut FileEntry> {
        self.files.get_mut(index)
    }

    /// Return the current row index for a stable `FileId`, if it still
    /// exists in the store. Frontends call this when applying events so
    /// that a stale event for a removed file is silently dropped.
    #[must_use]
    pub fn position_of(&self, id: FileId) -> Option<usize> {
        self.files.iter().position(|f| f.id == id)
    }

    /// Add files and start background metadata checking.
    /// Returns the indices of the newly added files.
    pub fn add_files(&mut self, paths: Vec<PathBuf>, tx: &Sender<FileStoreEvent>) -> Vec<usize> {
        let start_index = self.files.len();
        let mut indices = Vec::new();
        let mut ids = Vec::with_capacity(paths.len());

        for path in &paths {
            let id = next_file_id();
            let entry = FileEntry::new(id, path);
            self.files.push(entry);
            indices.push(self.files.len() - 1);
            ids.push(id);
        }

        // Spawn background threads to check metadata
        for (path, id) in paths.into_iter().zip(ids) {
            let tx = tx.clone();
            thread::spawn(move || {
                check_file_metadata(id, &path, &tx);
            });
        }

        let _ = start_index;
        indices
    }

    /// Add all files from a directory (optionally recursive).
    pub fn add_directory(
        &mut self,
        dir: &Path,
        recursive: bool,
        tx: &Sender<FileStoreEvent>,
    ) -> Vec<usize> {
        let paths = collect_paths(dir, recursive);
        self.add_files(paths, tx)
    }

    /// Remove a file at the given index.
    pub fn remove_file(&mut self, index: usize) {
        if index < self.files.len() {
            self.files.remove(index);
        }
    }

    /// Clear all files.
    pub fn clear(&mut self) {
        self.files.clear();
    }

    /// Start cleaning all cleanable files in background threads.
    pub fn clean_files(&mut self, tx: &Sender<FileStoreEvent>) {
        for entry in &mut self.files {
            if entry.state.is_cleanable() {
                entry.state = FileState::RemovingMetadata;
                let path = entry.path.clone();
                let id = entry.id;
                let tx = tx.clone();
                thread::spawn(move || {
                    clean_single_file(id, &path, &tx);
                });
            }
        }
    }

    #[must_use]
    pub fn cleanable_count(&self) -> usize {
        self.files.iter().filter(|f| f.state.is_cleanable()).count()
    }

    #[must_use]
    pub fn cleaned_count(&self) -> usize {
        self.files
            .iter()
            .filter(|f| f.state == FileState::Cleaned)
            .count()
    }

    #[must_use]
    pub fn has_working(&self) -> bool {
        self.files.iter().any(|f| f.state.is_working())
    }

    /// Apply a `FileStoreEvent` to update internal state. Returns the
    /// current row index that the event affected, if the corresponding
    /// file is still in the store. Events for removed files are silently
    /// dropped.
    pub fn apply_event(&mut self, event: &FileStoreEvent) -> Option<usize> {
        match event {
            FileStoreEvent::FileStateChanged {
                id,
                state,
                mime_type,
            } => {
                let pos = self.position_of(*id)?;
                let entry = self.files.get_mut(pos)?;
                entry.state = *state;
                if let Some(mime) = mime_type {
                    entry.mime_type.clone_from(mime);
                }
                Some(pos)
            }
            FileStoreEvent::MetadataReady { id, metadata } => {
                let pos = self.position_of(*id)?;
                let entry = self.files.get_mut(pos)?;
                entry.state = if metadata.is_empty() {
                    FileState::HasNoMetadata
                } else {
                    FileState::HasMetadata
                };
                entry.metadata = Some(metadata.clone());
                Some(pos)
            }
            FileStoreEvent::FileError {
                id,
                state,
                message,
            } => {
                let pos = self.position_of(*id)?;
                let entry = self.files.get_mut(pos)?;
                entry.state = *state;
                entry.error = Some(message.clone());
                Some(pos)
            }
        }
    }
}

impl Default for FileStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Collect regular files under `dir`, recursively if asked. Symlinked
/// directories are skipped to avoid infinite recursion on cyclic links.
#[must_use]
pub fn collect_paths(dir: &Path, recursive: bool) -> Vec<PathBuf> {
    collect_files_from_dir(dir, recursive)
}

fn check_file_metadata(id: FileId, path: &Path, tx: &Sender<FileStoreEvent>) {
    let mime = detect_mime(path);

    let Some(handler) = get_handler_for_mime(&mime) else {
        let _ = tx.send_blocking(FileStoreEvent::FileError {
            id,
            state: FileState::Unsupported,
            message: format!("Unsupported format: {mime}"),
        });
        return;
    };

    // Notify: supported, now checking
    let _ = tx.send_blocking(FileStoreEvent::FileStateChanged {
        id,
        state: FileState::CheckingMetadata,
        mime_type: Some(mime),
    });

    match handler.read_metadata(path) {
        Ok(metadata) => {
            let _ = tx.send_blocking(FileStoreEvent::MetadataReady { id, metadata });
        }
        Err(e) => {
            let _ = tx.send_blocking(FileStoreEvent::FileError {
                id,
                state: FileState::ErrorWhileCheckingMetadata,
                message: e.to_string(),
            });
        }
    }
}

fn clean_single_file(
    id: FileId,
    path: &Path,
    tx: &Sender<FileStoreEvent>,
) {
    let _ = tx.send_blocking(FileStoreEvent::FileStateChanged {
        id,
        state: FileState::RemovingMetadata,
        mime_type: None,
    });

    let mime = detect_mime(path);
    let Some(handler) = get_handler_for_mime(&mime) else {
        let _ = tx.send_blocking(FileStoreEvent::FileError {
            id,
            state: FileState::ErrorWhileRemovingMetadata,
            message: format!("No handler for {mime}"),
        });
        return;
    };

    // Write cleaned file to a temp path, then atomically replace the original.
    // Keep the original extension on the temp file: several backends
    // (little_exif, lofty, ffmpeg) dispatch on extension, so overwriting it
    // here would make them refuse the file.
    let temp_path = make_temp_path(path);

    match handler.clean_metadata(path, &temp_path) {
        Ok(()) => {
            if let Err(e) = fs::rename(&temp_path, path) {
                let _ = fs::remove_file(&temp_path);
                let _ = tx.send_blocking(FileStoreEvent::FileError {
                    id,
                    state: FileState::ErrorWhileRemovingMetadata,
                    message: format!("Failed to replace original file: {e}"),
                });
                return;
            }
            let _ = tx.send_blocking(FileStoreEvent::FileStateChanged {
                id,
                state: FileState::Cleaned,
                mime_type: None,
            });
        }
        Err(e) => {
            let _ = fs::remove_file(&temp_path);
            let _ = tx.send_blocking(FileStoreEvent::FileError {
                id,
                state: FileState::ErrorWhileRemovingMetadata,
                message: e.to_string(),
            });
        }
    }
}

/// Build a sibling temp-file path that keeps the original extension so
/// extension-sniffing libraries (`little_exif`, `lofty`, `ffmpeg`) still
/// recognise the format. `photo.jpg` → `.photo.traceless-tmp.jpg`.
fn make_temp_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = if ext.is_empty() {
        format!(".{stem}.traceless-tmp")
    } else {
        format!(".{stem}.traceless-tmp.{ext}")
    };
    path.with_file_name(name)
}

fn collect_files_from_dir(dir: &Path, recursive: bool) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return result;
    };

    for entry in entries.flatten() {
        // `DirEntry::file_type` does *not* follow symlinks; this skips
        // symlinked directories up-front and prevents infinite recursion
        // on pathological trees (`~/loop -> ~/`).
        let Ok(file_type) = entry.file_type() else { continue };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_file() {
            result.push(path);
        } else if file_type.is_dir() && recursive {
            result.extend(collect_files_from_dir(&path, true));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::make_temp_path;
    use std::path::Path;

    #[test]
    fn make_temp_path_preserves_extension() {
        let p = make_temp_path(Path::new("/tmp/photo.jpg"));
        assert_eq!(p, Path::new("/tmp/.photo.traceless-tmp.jpg"));
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("jpg"));
    }

    #[test]
    fn make_temp_path_no_extension() {
        let p = make_temp_path(Path::new("/tmp/README"));
        assert_eq!(p, Path::new("/tmp/.README.traceless-tmp"));
    }

    #[test]
    fn make_temp_path_dotted_stem() {
        let p = make_temp_path(Path::new("/srv/archive.tar.gz"));
        // `with_file_name` only looks at the last component, and `file_stem`
        // strips the final dot-component, so the helper keeps the final `.gz`.
        assert_eq!(p, Path::new("/srv/.archive.tar.traceless-tmp.gz"));
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("gz"));
    }
}
