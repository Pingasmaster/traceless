//! In-memory metadata-strip API.
//!
//! This is the entry point the `wasm32-wasip2` API component calls: a
//! single `clean_bytes(ext, input) -> Result<Vec<u8>, CoreError>` that
//! maps a filename extension to a MIME type and dispatches to the
//! matching handler's in-memory `clean_bytes`. It does NOT touch the
//! filesystem, spawn a subprocess, or use threads, so it cross-compiles
//! to wasip2 cleanly.
//!
//! It is also available (and harmless) under the native feature: the
//! per-handler `clean_bytes` functions are the single source of truth
//! for the cleaning logic, and the native path-based `FormatHandler`
//! impls call into them. Sharing the dispatch here lets the archive /
//! office-document recursion route nested members through one place
//! instead of two parallel tables.
//!
//! Pure-Rust formats are fully supported. Video (all containers) and the
//! ffmpeg-routed MP4/M4A/AAC audio containers are stubbed: they return
//! [`CoreError::NotImplementedInWasm`] so a caller gets a real, matchable
//! error rather than a panic. A later workflow fills in those bodies
//! behind this same dispatch.

use crate::error::CoreError;
use crate::handlers::{
    archive, audio, css, document, gif, harmless, html, image, pdf, svg, torrent,
};

/// Map a filename (or bare extension) to the MIME type the cleaner
/// dispatch keys on. Mirrors the extension table in
/// [`crate::format_support`] but works without a `Path`, so it is usable
/// from both the wasm build and the archive recursion. Unknown
/// extensions yield `application/octet-stream`.
#[must_use]
pub fn mime_for_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    // Longest compound archive suffixes first so `.tar.gz` wins over
    // `.gz`. These map to the same MIME types `mime_guess` produces, which
    // is what `format_support::get_handler_for_mime` routes on.
    const COMPOUND: &[(&str, &str)] = &[
        (".tar.gz", "application/gzip"),
        (".tar.bz2", "application/x-bzip2"),
        (".tar.xz", "application/x-xz"),
        (".tar.zst", "application/zstd"),
    ];
    for (suffix, mime) in COMPOUND {
        if lower.ends_with(suffix) {
            return (*mime).to_string();
        }
    }
    let ext = lower.rsplit('.').next().unwrap_or("");
    ext_to_mime(ext)
        .unwrap_or("application/octet-stream")
        .to_string()
}

/// Map a bare lowercase extension (no dot) to a MIME type. Single source
/// of truth for the in-memory dispatch.
#[must_use]
fn ext_to_mime(ext: &str) -> Option<&'static str> {
    Some(match ext {
        // Images
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "tif" | "tiff" => "image/tiff",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "jxl" => "image/jxl",
        "gif" => "image/gif",
        // PDF
        "pdf" => "application/pdf",
        // Audio
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "ogg" => "audio/ogg",
        "opus" => "audio/opus",
        "wav" => "audio/wav",
        "aiff" => "audio/aiff",
        "m4a" => "audio/m4a",
        "mp4a" => "audio/mp4",
        "aac" => "audio/aac",
        // Documents
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "odg" => "application/vnd.oasis.opendocument.graphics",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "epub" => "application/epub+zip",
        // Video
        "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "wmv" => "video/x-ms-wmv",
        "flv" => "video/x-flv",
        // Harmless
        "txt" => "text/plain",
        "bmp" => "image/bmp",
        "ppm" => "image/x-portable-pixmap",
        "pgm" => "image/x-portable-graymap",
        "pbm" => "image/x-portable-bitmap",
        "pnm" => "image/x-portable-anymap",
        // Vector / web
        "svg" => "image/svg+xml",
        "css" => "text/css",
        "html" | "htm" => "text/html",
        "xhtml" => "application/xhtml+xml",
        // P2P
        "torrent" => "application/x-bittorrent",
        // Archives
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "tgz" | "gz" => "application/gzip",
        "tbz2" | "tbz" | "bz2" => "application/x-bzip2",
        "txz" | "xz" => "application/x-xz",
        "tzst" | "zst" => "application/zstd",
        _ => return None,
    })
}

