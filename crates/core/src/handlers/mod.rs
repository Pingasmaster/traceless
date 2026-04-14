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
