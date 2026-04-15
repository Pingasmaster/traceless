use std::any::Any;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use async_channel::Sender;

use crate::file::{FileEntry, FileId, FileState};
use crate::format_support::{detect_mime, get_handler_for_mime};
use crate::worker_pool;

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
        Self { files: Vec::new() }
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
    ///
    /// Two protective filters run before enqueuing any worker:
    ///
    /// 1. **Symlinks** are rejected up-front with a terminal
    ///    `FileError` event so the user sees why nothing happened.
    ///    mat2 refuses symlinks for the same reason: following one
    ///    can write to a target outside the trust boundary the user
    ///    thought they were cleaning. `collect_files_from_dir`
    ///    already skips symlinks during directory recursion; this
    ///    guard mirrors the same policy for paths handed in directly.
    ///
    /// 2. **Duplicates** are collapsed by canonical path. A user who
    ///    drags the same file twice (a GTK file-chooser quirk with
    ///    multi-select, or a drag that picks up the same inode via
    ///    two different relative paths) would otherwise get two
    ///    workers racing to rename onto the same destination.
    pub fn add_files(&mut self, paths: Vec<PathBuf>, tx: &Sender<FileStoreEvent>) -> Vec<usize> {
        let mut indices = Vec::new();
        let mut ids_paths: Vec<(FileId, PathBuf)> = Vec::new();

        // Canonical paths already present in the store (from any
        // previous add_files call) seed the dedup set so a second
        // drag of the same file is rejected even across batches.
        let mut seen: std::collections::HashSet<PathBuf> = self
            .files
            .iter()
            .filter_map(|f| fs::canonicalize(&f.path).ok())
            .collect();

        for path in paths {
            if let Ok(md) = fs::symlink_metadata(&path)
                && md.file_type().is_symlink()
            {
                let id = next_file_id();
                let entry = FileEntry::new(id, &path);
                self.files.push(entry);
                indices.push(self.files.len() - 1);
                let _ = tx.send_blocking(FileStoreEvent::FileError {
                    id,
                    state: FileState::ErrorWhileCheckingMetadata,
                    message: format!(
                        "refusing to process symlink {}; pass the target file directly",
                        path.display()
                    ),
                });
                continue;
            }

            let canonical = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if !seen.insert(canonical) {
                continue;
            }

            let id = next_file_id();
            let entry = FileEntry::new(id, &path);
            self.files.push(entry);
            indices.push(self.files.len() - 1);
            ids_paths.push((id, path));
        }

        // Submit per-file metadata scans to the shared worker pool.
        // Using the pool bounds concurrency at
        // `min(available_parallelism(), 8)`; the old `thread::spawn`
        // per path hit `RLIMIT_NPROC` (and panicked the caller) on
        // large batches such as a dropped photo library.
        //
        // `run_job_with_terminal_error` wraps the job body in
        // `catch_unwind` so a handler panic mid-scan still produces
        // a terminal `FileError` event, instead of leaving the file
        // stuck in `CheckingMetadata` forever.
        for (id, path) in ids_paths {
            let tx = tx.clone();
            worker_pool::submit(move || {
                run_job_with_terminal_error(id, &tx, FileState::ErrorWhileCheckingMetadata, || {
                    check_file_metadata(id, &path, &tx);
                });
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

    /// Start cleaning all cleanable files on the shared worker pool.
    pub fn clean_files(&mut self, tx: &Sender<FileStoreEvent>) {
        for entry in &mut self.files {
            if entry.state.is_cleanable() {
                entry.state = FileState::RemovingMetadata;
                let path = entry.path.clone();
                let id = entry.id;
                let tx = tx.clone();
                worker_pool::submit(move || {
                    run_job_with_terminal_error(
                        id,
                        &tx,
                        FileState::ErrorWhileRemovingMetadata,
                        || clean_single_file(id, &path, &tx),
                    );
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
            FileStoreEvent::FileError { id, state, message } => {
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

/// Decode a `catch_unwind` payload into a displayable string. The
/// standard library types `Box<dyn Any + Send>` as the payload; the
/// `&'static str` and `String` cases cover every panic that Rust's
/// own `panic!` macro produces. Anything else falls back to a
/// generic label.
///
/// Returns an owned `String` rather than a `&'static str` so the
/// `String` payload arm does not have to `Box::leak` the cloned
/// bytes. The caller immediately feeds the result into `format!`,
/// which allocates anyway, so there is no efficiency cost to owning.
fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "<non-string panic payload>".to_string()
}

/// Run a job that emits lifecycle events through `tx`, catching any
/// panic and converting it to a terminal `FileError` so the UI never
/// sees a file stuck in a working state. The inner `worker_pool`
/// `catch_unwind` is still the last-line safety net that keeps
/// workers alive if this wrapper is ever bypassed.
fn run_job_with_terminal_error<F>(
    id: FileId,
    tx: &Sender<FileStoreEvent>,
    terminal_error_state: FileState,
    job: F,
) where
    F: FnOnce(),
{
    let result = catch_unwind(AssertUnwindSafe(job));
    if let Err(payload) = result {
        let msg = panic_payload_message(payload.as_ref());
        let _ = tx.send_blocking(FileStoreEvent::FileError {
            id,
            state: terminal_error_state,
            message: format!("handler panicked: {msg}"),
        });
    }
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

fn clean_single_file(id: FileId, path: &Path, tx: &Sender<FileStoreEvent>) {
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

    // Write the cleaned file into a *private* tempdir sitting alongside
    // the original, then atomically rename it into place. The private
    // tempdir (`tempfile::Builder::tempdir_in`, mkdir'd with 0700) is
    // what prevents a local attacker from pre-creating a symlink at a
    // predictable path and redirecting the handler's writes: the old
    // scheme wrote to `.{stem}.traceless-tmp.{ext}` next to the original,
    // which any user with write access to the parent directory could
    // squat on before the clean started. Keeping the original filename
    // (inside the private dir) preserves the extension for
    // `little_exif` / `lofty` / `ffmpeg`, all of which dispatch on it.
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let tempdir = match tempfile::Builder::new()
        .prefix(".traceless-")
        .tempdir_in(parent)
    {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send_blocking(FileStoreEvent::FileError {
                id,
                state: FileState::ErrorWhileRemovingMetadata,
                message: format!("Failed to create temp directory: {e}"),
            });
            return;
        }
    };
    let file_name = path.file_name().unwrap_or_default();
    let temp_path = tempdir.path().join(file_name);

    match handler.clean_metadata(path, &temp_path) {
        Ok(()) => {
            if let Err(e) = finalize_cleaned_file(path, &temp_path) {
                // tempdir Drop removes the temp file + directory.
                let _ = tx.send_blocking(FileStoreEvent::FileError {
                    id,
                    state: FileState::ErrorWhileRemovingMetadata,
                    message: e,
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
            // tempdir Drop removes the temp file + directory.
            let _ = tx.send_blocking(FileStoreEvent::FileError {
                id,
                state: FileState::ErrorWhileRemovingMetadata,
                message: e.to_string(),
            });
        }
    }
}

/// Atomically move a freshly-cleaned temp file into the original's
/// place, first copying the original's Unix mode bits onto the temp
/// and fsync'ing the temp to durable storage.
///
/// Mode preservation: the handler writes the cleaned output via
/// `File::create` / `fs::write`, which applies `0o666 & !umask`
/// (typically `0o644`). If the original file was `0o600` (e.g. a
/// private keyring export or a restricted-share-directory PDF) the
/// rename would otherwise publish the cleaned contents at `0o644`,
/// making the metadata-free bytes world-readable. We read the
/// original's mode and re-apply it before the rename.
///
/// fsync: on power loss between the handler's last write and the
/// filesystem journal commit, a bare `rename(tmp, dst)` can leave the
/// user with a renamed-but-zero-tailed file where the original used
/// to be. A `sync_all` on the temp file before the rename makes that
/// window impossible on any POSIX-compliant filesystem.
#[cfg(unix)]
fn finalize_cleaned_file(path: &Path, temp_path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(orig_meta) = fs::metadata(path) {
        let mode = orig_meta.permissions().mode();
        let new_perms = std::fs::Permissions::from_mode(mode);
        fs::set_permissions(temp_path, new_perms)
            .map_err(|e| format!("Failed to preserve file mode bits: {e}"))?;
    }
    let temp_file = fs::File::open(temp_path)
        .map_err(|e| format!("Failed to reopen temp file for fsync: {e}"))?;
    temp_file
        .sync_all()
        .map_err(|e| format!("Failed to sync cleaned file: {e}"))?;
    drop(temp_file);
    fs::rename(temp_path, path).map_err(|e| format!("Failed to replace original file: {e}"))
}

#[cfg(not(unix))]
fn finalize_cleaned_file(path: &Path, temp_path: &Path) -> Result<(), String> {
    let temp_file = fs::File::open(temp_path)
        .map_err(|e| format!("Failed to reopen temp file for fsync: {e}"))?;
    temp_file
        .sync_all()
        .map_err(|e| format!("Failed to sync cleaned file: {e}"))?;
    drop(temp_file);
    fs::rename(temp_path, path).map_err(|e| format!("Failed to replace original file: {e}"))
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
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
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
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{
        FileId, FileState, FileStoreEvent, clean_single_file, run_job_with_terminal_error,
    };
    use async_channel::unbounded;
    use std::os::unix::fs::symlink;

    #[test]
    fn clean_single_file_ignores_preexisting_predictable_tmp_symlink() {
        // Regression: the cleaner used to write its intermediate file
        // to `.{stem}.traceless-tmp.{ext}` alongside the original. A
        // local attacker who could write the same directory could
        // pre-create that exact path as a symlink pointing at a
        // sensitive file, and the handler's `fs::write` (O_CREAT |
        // O_TRUNC, follows symlinks) would then overwrite the symlink
        // target. The fix is to write inside a private mkdir'd-0700
        // tempdir whose name is not predictable, so no pre-created
        // squat at the old path can ever be opened.

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("note.txt");
        std::fs::write(&src, b"hello world\n").unwrap();

        let honeypot = dir.path().join("honeypot.txt");
        std::fs::write(&honeypot, b"do-not-touch").unwrap();
        let old_predictable = dir.path().join(".note.traceless-tmp.txt");
        symlink(&honeypot, &old_predictable).unwrap();

        let (tx, rx) = unbounded::<FileStoreEvent>();
        clean_single_file(FileId(1), &src, &tx);
        drop(tx);
        while rx.try_recv().is_ok() {}

        // 1. The honeypot the old scheme would have clobbered is intact.
        let honeypot_content = std::fs::read(&honeypot).unwrap();
        assert_eq!(honeypot_content, b"do-not-touch");

        // 2. The symlink we pre-created also still exists unchanged.
        assert!(old_predictable.is_symlink());

        // 3. The original file round-tripped through the harmless
        //    text/plain handler (a byte-for-byte copy) and is still there.
        assert!(src.is_file());
        assert_eq!(std::fs::read(&src).unwrap(), b"hello world\n");

        // 4. No stray `.traceless-*` tempdir left behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .filter(|n| {
                let s = n.to_string_lossy();
                s.starts_with(".traceless-")
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "expected no leftover .traceless-* entries, found {leftovers:?}"
        );
    }

    #[test]
    fn clean_single_file_preserves_restrictive_mode_bits() {
        // Regression: before this fix the cleaner wrote its output via
        // `File::create` / `fs::write` (both apply `0o666 & !umask` -
        // typically `0o644`), then atomically renamed the temp file into
        // place. A user cleaning a `0o600` private file (e.g. an
        // exported password list or a diary PDF stored in a
        // group-readable parent directory) would find the cleaned
        // output published at `0o644`, quietly world-readable to any
        // local user. The fix copies the original's Unix mode onto the
        // temp file before the rename.
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("secret.txt");
        std::fs::write(&src, b"top-secret bytes\n").unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o600)).unwrap();

        let (tx, rx) = unbounded::<FileStoreEvent>();
        clean_single_file(FileId(1), &src, &tx);
        drop(tx);
        while rx.try_recv().is_ok() {}

        assert!(src.is_file(), "cleaned file should still exist");
        let mode = std::fs::metadata(&src).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "cleaned file lost its restrictive mode: {mode:o}"
        );
    }

    #[test]
    fn panic_in_job_emits_terminal_error_event() {
        // Regression: `worker_pool::submit` already catches panics so
        // the worker thread survives, but before this fix the file
        // whose job panicked got no terminal event at all - the UI
        // left the entry stuck on whatever `FileStateChanged` state
        // had been emitted before the panic. `run_job_with_terminal_error`
        // now wraps the job in its own `catch_unwind` and emits a
        // `FileError` on the unwind path so every submitted job
        // always terminates with a terminal event.
        let (tx, rx) = unbounded::<FileStoreEvent>();
        run_job_with_terminal_error(
            FileId(42),
            &tx,
            FileState::ErrorWhileRemovingMetadata,
            || {
                panic!("synthetic handler panic");
            },
        );
        drop(tx);

        let mut saw_terminal_error = false;
        while let Ok(event) = rx.try_recv() {
            if let FileStoreEvent::FileError { id, state, message } = event {
                assert_eq!(id, FileId(42));
                assert_eq!(state, FileState::ErrorWhileRemovingMetadata);
                assert!(
                    message.contains("synthetic handler panic"),
                    "error message should carry the panic payload, got: {message}"
                );
                saw_terminal_error = true;
            }
        }
        assert!(
            saw_terminal_error,
            "expected a terminal FileError event after the panicking job"
        );
    }

    #[test]
    fn successful_job_does_not_emit_spurious_error() {
        // The normal non-panic path must not emit a FileError just
        // because the wrapper is in place. The terminal events are
        // still the handler's responsibility on the happy path.
        let (tx, rx) = unbounded::<FileStoreEvent>();
        run_job_with_terminal_error(
            FileId(7),
            &tx,
            FileState::ErrorWhileCheckingMetadata,
            || {
                // Empty job. No events emitted from inside.
            },
        );
        drop(tx);

        while let Ok(event) = rx.try_recv() {
            if let FileStoreEvent::FileError { .. } = event {
                panic!("wrapper emitted a spurious error on a successful job");
            }
        }
    }

    #[test]
    fn add_files_rejects_top_level_symlink_with_terminal_error() {
        use crate::file_store::FileStore;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        std::fs::write(&target, b"hello").unwrap();
        let link = dir.path().join("link.txt");
        symlink(&target, &link).unwrap();

        let (tx, rx) = unbounded::<FileStoreEvent>();
        let mut store = FileStore::new();
        let indices = store.add_files(vec![link], &tx);

        // The symlink still gets an entry so the UI can display the
        // rejection, but the worker never runs.
        assert_eq!(indices.len(), 1, "symlink must still take a row");
        drop(tx);

        let mut saw_symlink_rejection = false;
        while let Ok(event) = rx.try_recv() {
            if let FileStoreEvent::FileError { state, message, .. } = event {
                assert_eq!(state, FileState::ErrorWhileCheckingMetadata);
                assert!(
                    message.contains("symlink"),
                    "rejection must mention symlink, got: {message}"
                );
                saw_symlink_rejection = true;
            }
        }
        assert!(
            saw_symlink_rejection,
            "expected a FileError rejecting the top-level symlink"
        );
    }

    #[test]
    fn add_files_dedups_duplicate_paths_in_same_batch() {
        use crate::file_store::FileStore;

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("note.txt");
        std::fs::write(&src, b"one\n").unwrap();

        let (tx, rx) = unbounded::<FileStoreEvent>();
        let mut store = FileStore::new();
        let indices = store.add_files(vec![src.clone(), src.clone(), src], &tx);
        drop(tx);
        while rx.try_recv().is_ok() {}

        assert_eq!(
            indices.len(),
            1,
            "the same path dragged three times must collapse to one entry"
        );
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn add_files_dedups_across_batches() {
        use crate::file_store::FileStore;

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("note.txt");
        std::fs::write(&src, b"one\n").unwrap();

        let (tx, rx) = unbounded::<FileStoreEvent>();
        let mut store = FileStore::new();
        let _ = store.add_files(vec![src.clone()], &tx);
        let second = store.add_files(vec![src], &tx);
        drop(tx);
        while rx.try_recv().is_ok() {}

        assert!(
            second.is_empty(),
            "re-adding an already-present file must produce no new rows"
        );
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn panic_with_formatted_string_payload_is_surfaced_without_leaking() {
        // Regression: `panic_payload_message` used to `Box::leak` the
        // cloned bytes of the `String` panic payload to satisfy its
        // `&'static str` return type. Every formatted `panic!("{}", x)`
        // call therefore leaked its message for the rest of the
        // process lifetime. The function now returns `String`, so the
        // payload owns its bytes and is freed at the end of the
        // `format!` that embeds it. This test also documents that the
        // `String`-payload branch still delivers the message to the
        // UI, not just the `&'static str` branch.
        let (tx, rx) = unbounded::<FileStoreEvent>();
        run_job_with_terminal_error(
            FileId(99),
            &tx,
            FileState::ErrorWhileCheckingMetadata,
            || {
                let dynamic = format!("runtime-{}-error", 42);
                panic!("{dynamic}");
            },
        );
        drop(tx);

        let mut saw_terminal_error = false;
        while let Ok(event) = rx.try_recv() {
            if let FileStoreEvent::FileError { id, state, message } = event {
                assert_eq!(id, FileId(99));
                assert_eq!(state, FileState::ErrorWhileCheckingMetadata);
                assert!(
                    message.contains("runtime-42-error"),
                    "message should carry the formatted String payload, got: {message}"
                );
                saw_terminal_error = true;
            }
        }
        assert!(
            saw_terminal_error,
            "expected a FileError event for the formatted-panic path"
        );
    }
}