/// Whether a handler exists for the given MIME type in the in-memory
/// dispatch. Used by the archive recursion to honour the
/// unknown-member policy.
#[must_use]
pub fn has_handler(mime: &str) -> bool {
    matches!(
        mime,
        "image/jpeg"
            | "image/png"
            | "image/webp"
            | "image/tiff"
            | "image/heic"
            | "image/heif"
            | "image/jxl"
            | "image/gif"
            | "application/pdf"
            | "audio/mpeg"
            | "audio/flac"
            | "audio/ogg"
            | "audio/vorbis"
            | "audio/mp4"
            | "audio/x-wav"
            | "audio/wav"
            | "audio/aac"
            | "audio/x-aiff"
            | "audio/x-flac"
            | "audio/x-m4a"
            | "audio/m4a"
            | "audio/aiff"
            | "audio/opus"
            | "application/vnd.oasis.opendocument.text"
            | "application/vnd.oasis.opendocument.spreadsheet"
            | "application/vnd.oasis.opendocument.presentation"
            | "application/vnd.oasis.opendocument.graphics"
            | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
            | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            | "application/epub+zip"
            | "video/mp4"
            | "video/x-matroska"
            | "video/webm"
            | "video/x-msvideo"
            | "video/avi"
            | "video/quicktime"
            | "video/x-ms-wmv"
            | "video/x-flv"
            | "video/ogg"
            | "text/plain"
            | "image/bmp"
            | "image/x-ms-bmp"
            | "image/x-portable-pixmap"
            | "image/x-portable-graymap"
            | "image/x-portable-bitmap"
            | "image/x-portable-anymap"
            | "image/svg+xml"
            | "text/css"
            | "text/html"
            | "application/xhtml+xml"
            | "application/x-bittorrent"
            | "application/zip"
            | "application/x-tar"
            | "application/gzip"
            | "application/x-gzip"
            | "application/x-compressed"
            | "application/x-bzip2"
            | "application/x-bzip-compressed-tar"
            | "application/x-gtar"
            | "application/x-xz"
            | "application/zstd"
            | "application/x-zstd"
    )
}

/// Strip every piece of metadata the given file format carries, fully in
/// memory. `ext` is the filename extension (no leading dot; case
/// insensitive) or, for compound archive forms, the trailing extension
/// chain (`tar.gz`, `tar.zst`, ...). `input` is the file body.
///
/// # Errors
///
/// Returns [`CoreError::UnsupportedFormat`] for an extension no handler
/// owns, [`CoreError::NotImplementedInWasm`] for the video / MP4-family
/// audio stubs, or a handler-specific [`CoreError`] if the input cannot
/// be parsed or rewritten.
pub fn clean_bytes(ext: &str, input: &[u8]) -> Result<Vec<u8>, CoreError> {
    crate::handlers::check_input_len(input.len())?;
    // Treat `ext` as a (possibly compound) filename tail.
    let mime = mime_for_name(ext.strip_prefix('.').unwrap_or(ext));
    if mime == "application/octet-stream" {
        return Err(CoreError::UnsupportedFormat {
            mime_type: format!("no in-memory handler for extension '{ext}'"),
        });
    }
    // `clean_bytes_for_mime` needs a name to recover the extension for the
    // handlers that branch on it (image / audio / archive); synthesize one
    // from `ext`.
    let synthetic_name = format!("file.{}", ext.trim_start_matches('.'));
    clean_bytes_for_mime(&mime, &synthetic_name, input)
}

