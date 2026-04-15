pub mod archive;
pub mod audio;
pub mod css;
pub mod document;
pub mod epub;
pub mod gif;
pub mod harmless;
pub mod html;
pub mod image;
pub mod odf;
pub mod ooxml;
pub mod pdf;
pub mod sandbox;
pub mod svg;
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
pub mod torrent;
pub mod video;
pub mod xml_util;
pub mod xmp;
pub mod zip_util;

use std::path::Path;

use crate::error::CoreError;
use crate::metadata::MetadataSet;

/// Hard ceiling on the size of any file that can enter the cleaner.
///
/// Every handler calls [`check_input_size`] at the top of its
/// `read_metadata` and `clean_metadata` impls, and `FileStore::add_files`
/// checks it one layer earlier. 10 GiB is comfortably above anything a
/// typical user cleans (a full photo library's worth of RAW files, a
/// complete VM image, a blu-ray rip) and well below the point where
/// any of the libraries we bind to remain sane.
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
/// # Errors
///
/// Returns [`CoreError::ReadError`] if the file cannot be stat'd,
/// or [`CoreError::FileTooLarge`] if it exceeds the cap.
pub(crate) fn check_input_size(path: &Path) -> Result<(), CoreError> {
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

/// Trait implemented by each format handler (images, PDF, audio, documents, video).
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
