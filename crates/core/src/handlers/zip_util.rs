//! Utilities shared by every archive-based handler (OOXML, ODF, EPUB, and
//! eventually plain ZIP/TAR).
//!
//! The core responsibility is to normalize every ZIP member that gets
//! written so that output archives are byte-reproducible and don't leak
//! the host system's timezone, username, or clock. mat2 does the same via
//! `ZipInfo.create_system = 3`, `comment = b""`, and
//! `date_time = (1980, 1, 1, 0, 0, 0)` in `libmat2/archive.py`.

use zip::{CompressionMethod, DateTime, write::SimpleFileOptions};

/// Upper bound on eager ZIP member allocations.
///
/// ZIP local file headers carry an attacker-controlled size field.
/// `Vec::with_capacity` takes that value at face value and eagerly
/// allocates, so a crafted header claiming e.g. 10 TiB would panic with
/// `capacity overflow`. The real member body is still read in full;
/// this constant only bounds the *hint* passed to `with_capacity`.
const MAX_ALLOC_HINT: usize = 16 * 1024 * 1024;

/// Return a safe `Vec::with_capacity` hint for a member whose header
/// claims `reported_size` bytes.
///
/// The value is clamped to [`MAX_ALLOC_HINT`] so an attacker cannot drive
/// a multi-GiB eager allocation via a crafted header.
#[must_use]
pub fn safe_capacity_hint(reported_size: u64) -> usize {
    usize::try_from(reported_size)
        .unwrap_or(MAX_ALLOC_HINT)
        .min(MAX_ALLOC_HINT)
}

/// The canonical ZIP date/time for every cleaned archive member. January
/// 1st 1980 is the earliest representable value in MS-DOS date format
/// (which is what `.zip` uses internally). Using it means that two cleans
/// of the same input performed a day apart produce byte-identical output.
#[must_use]
pub fn epoch_datetime() -> DateTime {
    DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).expect("1980-01-01 is a valid ZIP DateTime")
}

/// Build the `SimpleFileOptions` used for every member we write to a
/// cleaned archive. Sets:
/// - `last_modified_time` → 1980-01-01 00:00:00
/// - `unix_permissions` → 0o644 (regular file, rw-r--r--)
/// - preserves the compression method of the source entry
#[must_use]
pub fn normalized_options(compression: CompressionMethod) -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(compression)
        .last_modified_time(epoch_datetime())
        .unix_permissions(0o644)
}

/// Returns true for members that should never be carried over into a
/// cleaned office archive. These paths are leaked fingerprints
/// (thumbnails, printer settings, tracked reviewers, etc.) and mat2 drops
/// them unconditionally.
///
/// The match is path-prefix / suffix based. The OOXML variant contains
/// both MS Office (DOCX, XLSX, PPTX) and ODF paths so every caller can
/// share the same helper.
#[must_use]
pub fn is_office_junk_path(name: &str) -> bool {
    // OOXML junk (mat2 office.py lines 133-159)
    if name.starts_with("customXml/")
        || name == "docProps/custom.xml"
        || name.starts_with("word/printerSettings/")
        || name.starts_with("ppt/printerSettings/")
        || name.starts_with("xl/printerSettings/")
        || name.starts_with("word/theme")
        || name.starts_with("ppt/theme")
        || name.starts_with("xl/theme")
        || name == "word/people.xml"
        || name == "ppt/people.xml"
        || name == "xl/people.xml"
        || name == "word/persons/person.xml"
        || name == "ppt/persons/person.xml"
        || name == "xl/persons/person.xml"
        || name.starts_with("word/tags/")
        || name.starts_with("ppt/tags/")
        || name.starts_with("xl/tags/")
        || name.starts_with("word/glossary/")
        || name.starts_with("ppt/glossary/")
        || name.starts_with("xl/glossary/")
        || name == "word/viewProps.xml"
        || name == "ppt/viewProps.xml"
        || name == "xl/viewProps.xml"
        || name == "word/presProps.xml"
        || name == "ppt/presProps.xml"
        || name == "xl/presProps.xml"
        // `numbering.xml` is list-formatting metadata that frequently
        // carries `w:rsid` author-revision markers on the producer side.
        // mat2 drops it unconditionally in every OOXML family.
        || name == "word/numbering.xml"
        || name == "ppt/numbering.xml"
        || name == "xl/numbering.xml"
        || name.ends_with("webSettings.xml")
        || name.ends_with("docMetadata/LabelInfo.xml")
    {
        return true;
    }

    // Word-style comment files (no fixed count)
    let basename = name.rsplit('/').next().unwrap_or(name);
    if basename.starts_with("comments")
        && (basename.ends_with(".xml") || basename.ends_with(".xml.rels"))
    {
        return true;
    }
    if name.contains("/threadedComments/") {
        return true;
    }

    // ODF junk (mat2 office.py LibreOfficeParser lines 573-578)
    if name.starts_with("Thumbnails/")
        || name.starts_with("Configurations2/")
        || name == "layout-cache"
        || name == "meta.xml"
    {
        return true;
    }

    // EPUB junk (mat2 epub.py lines 25-29)
    if name == "iTunesMetadata.plist" || name == "META-INF/calibre_bookmarks.txt" {
        return true;
    }

    false
}