/// Dispatch by MIME to the owning handler's in-memory cleaner. `name` is
/// the member/file name, used to recover the extension for the handlers
/// that branch on it. Shared by [`clean_bytes`], the archive recursion,
/// and the office-document recursion.
///
/// # Errors
///
/// See [`clean_bytes`].
pub fn clean_bytes_for_mime(mime: &str, name: &str, input: &[u8]) -> Result<Vec<u8>, CoreError> {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match mime {
        "image/jpeg" | "image/png" | "image/webp" | "image/tiff" | "image/heic" | "image/heif"
        | "image/jxl" => image::clean_bytes(input, &ext),
        "image/gif" => gif::clean_bytes(input, &ext),
        "application/pdf" => pdf::clean_bytes(input, &ext),
        "audio/mpeg" | "audio/flac" | "audio/ogg" | "audio/vorbis" | "audio/mp4"
        | "audio/x-wav" | "audio/wav" | "audio/aac" | "audio/x-aiff" | "audio/x-flac"
        | "audio/x-m4a" | "audio/m4a" | "audio/aiff" | "audio/opus" => {
            audio::clean_bytes(input, &ext)
        }
        "application/vnd.oasis.opendocument.text"
        | "application/vnd.oasis.opendocument.spreadsheet"
        | "application/vnd.oasis.opendocument.presentation"
        | "application/vnd.oasis.opendocument.graphics"
        | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        | "application/epub+zip" => document::clean_bytes(input, &ext),
        m @ ("video/mp4" | "video/x-matroska" | "video/webm" | "video/x-msvideo" | "video/avi"
        | "video/quicktime" | "video/x-ms-wmv" | "video/x-flv" | "video/ogg") => {
            strip_video(m, input)
        }
        "text/plain"
        | "image/bmp"
        | "image/x-ms-bmp"
        | "image/x-portable-pixmap"
        | "image/x-portable-graymap"
        | "image/x-portable-bitmap"
        | "image/x-portable-anymap" => harmless::clean_bytes(input, &ext),
        "image/svg+xml" => svg::clean_bytes(input, &ext),
        "text/css" => css::clean_bytes(input, &ext),
        "text/html" | "application/xhtml+xml" => html::clean_bytes(input, &ext),
        "application/x-bittorrent" => torrent::clean_bytes(input, &ext),
        "application/zip"
        | "application/x-tar"
        | "application/gzip"
        | "application/x-gzip"
        | "application/x-compressed"
        | "application/x-bzip2"
        | "application/x-bzip-compressed-tar"
        | "application/x-gtar"
        | "application/x-xz"
        | "application/zstd"
        | "application/x-zstd" => archive::clean_bytes(input, name),
        other => Err(CoreError::UnsupportedFormat {
            mime_type: format!("no in-memory handler for MIME '{other}'"),
        }),
    }
}

/// Strip metadata from a video container, in memory, with no subprocess.
/// Routes each container family to its pure-Rust stripper. Mirrors the
/// native `ffmpeg -map_metadata -1 -map_chapters -1` strip + remux.
///
/// # Errors
///
/// Propagates the per-container [`CoreError::ParseError`] /
/// [`CoreError::CleanError`]; returns [`CoreError::UnsupportedFormat`] for a
/// MIME no stripper owns.
#[cfg(feature = "wasm-inmem")]
fn strip_video(mime: &str, input: &[u8]) -> Result<Vec<u8>, CoreError> {
    use crate::handlers::inmem_video as v;
    match mime {
        "video/mp4" | "video/quicktime" => v::isobmff::strip(input),
        "video/x-matroska" | "video/webm" => v::mkv::strip(input),
        "video/x-msvideo" | "video/avi" => v::avi::strip(input),
        "video/x-ms-wmv" => v::asf::strip(input),
        "video/x-flv" => v::flv::strip(input),
        "video/ogg" => v::ogg::strip(input),
        other => Err(CoreError::UnsupportedFormat {
            mime_type: format!("no in-memory video stripper for MIME '{other}'"),
        }),
    }
}

