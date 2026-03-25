use std::path::Path;

use crate::handlers::audio::AudioHandler;
use crate::handlers::document::DocumentHandler;
use crate::handlers::image::ImageHandler;
use crate::handlers::pdf::PdfHandler;
use crate::handlers::video::VideoHandler;
use crate::handlers::FormatHandler;

/// Return the appropriate format handler for the given MIME type.
pub fn get_handler_for_mime(mime: &str) -> Option<Box<dyn FormatHandler>> {
    match mime {
        "image/jpeg" | "image/png" | "image/webp" => Some(Box::new(ImageHandler)),
        "application/pdf" => Some(Box::new(PdfHandler)),
        "audio/mpeg" | "audio/flac" | "audio/ogg" | "audio/vorbis" | "audio/mp4"
        | "audio/x-wav" | "audio/wav" | "audio/aac" | "audio/x-aiff" | "audio/x-flac"
        | "audio/x-m4a" => Some(Box::new(AudioHandler)),
        "application/vnd.oasis.opendocument.text"
        | "application/vnd.oasis.opendocument.spreadsheet"
        | "application/vnd.oasis.opendocument.presentation"
        | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        | "application/epub+zip" => Some(Box::new(DocumentHandler)),
        "video/mp4" | "video/x-matroska" | "video/webm" | "video/x-msvideo" | "video/avi"
        | "video/quicktime" => Some(Box::new(VideoHandler)),
        _ => None,
    }
}

/// Detect MIME type for a file path using extension-based guessing.
pub fn detect_mime(path: &Path) -> String {
    mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string()
}

/// All supported file extensions.
pub fn supported_extensions() -> &'static [&'static str] {
    &[
        // Images
        "jpg", "jpeg", "png", "webp",
        // PDF
        "pdf",
        // Audio
        "mp3", "flac", "ogg", "wav", "m4a", "aac", "aiff",
        // Documents
        "odt", "ods", "odp", "docx", "xlsx", "pptx", "epub",
        // Video
        "mp4", "mkv", "webm", "avi", "mov",
    ]
}
