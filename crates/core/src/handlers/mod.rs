pub mod audio;
pub mod document;
pub mod image;
pub mod pdf;
#[cfg(test)]
mod tests;
pub mod video;

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
    /// to `output_path`. If `lightweight` is true, the handler should
    /// preserve data integrity at the cost of possibly leaving some
    /// metadata intact.
    ///
    /// # Errors
    ///
    /// Returns an error if the input cannot be read, the cleaned output
    /// cannot be written to `output_path`, or the format does not support
    /// the requested clean depth.
    fn clean_metadata(
        &self,
        path: &Path,
        output_path: &Path,
        lightweight: bool,
    ) -> Result<(), CoreError>;

    /// MIME types this handler supports.
    fn supported_mime_types(&self) -> &[&str];
}