/// Without `wasm-inmem` the pure-Rust container strippers are not compiled
/// (they pull `mp4-atom`/`mkv-element`), and the native build reaches video
/// through the path-based ffmpeg `FormatHandler` instead, so this arm is
/// never hit in practice.
#[cfg(not(feature = "wasm-inmem"))]
fn strip_video(mime: &str, _input: &[u8]) -> Result<Vec<u8>, CoreError> {
    Err(CoreError::NotImplementedInWasm {
        format: format!("video '{mime}'"),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read, Write};

    /// A JPEG carrying an APP1 (EXIF) marker plus a COM comment.
    /// `clean_bytes` must produce an output with neither marker.
    #[test]
    fn jpeg_clean_bytes_strips_app1() {
        let dirty = dirty_jpeg_with_exif();
        // Sanity: the dirty bytes really do carry an APP1 + COM marker and
        // parse as an image.
        assert!(has_app1(&dirty), "fixture must contain an APP1 marker");
        assert!(
            contains_marker(&dirty, 0xFE),
            "fixture must contain a COM marker"
        );

        let cleaned = clean_bytes("jpg", &dirty).unwrap();
        assert!(
            !has_app1(&cleaned),
            "cleaned JPEG must not contain an APP1 (EXIF/XMP) marker"
        );
        assert!(
            !contains_marker(&cleaned, 0xFE),
            "cleaned JPEG must not contain a COM marker"
        );
    }

    /// A DOCX (zip) whose `docProps/core.xml` carries author metadata must
    /// come out with that part stubbed (no creator/lastModifiedBy text).
    #[test]
    fn docx_clean_bytes_strips_core_xml_metadata() {
        let dirty = minimal_docx_with_core_metadata();
        let cleaned = clean_bytes("docx", &dirty).unwrap();

        // Re-open the cleaned zip and inspect docProps/core.xml.
        let mut zip = zip::ZipArchive::new(Cursor::new(cleaned)).unwrap();
        let mut core = String::new();
        zip.by_name("docProps/core.xml")
            .unwrap()
            .read_to_string(&mut core)
            .unwrap();
        assert!(
            !core.contains("Alice Author"),
            "cleaned DOCX core.xml must not retain the dc:creator value, got: {core}"
        );
        assert!(
            !core.contains("Bob Reviewer"),
            "cleaned DOCX core.xml must not retain the lastModifiedBy value, got: {core}"
        );
    }

    /// A synthetic `.tar.xz` must decompress, strip, recompress, and
    /// re-decompress back to the same member set.
    #[test]
    fn tar_xz_round_trips_members() {
        round_trip_tar_archive("tar.xz");
    }

    /// Same for `.tar.zst`.
    #[test]
    fn tar_zst_round_trips_members() {
        round_trip_tar_archive("tar.zst");
    }

    // ---- helpers ----

    fn round_trip_tar_archive(ext: &str) {
        // Build an uncompressed tar with two members, dirty uid/gid/mtime,
        // compress it with the matching codec, then run it through
        // `clean_bytes` and assert the members survive a decompress.
        let raw_tar = build_dirty_tar();
        let compressed = compress_for_ext(ext, &raw_tar);

        let cleaned = clean_bytes(ext, &compressed).unwrap();

        // Decompress the cleaned archive and collect (name -> body).
        let tar_bytes = decompress_for_ext(ext, &cleaned);
        let mut archive = tar::Archive::new(Cursor::new(tar_bytes));
        let mut seen: Vec<(String, Vec<u8>)> = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().to_string_lossy().into_owned();
            let mut body = Vec::new();
            entry.read_to_end(&mut body).unwrap();
            // Metadata must be normalized.
            assert_eq!(entry.header().uid().unwrap(), 0);
            assert_eq!(entry.header().mtime().unwrap(), 0);
            seen.push((name, body));
        }
        seen.sort();
        assert_eq!(
            seen,
            vec![
                ("a.txt".to_string(), b"hello".to_vec()),
                ("b.txt".to_string(), b"world".to_vec()),
            ],
            "cleaned {ext} must round-trip back to the same tar members"
        );
    }

    fn build_dirty_tar() -> Vec<u8> {
        let mut builder = tar::Builder::new(Cursor::new(Vec::new()));
        for (name, body) in [("a.txt", &b"hello"[..]), ("b.txt", &b"world"[..])] {
            let mut h = tar::Header::new_gnu();
            h.set_path(name).unwrap();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            h.set_uid(1000);
            h.set_gid(1000);
            h.set_mtime(1_700_000_000);
            h.set_entry_type(tar::EntryType::Regular);
            h.set_cksum();
            builder.append(&h, body).unwrap();
        }
        builder.into_inner().unwrap().into_inner()
    }

    fn compress_for_ext(ext: &str, data: &[u8]) -> Vec<u8> {
        match ext {
            "tar.xz" => xz_compress(data),
            "tar.zst" => zst_compress(data),
            other => panic!("unsupported test ext {other}"),
        }
    }

    fn decompress_for_ext(ext: &str, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        match ext {
            "tar.xz" => {
                xz_reader(data).read_to_end(&mut out).unwrap();
            }
            "tar.zst" => {
                zst_reader(data).read_to_end(&mut out).unwrap();
            }
            other => panic!("unsupported test ext {other}"),
        }
        out
    }

    #[cfg(feature = "native")]
    fn xz_compress(data: &[u8]) -> Vec<u8> {
        let mut enc = xz2::write::XzEncoder::new(Vec::new(), 6);
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }
    #[cfg(not(feature = "native"))]
    fn xz_compress(data: &[u8]) -> Vec<u8> {
        let mut enc =
            lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(6)).unwrap();
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }
    #[cfg(feature = "native")]
    fn xz_reader(data: &[u8]) -> Box<dyn Read + '_> {
        Box::new(xz2::read::XzDecoder::new(Cursor::new(data)))
    }
    #[cfg(not(feature = "native"))]
    fn xz_reader(data: &[u8]) -> Box<dyn Read + '_> {
        Box::new(lzma_rust2::XzReader::new(Cursor::new(data), true))
    }

    #[cfg(feature = "native")]
    fn zst_compress(data: &[u8]) -> Vec<u8> {
        let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }
    #[cfg(not(feature = "native"))]
    fn zst_compress(data: &[u8]) -> Vec<u8> {
        ruzstd::encoding::compress_to_vec(data, ruzstd::encoding::CompressionLevel::Fastest)
    }
    #[cfg(feature = "native")]
    fn zst_reader(data: &[u8]) -> Box<dyn Read + '_> {
        Box::new(zstd::stream::read::Decoder::new(Cursor::new(data)).unwrap())
    }
    #[cfg(not(feature = "native"))]
    fn zst_reader(data: &[u8]) -> Box<dyn Read + '_> {
        Box::new(ruzstd::decoding::StreamingDecoder::new(Cursor::new(data)).unwrap())
    }

    /// Build a structurally valid baseline JPEG (SOI, APP1/EXIF, APP0/JFIF,
    /// COM, DQT, SOF0, DHT, SOS + a byte of entropy data, EOI) that
    /// img-parts parses by segment framing. Carries an APP1 EXIF marker
    /// and a COM comment for the cleaner to strip.
    fn dirty_jpeg_with_exif() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&[0xFF, 0xD8]); // SOI

        // APP1 EXIF segment: marker, length (2-byte, includes the 2 length
        // bytes), "Exif\0\0", payload.
        let exif_payload = b"Exif\0\0\x49\x49\x2a\x00fake-exif";
        let app1_len = (exif_payload.len() + 2) as u16;
        v.extend_from_slice(&[0xFF, 0xE1]);
        v.extend_from_slice(&app1_len.to_be_bytes());
        v.extend_from_slice(exif_payload);

        // APP0 JFIF
        v.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        v.extend_from_slice(b"JFIF\0");
        v.extend_from_slice(&[0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);

        // COM comment segment
        let com = b"author=alice";
        let com_len = (com.len() + 2) as u16;
        v.extend_from_slice(&[0xFF, 0xFE]);
        v.extend_from_slice(&com_len.to_be_bytes());
        v.extend_from_slice(com);

        // DQT (one 8-bit table, all ones)
        v.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x43, 0x00]);
        v.extend_from_slice(&[1u8; 64]);

        // SOF0: 8-bit precision, 1x1, 1 component (id 1, 1x1 sampling, qtable 0)
        v.extend_from_slice(&[
            0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11, 0x00,
        ]);

        // DHT: minimal DC table for component 0 (class 0, id 0). One code of
        // length 1 mapping to symbol 0.
        v.extend_from_slice(&[0xFF, 0xC4, 0x00, 0x14, 0x00]);
        v.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // 16 counts
        v.push(0x00); // one symbol

        // SOS: 1 component (id 1, DC table 0 / AC table 0), spectral 0..63, Ah/Al 0
        v.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
        // A byte of entropy-coded data.
        v.push(0x00);

        v.extend_from_slice(&[0xFF, 0xD9]); // EOI
        v
    }

    fn has_app1(jpeg: &[u8]) -> bool {
        contains_marker(jpeg, 0xE1)
    }

    /// Scan a JPEG byte stream for a given APPn/COM marker (0xFF 0xNN).
    fn contains_marker(jpeg: &[u8], marker: u8) -> bool {
        let mut i = 2; // skip SOI
        while i + 4 <= jpeg.len() {
            if jpeg[i] != 0xFF {
                i += 1;
                continue;
            }
            let m = jpeg[i + 1];
            if m == 0xD9 {
                break; // EOI
            }
            if m == marker {
                return true;
            }
            let len = ((jpeg[i + 2] as usize) << 8) | jpeg[i + 3] as usize;
            if len < 2 {
                break;
            }
            i += 2 + len;
        }
        false
    }

    /// Build a minimal DOCX: a zip with `[Content_Types].xml`, a stub
    /// `word/document.xml`, and a `docProps/core.xml` carrying dc:creator
    /// + cp:lastModifiedBy author metadata.
    fn minimal_docx_with_core_metadata() -> Vec<u8> {
        use zip::write::SimpleFileOptions;
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default();

        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>
</Types>"#,
        )
        .unwrap();

        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body/></w:document>"#,
        )
        .unwrap();

        zip.start_file("docProps/core.xml", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:creator>Alice Author</dc:creator>
<cp:lastModifiedBy>Bob Reviewer</cp:lastModifiedBy>
</cp:coreProperties>"#,
        )
        .unwrap();

        zip.finish().unwrap().into_inner()
    }
}
