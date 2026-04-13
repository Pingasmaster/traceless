use std::fs;
use std::path::{Path, PathBuf};
use std::thread;

use async_channel::Sender;

use crate::file::{FileEntry, FileState};
use crate::format_support::{detect_mime, get_handler_for_mime};

/// Events sent from background worker threads to the UI.
#[derive(Debug)]
pub enum FileStoreEvent {
    /// A file was added and initialized with the given index.
    FileStateChanged {
        index: usize,
        state: FileState,
        mime_type: Option<String>,
    },
    /// Metadata was read for the file at the given index.
    MetadataReady {
        index: usize,
        metadata: crate::metadata::MetadataSet,
    },
    /// An error occurred for the file at the given index.
    FileError {
        index: usize,
        state: FileState,
        message: String,
    },
    /// All current operations are done.
    AllDone,
}

/// Owns the list of files and orchestrates background processing.
pub struct FileStore {
    files: Vec<FileEntry>,
    pub lightweight_mode: bool,
}

impl FileStore {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            files: Vec::new(),
            lightweight_mode: false,
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

    /// Add files and start background metadata checking.
    /// Returns the indices of the newly added files.
    pub fn add_files(&mut self, paths: Vec<PathBuf>, tx: &Sender<FileStoreEvent>) -> Vec<usize> {
        let start_index = self.files.len();
        let mut indices = Vec::new();

        for path in &paths {
            let entry = FileEntry::new(path);
            self.files.push(entry);
            indices.push(self.files.len() - 1);
        }

        // Spawn background threads to check metadata
        for (i, path) in paths.into_iter().enumerate() {
            let index = start_index + i;
            let tx = tx.clone();
            thread::spawn(move || {
                check_file_metadata(index, &path, &tx);
            });
        }

        indices
    }

    /// Add all files from a directory (optionally recursive).
    pub fn add_directory(
        &mut self,
        dir: &Path,
        recursive: bool,
        tx: &Sender<FileStoreEvent>,
    ) -> Vec<usize> {
        let paths = collect_files_from_dir(dir, recursive);
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
        let lightweight = self.lightweight_mode;

        for (index, entry) in self.files.iter_mut().enumerate() {
            if entry.state.is_cleanable() {
                entry.state = FileState::RemovingMetadata;
                let path = entry.path.clone();
                let tx = tx.clone();
                thread::spawn(move || {
                    clean_single_file(index, &path, lightweight, &tx);
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

    /// Apply a `FileStoreEvent` to update internal state.
    pub fn apply_event(&mut self, event: &FileStoreEvent) {
        match event {
            FileStoreEvent::FileStateChanged {
                index,
                state,
                mime_type,
            } => {
                if let Some(entry) = self.files.get_mut(*index) {
                    entry.state = *state;
                    if let Some(mime) = mime_type {
                        entry.mime_type.clone_from(mime);
                    }
                }
            }
            FileStoreEvent::MetadataReady { index, metadata } => {
                if let Some(entry) = self.files.get_mut(*index) {
                    if metadata.is_empty() {
                        entry.state = FileState::HasNoMetadata;
                    } else {
                        entry.state = FileState::HasMetadata;
                    }
                    entry.metadata = Some(metadata.clone());
                }
            }
            FileStoreEvent::FileError {
                index,
                state,
                message,
            } => {
                if let Some(entry) = self.files.get_mut(*index) {
                    entry.state = *state;
                    entry.error = Some(message.clone());
                }
            }
            FileStoreEvent::AllDone => {}
        }
    }
}

impl Default for FileStore {
    fn default() -> Self {
        Self::new()
    }
}

fn check_file_metadata(index: usize, path: &Path, tx: &Sender<FileStoreEvent>) {
    let mime = detect_mime(path);

    let Some(handler) = get_handler_for_mime(&mime) else {
        let _ = tx.send_blocking(FileStoreEvent::FileError {
            index,
            state: FileState::Unsupported,
            message: format!("Unsupported format: {mime}"),
        });
        return;
    };

    // Notify: supported, now checking
    let _ = tx.send_blocking(FileStoreEvent::FileStateChanged {
        index,
        state: FileState::CheckingMetadata,
        mime_type: Some(mime),
    });

    match handler.read_metadata(path) {
        Ok(metadata) => {
            let _ = tx.send_blocking(FileStoreEvent::MetadataReady { index, metadata });
        }
        Err(e) => {
            let _ = tx.send_blocking(FileStoreEvent::FileError {
                index,
                state: FileState::ErrorWhileCheckingMetadata,
                message: e.to_string(),
            });
        }
    }
}

fn clean_single_file(
    index: usize,
    path: &Path,
    lightweight: bool,
    tx: &Sender<FileStoreEvent>,
) {
    let _ = tx.send_blocking(FileStoreEvent::FileStateChanged {
        index,
        state: FileState::RemovingMetadata,
        mime_type: None,
    });

    let mime = detect_mime(path);
    let Some(handler) = get_handler_for_mime(&mime) else {
        let _ = tx.send_blocking(FileStoreEvent::FileError {
            index,
            state: FileState::ErrorWhileRemovingMetadata,
            message: format!("No handler for {mime}"),
        });
        return;
    };

    // Write cleaned file to a temp path, then atomically replace the original
    let temp_path = path.with_extension("traceless.tmp");

    match handler.clean_metadata(path, &temp_path, lightweight) {
        Ok(()) => {
            if let Err(e) = fs::rename(&temp_path, path) {
                let _ = fs::remove_file(&temp_path);
                let _ = tx.send_blocking(FileStoreEvent::FileError {
                    index,
                    state: FileState::ErrorWhileRemovingMetadata,
                    message: format!("Failed to replace original file: {e}"),
                });
                return;
            }
            let _ = tx.send_blocking(FileStoreEvent::FileStateChanged {
                index,
                state: FileState::Cleaned,
                mime_type: None,
            });
        }
        Err(e) => {
            let _ = fs::remove_file(&temp_path);
            let _ = tx.send_blocking(FileStoreEvent::FileError {
                index,
                state: FileState::ErrorWhileRemovingMetadata,
                message: e.to_string(),
            });
        }
    }
}

fn collect_files_from_dir(dir: &Path, recursive: bool) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return result;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            result.push(path);
        } else if path.is_dir() && recursive {
            result.extend(collect_files_from_dir(&path, true));
        }
    }
    result
}