/// Common image extensions embedded inside office archives. Members
/// matching these should be cleaned through the image handler so the
/// camera EXIF / GPS inside them is removed too.
///
/// JPEG, PNG and WebP are handled in-memory via `img-parts` in the
/// fast path. Everything else round-trips through the appropriate
/// registered `FormatHandler` via a temp file because the underlying
/// libraries (little_exif, GifHandler, SvgHandler, HarmlessHandler)
/// operate on filesystem paths rather than `&[u8]`.
#[must_use]
pub fn is_cleanable_media(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some("image/jpeg")
    } else if lower.ends_with(".png") {
        Some("image/png")
    } else if lower.ends_with(".webp") {
        Some("image/webp")
    } else if lower.ends_with(".gif") {
        Some("image/gif")
    } else if lower.ends_with(".tif") || lower.ends_with(".tiff") {
        Some("image/tiff")
    } else if lower.ends_with(".bmp") {
        Some("image/bmp")
    } else if lower.ends_with(".svg") {
        Some("image/svg+xml")
    } else if lower.ends_with(".heic") {
        Some("image/heic")
    } else if lower.ends_with(".heif") {
        Some("image/heif")
    } else if lower.ends_with(".jxl") {
        Some("image/jxl")
    } else {
        None
    }
}

/// Map an embedded-media MIME type back to the file extension the
/// temp-file round-trip needs, so handlers that detect format by
/// extension (the ImageHandler does this via mime_guess) route
/// correctly when we hand them the temp path.
#[must_use]
pub fn embedded_media_extension(mime: &str) -> Option<&'static str> {
    match mime {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        "image/tiff" => Some("tiff"),
        "image/bmp" => Some("bmp"),
        "image/svg+xml" => Some("svg"),
        "image/heic" => Some("heic"),
        "image/heif" => Some("heif"),
        "image/jxl" => Some("jxl"),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn office_junk_identifies_thumbnails_and_custom_xml() {
        assert!(is_office_junk_path("Thumbnails/thumbnail.png"));
        assert!(is_office_junk_path("docProps/custom.xml"));
        assert!(is_office_junk_path("customXml/item1.xml"));
        assert!(is_office_junk_path(
            "word/printerSettings/printerSettings1.bin"
        ));
        assert!(is_office_junk_path("word/comments.xml"));
        assert!(is_office_junk_path("word/comments12.xml"));
        assert!(is_office_junk_path(
            "word/threadedComments/threadedComment1.xml"
        ));
        assert!(is_office_junk_path("iTunesMetadata.plist"));
        assert!(is_office_junk_path("META-INF/calibre_bookmarks.txt"));
        assert!(is_office_junk_path("meta.xml"));
    }

    #[test]
    fn office_junk_drops_numbering_xml_in_every_ooxml_family() {
        // mat2's `files_to_omit` regex `^(?:word|ppt|xl)/numbering\.xml$`
        // drops list-numbering metadata in every OOXML family. Match
        // that behaviour here so `w:rsid` author-revision markers inside
        // numbering.xml cannot leak out of a cleaned document.
        assert!(is_office_junk_path("word/numbering.xml"));
        assert!(is_office_junk_path("ppt/numbering.xml"));
        assert!(is_office_junk_path("xl/numbering.xml"));
    }

    #[test]
    fn office_junk_keeps_real_files() {
        assert!(!is_office_junk_path("word/document.xml"));
        assert!(!is_office_junk_path("word/_rels/document.xml.rels"));
        assert!(!is_office_junk_path("content.xml"));
        assert!(!is_office_junk_path("styles.xml"));
        assert!(!is_office_junk_path("mimetype"));
    }

    #[test]
    fn media_detection_maps_extensions() {
        assert_eq!(
            is_cleanable_media("word/media/image1.jpg"),
            Some("image/jpeg")
        );
        assert_eq!(
            is_cleanable_media("word/media/image2.PNG"),
            Some("image/png")
        );
        assert_eq!(
            is_cleanable_media("OPS/images/cover.webp"),
            Some("image/webp")
        );
        assert_eq!(
            is_cleanable_media("word/media/photo.GIF"),
            Some("image/gif")
        );
        assert_eq!(is_cleanable_media("xl/media/scan.TIFF"), Some("image/tiff"));
        assert_eq!(is_cleanable_media("xl/media/scan.tif"), Some("image/tiff"));
        assert_eq!(
            is_cleanable_media("ppt/media/diagram.bmp"),
            Some("image/bmp")
        );
        assert_eq!(
            is_cleanable_media("OPS/images/vector.svg"),
            Some("image/svg+xml")
        );
        assert_eq!(
            is_cleanable_media("word/media/cover.heic"),
            Some("image/heic")
        );
        assert_eq!(
            is_cleanable_media("word/media/cover.heif"),
            Some("image/heif")
        );
        assert_eq!(
            is_cleanable_media("word/media/cover.JXL"),
            Some("image/jxl")
        );
        assert_eq!(is_cleanable_media("word/document.xml"), None);
    }

    #[test]
    fn embedded_media_extension_maps_every_cleanable_mime() {
        for mime in [
            "image/jpeg",
            "image/png",
            "image/webp",
            "image/gif",
            "image/tiff",
            "image/bmp",
            "image/svg+xml",
            "image/heic",
            "image/heif",
            "image/jxl",
        ] {
            assert!(
                embedded_media_extension(mime).is_some(),
                "every cleanable-media MIME must map back to an extension: {mime}"
            );
        }
        assert_eq!(embedded_media_extension("application/octet-stream"), None);
    }
}
