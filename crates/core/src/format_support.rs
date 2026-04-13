use std::path::Path;

use crate::handlers::archive::ArchiveHandler;
use crate::handlers::audio::AudioHandler;
use crate::handlers::css::CssHandler;
use crate::handlers::document::DocumentHandler;
use crate::handlers::gif::GifHandler;
use crate::handlers::harmless::HarmlessHandler;
use crate::handlers::html::HtmlHandler;
use crate::handlers::image::ImageHandler;
use crate::handlers::pdf::PdfHandler;
use crate::handlers::svg::SvgHandler;
use crate::handlers::torrent::TorrentHandler;
use crate::handlers::video::VideoHandler;
use crate::handlers::FormatHandler;

/// Return the appropriate format handler for the given MIME type.
#[must_use]
pub fn get_handler_for_mime(mime: &str) -> Option<Box<dyn FormatHandler>> {
    match mime {
        "image/jpeg" | "image/png" | "image/webp" | "image/tiff" | "image/heic"
        | "image/heif" | "image/jxl" => Some(Box::new(ImageHandler)),
        "image/gif" => Some(Box::new(GifHandler)),
        "application/pdf" => Some(Box::new(PdfHandler)),
        "audio/mpeg" | "audio/flac" | "audio/ogg" | "audio/vorbis" | "audio/mp4"
        | "audio/x-wav" | "audio/wav" | "audio/aac" | "audio/x-aiff" | "audio/x-flac"
        | "audio/x-m4a" | "audio/m4a" | "audio/aiff" | "audio/opus" => {
            Some(Box::new(AudioHandler))
        }
        "application/vnd.oasis.opendocument.text"
        | "application/vnd.oasis.opendocument.spreadsheet"
        | "application/vnd.oasis.opendocument.presentation"
        | "application/vnd.oasis.opendocument.graphics"
        | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        | "application/epub+zip" => Some(Box::new(DocumentHandler)),
        "video/mp4" | "video/x-matroska" | "video/webm" | "video/x-msvideo" | "video/avi"
        | "video/quicktime" | "video/x-ms-wmv" | "video/x-flv" | "video/ogg" => {
            Some(Box::new(VideoHandler))
        }
        "text/plain"
        | "image/bmp"
        | "image/x-ms-bmp"
        | "image/x-portable-pixmap"
        | "image/x-portable-graymap"
        | "image/x-portable-bitmap"
        | "image/x-portable-anymap" => Some(Box::new(HarmlessHandler)),
        "image/svg+xml" => Some(Box::new(SvgHandler)),
        "text/css" => Some(Box::new(CssHandler)),
        "text/html" | "application/xhtml+xml" => Some(Box::new(HtmlHandler)),
        "application/x-bittorrent" => Some(Box::new(TorrentHandler)),
        // NB: `mime_guess` maps `.tar.gz` / `.tar.bz2` / `.tar.xz` to
        // `application/gzip` / `application/x-bzip2` / `application/x-xz`
        // respectively, so those MIME types *are* the entry point for real
        // tar-bundled archives. A plain compressed file (e.g. `foo.txt.gz`)
        // reaches the same handler, where `ArchiveFormat::detect` rejects it
        // with a specific "plain compressed" error.
        "application/zip"
        | "application/x-tar"
        | "application/gzip"
        | "application/x-gzip"
        | "application/x-compressed"
        | "application/x-bzip2"
        | "application/x-bzip-compressed-tar"
        | "application/x-gtar"
        | "application/x-xz" => Some(Box::new(ArchiveHandler)),
        _ => None,
    }
}

/// Detect MIME type for a file path using extension-based guessing.
#[must_use]
pub fn detect_mime(path: &Path) -> String {
    mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string()
}

/// All supported file extensions.
#[must_use]
pub const fn supported_extensions() -> &'static [&'static str] {
    &[
        // Images
        "jpg", "jpeg", "png", "webp", "tif", "tiff", "heic", "heif", "gif", "jxl",
        // PDF
        "pdf",
        // Audio
        "mp3", "flac", "ogg", "wav", "m4a", "aac", "aiff",
        // Documents
        "odt", "ods", "odp", "odg", "docx", "xlsx", "pptx", "epub",
        // Video
        "mp4", "mkv", "webm", "avi", "mov", "wmv", "flv",
        // Harmless (text + trivial images)
        "txt", "bmp", "ppm", "pgm", "pbm", "pnm",
        // Vector / web
        "svg", "css", "html", "htm", "xhtml",
        // P2P
        "torrent",
        // Generic archives. `.tgz` / `.tbz2` / `.txz` and their expanded
        // siblings (`.tar.gz`, `.tar.bz2`, `.tar.xz`) are matched inside
        // `ArchiveFormat::detect` via the full filename rather than the
        // final extension, so listing `tar` and `zip` here is enough for
        // the file picker.
        "zip", "tar",
    ]
}
