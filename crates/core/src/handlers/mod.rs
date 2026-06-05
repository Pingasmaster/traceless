pub mod archive;
pub mod audio;
pub mod css;
pub mod document;
pub mod epub;
pub mod gif;
pub mod harmless;
pub mod html;
pub mod image;
// Pure-Rust in-memory video / MP4-family-audio strippers (replace the
// native ffmpeg path). They pull `mp4-atom`/`mkv-element`, which only
// build under `wasm-inmem`, so the whole tree is feature-gated.
#[cfg(feature = "wasm-inmem")]
pub(crate) mod inmem_video;
pub mod odf;
pub mod ooxml;
pub mod pdf;
// bubblewrap + Command live behind the native feature: the wasm build has
// no subprocess surface, so this whole module (and the ffmpeg paths that
// depend on it) is compiled out.
#[cfg(feature = "native")]
pub mod sandbox;
pub mod svg;
// The handler unit tests under `tests.rs` are path-oriented (they stage
// fixtures to temp files and call the `FormatHandler` path API), so they
// only build with `native`. The in-memory `clean_bytes` round-trips have
// their own `#[cfg(test)]` modules inside each handler / `inmem.rs`.
#[cfg(all(test, feature = "native"))]
#[allow(clippy::unwrap_used)]
mod tests;
pub mod torrent;
#[cfg(feature = "native")]
pub mod video;
pub mod xml_util;
pub mod xmp;
pub mod zip_util;

#[cfg(feature = "native")]
use std::path::Path;

use crate::error::CoreError;
#[cfg(feature = "native")]
use crate::metadata::MetadataSet;

/// Hard ceiling on the size of any file that can enter the cleaner.
///
/// Every handler calls [`check_input_size`] at the top of its
/// `read_metadata` and `clean_metadata` impls, and `FileStore::add_files`
/// checks it one layer earlier. 10 GiB is comfortably above anything a
/// typical user cleans (a full photo library's worth of RAW files, a
/// complete VM image, a blu-ray rip) and well below the point where
/// any of the libraries we bind to remain sane.
///
/// The user can opt out of this (and every other resource cap) at run
/// time via [`crate::set_limits_disabled`].
pub const MAX_INPUT_FILE_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// Stat `path` and reject anything larger than [`MAX_INPUT_FILE_BYTES`].
///
/// Called by every `FormatHandler` entry point so the cap is impossible
/// to bypass by invoking a handler directly. Uses `symlink_metadata` so
/// a symlink pointing at a larger file cannot slip past the check -
/// handlers never dereference symlinks (the frontends and API are both
/// expected to pass regular files), but defending at this layer is
/// cheaper than auditing every `std::fs::read(path)` call.
///
/// Becomes a no-op while [`crate::limits_disabled`] is `true`.
///
/// # Errors
///
/// Returns [`CoreError::ReadError`] if the file cannot be stat'd,
/// or [`CoreError::FileTooLarge`] if it exceeds the cap.
#[cfg(feature = "native")]
pub(crate) fn check_input_size(path: &Path) -> Result<(), CoreError> {
    if crate::config::limits_disabled() {
        return Ok(());
    }
    let md = std::fs::symlink_metadata(path).map_err(|e| CoreError::ReadError {
        path: path.to_path_buf(),
        source: e,
    })?;
    if md.len() > MAX_INPUT_FILE_BYTES {
        return Err(CoreError::FileTooLarge {
            path: path.to_path_buf(),
            size: md.len(),
            limit: MAX_INPUT_FILE_BYTES,
        });
    }
    Ok(())
}

/// In-memory equivalent of [`check_input_size`] for the wasm build:
/// there is no path to stat, so it just bounds the already-buffered
/// input length against [`MAX_INPUT_FILE_BYTES`]. The HTTP layer in
/// `efrei-api` already enforces the same 10 GiB body cap one layer up,
/// so this is defence in depth rather than the primary gate. Becomes a
/// no-op while [`crate::limits_disabled`] is `true`.
///
/// # Errors
///
/// Returns [`CoreError::FileTooLarge`] if the buffer exceeds the cap.
pub(crate) fn check_input_len(len: usize) -> Result<(), CoreError> {
    if crate::config::limits_disabled() {
        return Ok(());
    }
    if len as u64 > MAX_INPUT_FILE_BYTES {
        return Err(CoreError::FileTooLarge {
            path: std::path::PathBuf::new(),
            size: len as u64,
            limit: MAX_INPUT_FILE_BYTES,
        });
    }
    Ok(())
}

/// Rewrite the (empty) `PathBuf` carried by a `CoreError` produced by an
/// in-memory `clean_bytes` call with the real on-disk `path`, so the
/// native path-API surfaces the same path-bearing error messages it did
/// before the in-memory refactor. Variants that carry no path are
/// returned untouched.
#[cfg(feature = "native")]
pub(crate) fn repath(err: CoreError, path: &Path) -> CoreError {
    let p = || path.to_path_buf();
    match err {
        CoreError::ReadError { source, .. } => CoreError::ReadError { path: p(), source },
        CoreError::ParseError { detail, .. } => CoreError::ParseError { path: p(), detail },
        CoreError::CleanError { detail, .. } => CoreError::CleanError { path: p(), detail },
        CoreError::NotFound { .. } => CoreError::NotFound { path: p() },
        CoreError::FileTooLarge { size, limit, .. } => CoreError::FileTooLarge {
            path: p(),
            size,
            limit,
        },
        other => other,
    }
}

/// Trait implemented by each format handler (images, PDF, audio, documents, video).
///
/// This is the **path-oriented** native API used by `FileStore` and the
/// desktop frontends. The wasm build does not use it; it dispatches
/// through the in-memory `clean_bytes` free functions in each handler
/// module (see [`crate::inmem`]).
#[cfg(feature = "native")]
pub trait FormatHandler: Send + Sync {
    /// Read metadata from the file. Returns the discovered metadata.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or its container
    /// cannot be parsed by the underlying format library.
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError>;

    /// Remove all metadata from the file, writing the cleaned version
    /// to `output_path`.
    ///
    /// # Errors
    ///
    /// Returns an error if the input cannot be read, the cleaned output
    /// cannot be written to `output_path`, or the format cannot be parsed.
    fn clean_metadata(&self, path: &Path, output_path: &Path) -> Result<(), CoreError>;

    /// MIME types this handler supports.
    fn supported_mime_types(&self) -> &[&str];
}
