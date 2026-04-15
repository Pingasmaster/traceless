//! Panic-freedom matrix.
//!
//! For every handler, feed it a series of hand-crafted malformed or
//! pathological inputs and assert that both `read_metadata` and
//! `clean_metadata` return without panicking. The handlers are allowed
//! to return `Err`; they are *not* allowed to crash the process.
//!
//! This is the structural test that the rest of the suite can't
//! provide: integration fixtures only exercise the happy path plus a
//! small hand-picked set of bad inputs, so parser bugs triggered by
//! truncation or adversarial byte sequences are invisible until a real
//! user hits them. Every time one of those gets caught, the repro
//! bytes should get pasted into this file as a new matrix row.

#![allow(clippy::unwrap_used)]
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use traceless_core::format_support::get_handler_for_mime;

/// Wrap a single call and assert it did not panic. Returns the
/// `Result` from the call so the caller can additionally assert that
/// an error (if any) is of the expected shape. The `AssertUnwindSafe`
/// is required because `Box<dyn FormatHandler>` is not `UnwindSafe`,
/// and in this test we *do* want to catch anything the handler throws.
fn no_panic<T, F>(label: &str, f: F) -> T
where
    F: FnOnce() -> T,
{
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(payload) => {
            let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "<non-string panic payload>".to_string()
            };
            panic!("{label} panicked: {msg}");
        }
    }
}

/// Write `bytes` to a tempfile with the given extension and return
/// both the tempdir (to keep it alive) and the full file path.
fn fixture(bytes: &[u8], ext: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("fixture.{ext}"));
    fs::write(&path, bytes).unwrap();
    (dir, path)
}

/// Call both `read_metadata` and `clean_metadata` on a handler with
/// the given path and assert neither panics. The results are
/// discarded; we only care that the calls completed.
fn exercise(mime: &str, path: &Path, label: &str) {
    let handler =
        get_handler_for_mime(mime).unwrap_or_else(|| panic!("no handler registered for {mime}"));
    let _ = no_panic(&format!("{label}: read_metadata({mime})"), || {
        handler.read_metadata(path)
    });

    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("out.bin");
    let _ = no_panic(&format!("{label}: clean_metadata({mime})"), || {
        handler.clean_metadata(path, &out_path)
    });
}

// ============================================================
// Generic cases: every handler fed a 0-byte and a garbage file
// ============================================================

const ALL_MIMES: &[(&str, &str)] = &[
    ("image/jpeg", "jpg"),
    ("image/png", "png"),
    ("image/webp", "webp"),
    ("image/tiff", "tiff"),
    ("image/heic", "heic"),
    ("image/heif", "heif"),
    ("image/jxl", "jxl"),
    ("image/gif", "gif"),
    ("image/bmp", "bmp"),
    ("image/svg+xml", "svg"),
    ("application/pdf", "pdf"),
    ("audio/mpeg", "mp3"),
    ("audio/flac", "flac"),
    ("audio/ogg", "ogg"),
    ("audio/x-wav", "wav"),
    ("audio/mp4", "m4a"),
    ("audio/aac", "aac"),
    ("audio/x-aiff", "aiff"),
    ("audio/opus", "opus"),
    ("video/mp4", "mp4"),
    ("video/x-matroska", "mkv"),
    ("video/webm", "webm"),
    ("video/x-msvideo", "avi"),
    ("video/quicktime", "mov"),
    ("video/x-ms-wmv", "wmv"),
    ("video/x-flv", "flv"),
    (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "docx",
    ),
    (
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xlsx",
    ),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "pptx",
    ),
    ("application/vnd.oasis.opendocument.text", "odt"),
    ("application/vnd.oasis.opendocument.spreadsheet", "ods"),
    ("application/vnd.oasis.opendocument.presentation", "odp"),
    ("application/vnd.oasis.opendocument.graphics", "odg"),
    ("application/epub+zip", "epub"),
    ("text/plain", "txt"),
    ("text/html", "html"),
    ("text/css", "css"),
    ("application/x-bittorrent", "torrent"),
    ("application/zip", "zip"),
    ("application/x-tar", "tar"),
    ("application/gzip", "tar.gz"),
    ("application/x-bzip2", "tar.bz2"),
    ("application/x-xz", "tar.xz"),
];

#[test]
fn all_handlers_survive_empty_file() {
    for (mime, ext) in ALL_MIMES {
        let (_g, path) = fixture(b"", ext);
        exercise(mime, &path, &format!("empty:{mime}"));
    }
}

#[test]
fn all_handlers_survive_one_byte() {
    for (mime, ext) in ALL_MIMES {
        let (_g, path) = fixture(b"\x00", ext);
        exercise(mime, &path, &format!("single_null:{mime}"));
    }
}

#[test]
fn all_handlers_survive_random_garbage() {
    // Pseudo-random bytes from a fixed seed; avoids pulling in `rand`
    // as a dev-dep and keeps the test reproducible across machines.
    let mut buf = Vec::with_capacity(4096);
    let mut state: u32 = 0xdead_beef;
    for _ in 0..4096 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        buf.push((state >> 16) as u8);
    }
    for (mime, ext) in ALL_MIMES {
        let (_g, path) = fixture(&buf, ext);
        exercise(mime, &path, &format!("garbage:{mime}"));
    }
}

#[test]
fn all_handlers_survive_all_zeros_1kib() {
    let buf = vec![0u8; 1024];
    for (mime, ext) in ALL_MIMES {
        let (_g, path) = fixture(&buf, ext);
        exercise(mime, &path, &format!("zeros:{mime}"));
    }
}

#[test]
fn all_handlers_survive_ff_fill_1kib() {
    let buf = vec![0xffu8; 1024];
    for (mime, ext) in ALL_MIMES {
        let (_g, path) = fixture(&buf, ext);
        exercise(mime, &path, &format!("ffs:{mime}"));
    }
}

// ============================================================
// Targeted malformed inputs per family
// ============================================================

// ---- JPEG ----

#[test]
fn jpeg_truncated_after_soi() {
    let (_g, path) = fixture(&[0xff, 0xd8], "jpg");
    exercise("image/jpeg", &path, "jpeg_soi_only");
}

#[test]
fn jpeg_truncated_mid_app1() {
    // SOI + APP1 marker + a truncated length. `strip_jpeg_extra_segments`
    // parses segment-by-segment; a bogus length field used to be enough
    // to slice past the buffer end before the bounds check was added.
    let (_g, path) = fixture(
        &[
            0xff, 0xd8, // SOI
            0xff, 0xe1, // APP1
            0x10, 0x00, // fake length = 4096
            b'x', b'y', // 2 bytes instead of the promised 4094
        ],
        "jpg",
    );
    exercise("image/jpeg", &path, "jpeg_truncated_app1");
}

#[test]
fn jpeg_tiny_app1_under_29_bytes() {
    // The Exif namespace detection in image.rs indexes `&seg_data[29..]`
    // only after checking the segment length; this test pins the
    // bounds check in place so a regression panics here instead of on
    // a user's photo.
    let (_g, path) = fixture(
        &[
            0xff, 0xd8, 0xff, 0xe1, 0x00, 0x10, // APP1 length = 14 bytes of payload
            // 14 bytes of payload, too short for the `Exif\0\0` probe:
            b'E', b'x', b'i', b'f', 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ],
        "jpg",
    );
    exercise("image/jpeg", &path, "jpeg_tiny_app1");
}

// ---- PNG ----

#[test]
fn png_signature_only() {
    let sig = b"\x89PNG\r\n\x1a\n";
    let (_g, path) = fixture(sig, "png");
    exercise("image/png", &path, "png_sig_only");
}

#[test]
fn png_truncated_ihdr() {
    let mut buf = Vec::from(&b"\x89PNG\r\n\x1a\n"[..]);
    // Claim a 13-byte IHDR chunk but provide only 3 bytes.
    buf.extend_from_slice(&[0, 0, 0, 13]); // length
    buf.extend_from_slice(b"IHDR");
    buf.extend_from_slice(&[1, 2, 3]);
    let (_g, path) = fixture(&buf, "png");
    exercise("image/png", &path, "png_trunc_ihdr");
}

// ---- WebP ----

#[test]
fn webp_riff_header_only() {
    // "RIFF____WEBP" then nothing. The parser must bail cleanly.
    let (_g, path) = fixture(b"RIFF\x00\x00\x00\x00WEBP", "webp");
    exercise("image/webp", &path, "webp_header_only");
}

// ---- GIF ----

#[test]
fn gif_signature_only() {
    let (_g, path) = fixture(b"GIF89a", "gif");
    exercise("image/gif", &path, "gif_sig_only");
}

#[test]
fn gif_truncated_subblock_stream() {
    // Recent regression: the cleaner used to panic on a sub-block
    // length byte that pointed past EOF. Pin it.
    let (_g, path) = fixture(
        &[
            b'G', b'I', b'F', b'8', b'9', b'a', 0, 0, 0, 0, 0, 0, 0, 0x21, // extension
            0xfe, // comment ext
            0x05, // sub-block length = 5
            b'h', b'i', // but only 2 bytes follow
        ],
        "gif",
    );
    exercise("image/gif", &path, "gif_trunc_subblock");
}

// ---- SVG ----

#[test]
fn svg_malformed_cdata() {
    let body = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><style><![CDATA[ foo </style></svg>";
    let (_g, path) = fixture(body, "svg");
    exercise("image/svg+xml", &path, "svg_bad_cdata");
}

#[test]
fn svg_unclosed_tags() {
    let body = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><g><g><g><g>";
    let (_g, path) = fixture(body, "svg");
    exercise("image/svg+xml", &path, "svg_unclosed");
}

// ---- HTML ----

#[test]
fn html_unterminated_textarea() {
    // Sibling to the existing "unterminated script" test.
    let body = b"<html><body><textarea>leak this";
    let (_g, path) = fixture(body, "html");
    exercise("text/html", &path, "html_trunc_textarea");
}

#[test]
fn html_nested_script_in_style() {
    let body = b"<html><head><style><script>alert(1)</script></style></head></html>";
    let (_g, path) = fixture(body, "html");
    exercise("text/html", &path, "html_nested_raw");
}

#[test]
fn html_bom_prefix() {
    let mut body = Vec::new();
    body.extend_from_slice(b"\xef\xbb\xbf");
    body.extend_from_slice(b"<html><head><meta name=author content=x></head></html>");
    let (_g, path) = fixture(&body, "html");
    exercise("text/html", &path, "html_bom");
}

// ---- CSS ----

#[test]
fn css_comment_containing_close_marker_in_string() {
    let body = br#"/* foo "*/" bar */ a { color: red; }"#;
    let (_g, path) = fixture(body, "css");
    exercise("text/css", &path, "css_tricky_comment");
}

#[test]
fn css_unterminated_comment() {
    let body = b"/* never ends";
    let (_g, path) = fixture(body, "css");
    exercise("text/css", &path, "css_unterm_comment");
}

// ---- PDF ----

#[test]
fn pdf_header_only() {
    let (_g, path) = fixture(b"%PDF-1.7\n", "pdf");
    exercise("application/pdf", &path, "pdf_header_only");
}

#[test]
fn pdf_truncated_midxref() {
    let body = b"%PDF-1.4\n1 0 obj <</Type /Catalog>> endobj\nxref\n0 2\n";
    let (_g, path) = fixture(body, "pdf");
    exercise("application/pdf", &path, "pdf_trunc_xref");
}

// ---- ZIP ----

#[test]
fn zip_eocd_only() {
    // End-of-central-directory record at the very start of the file.
    // No entries, no local headers, no central directory. lopdf/zip
    // may choose to either accept this (empty archive) or reject it;
    // either way, no panic.
    let (_g, path) = fixture(
        &[
            b'P', b'K', 0x05, 0x06, // signature
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
        "zip",
    );
    exercise("application/zip", &path, "zip_eocd_only");
}

#[test]
fn zip_signature_only() {
    let (_g, path) = fixture(b"PK\x03\x04", "zip");
    exercise("application/zip", &path, "zip_sig_only");
}

// ---- TAR ----

#[test]
fn tar_all_zeros_one_block() {
    // Two all-zero 512-byte blocks form a valid empty tar. One block
    // on its own is truncated.
    let (_g, path) = fixture(&vec![0u8; 512], "tar");
    exercise("application/x-tar", &path, "tar_one_zero_block");
}

#[test]
fn tar_bogus_header_checksum() {
    let mut buf = vec![0u8; 1024];
    buf[0..5].copy_from_slice(b"hello");
    // name set, everything else zero, checksum therefore invalid.
    let (_g, path) = fixture(&buf, "tar");
    exercise("application/x-tar", &path, "tar_bogus_cksum");
}

// ---- TAR.GZ ----

#[test]
fn targz_gzip_header_only() {
    // Valid gzip magic and FHCRC flag but truncated body.
    let (_g, path) = fixture(&[0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0, 0, 0], "tar.gz");
    exercise("application/gzip", &path, "targz_header_only");
}

// ---- Torrent ----

#[test]
fn torrent_empty_dict() {
    let (_g, path) = fixture(b"de", "torrent");
    exercise("application/x-bittorrent", &path, "torrent_empty_dict");
}

#[test]
fn torrent_unterminated_int() {
    let (_g, path) = fixture(b"i12345", "torrent");
    exercise("application/x-bittorrent", &path, "torrent_unterm_int");
}

#[test]
fn torrent_deeply_nested_lists() {
    let mut body = vec![b'l'; 512];
    body.extend(std::iter::repeat_n(b'e', 512));
    let (_g, path) = fixture(&body, "torrent");
    exercise("application/x-bittorrent", &path, "torrent_nested");
}

#[test]
fn torrent_huge_string_length_prefix() {
    // Claims a 2^31-byte string but provides nothing; a naive parser
    // would allocate gigabytes.
    let (_g, path) = fixture(b"2147483647:", "torrent");
    exercise("application/x-bittorrent", &path, "torrent_huge_str");
}

// ---- Audio / FLAC / MP3 / OGG ----

#[test]
fn flac_signature_only() {
    let (_g, path) = fixture(b"fLaC", "flac");
    exercise("audio/flac", &path, "flac_sig_only");
}

#[test]
fn mp3_id3v2_header_only() {
    // "ID3" + version bytes + flags + 4-byte synchsafe length (0)
    let (_g, path) = fixture(&[b'I', b'D', b'3', 0x04, 0x00, 0x00, 0, 0, 0, 0], "mp3");
    exercise("audio/mpeg", &path, "mp3_id3_only");
}

#[test]
fn ogg_signature_only() {
    let (_g, path) = fixture(b"OggS", "ogg");
    exercise("audio/ogg", &path, "ogg_sig_only");
}

// ---- Video ----

#[test]
fn mp4_ftyp_only() {
    // Size + "ftyp" box + minor brand, nothing else.
    let (_g, path) = fixture(
        &[
            0, 0, 0, 0x18, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm', 0, 0, 0, 1, b'i', b's',
            b'o', b'm', b'a', b'v', b'c', b'1',
        ],
        "mp4",
    );
    exercise("video/mp4", &path, "mp4_ftyp_only");
}

#[test]
fn mkv_ebml_header_only() {
    let (_g, path) = fixture(&[0x1a, 0x45, 0xdf, 0xa3], "mkv");
    exercise("video/x-matroska", &path, "mkv_ebml_only");
}

// ---- Harmless fallback ----

#[test]
fn txt_random_binary() {
    let (_g, path) = fixture(&[0x00, 0xff, 0x10, 0x7e, 0x01, 0x02], "txt");
    exercise("text/plain", &path, "txt_binary");
}

#[test]
fn bmp_invalid_header() {
    let (_g, path) = fixture(b"BM\x00\x00\x00\x00", "bmp");
    exercise("image/bmp", &path, "bmp_bad_header");
}

// ============================================================
// §B. Phase C: per-format targeted malformed inputs
//
// The ALL_MIMES sweep guarantees every handler survives empty /
// 1-byte / garbage / zeros / 0xFF fill inputs. This section adds
// "looks like a real header then lies" fixtures for formats that
// previously only had the generic coverage.
// ============================================================

// ---- TIFF ----

#[test]
fn tiff_ii_byte_order_with_bogus_ifd_offset() {
    // TIFF little-endian header claiming an IFD at offset 0x7FFFFFFF
    // (past EOF). The IFD walker must bail without panicking.
    let (_g, path) = fixture(
        &[
            b'I', b'I', 0x2A, 0x00, // magic: II, 42 (classic TIFF)
            0xFF, 0xFF, 0xFF, 0x7F, // IFD offset: 0x7FFFFFFF
        ],
        "tiff",
    );
    exercise("image/tiff", &path, "tiff_bogus_ifd_offset");
}

#[test]
fn tiff_mm_byte_order_claims_zero_entries() {
    // TIFF big-endian header with an IFD0 at a valid offset, but
    // that IFD claims zero entries. Parsers must not infinite-loop
    // searching for a nonexistent entry table.
    let (_g, path) = fixture(
        &[
            b'M', b'M', 0x00, 0x2A, // magic: MM, 42
            0x00, 0x00, 0x00, 0x08, // IFD at offset 8
            0x00, 0x00, // entry count = 0
            0x00, 0x00, 0x00, 0x00, // next IFD = 0 (end)
        ],
        "tiff",
    );
    exercise("image/tiff", &path, "tiff_zero_entries");
}

// ---- HEIC / HEIF ----

#[test]
fn heic_ftyp_only_no_mdat() {
    // 24-byte ftyp box claiming heic brand, then nothing. The
    // handler must not wander off the end of the box chain.
    let (_g, path) = fixture(
        &[
            0x00, 0x00, 0x00, 0x18, b'f', b't', b'y', b'p', b'h', b'e', b'i', b'c', 0x00, 0x00,
            0x00, 0x00, b'm', b'i', b'f', b'1', b'h', b'e', b'i', b'c',
        ],
        "heic",
    );
    exercise("image/heic", &path, "heic_ftyp_only");
}

#[test]
fn heif_ftyp_mif1_truncated_before_meta_box() {
    let (_g, path) = fixture(
        &[
            0x00, 0x00, 0x00, 0x14, b'f', b't', b'y', b'p', b'm', b'i', b'f', b'1', 0x00, 0x00,
            0x00, 0x00, b'm', b'i', b'f', b'1',
        ],
        "heif",
    );
    exercise("image/heif", &path, "heif_ftyp_only");
}

// ---- JXL ----

#[test]
fn jxl_signature_only_codestream() {
    // Raw JXL codestream starts with FF 0A. Nothing after.
    let (_g, path) = fixture(&[0xFF, 0x0A], "jxl");
    exercise("image/jxl", &path, "jxl_sig_only");
}

#[test]
fn jxl_iso_bmff_container_ftyp_only() {
    // JXL container variant: starts with a JXL box
    let (_g, path) = fixture(
        &[
            0x00, 0x00, 0x00, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A, 0x87, 0x0A,
        ],
        "jxl",
    );
    exercise("image/jxl", &path, "jxl_container_sig_only");
}

// ---- OOXML: ZIP with odd shapes ----

#[test]
fn zip_local_header_without_central_dir() {
    // Valid local file header signature but no central directory
    // or EOCD. The zip crate should reject this; we only care that
    // it doesn't panic.
    let (_g, path) = fixture(
        &[
            b'P', b'K', 0x03, 0x04, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ],
        "zip",
    );
    exercise("application/zip", &path, "zip_local_header_no_cd");
}

#[test]
fn ooxml_zip_missing_content_types_xml() {
    // Valid empty zip but without a [Content_Types].xml. OOXML
    // readers should reject this, not panic. Build a minimal empty
    // archive with just one non-OOXML entry.
    use std::io::Write as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.docx");
    {
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        writer.start_file("word/document.xml", opts).unwrap();
        writer
            .write_all(b"<w:document xmlns:w=\"http://foo\"/>")
            .unwrap();
        writer.finish().unwrap();
    }
    exercise(
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        &path,
        "ooxml_missing_content_types",
    );
}

#[test]
fn ooxml_zip_with_empty_central_directory() {
    // 22-byte EOCD with zero entries. A valid empty ZIP that
    // doesn't look like any document.
    let (_g, path) = fixture(
        &[
            b'P', b'K', 0x05, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ],
        "docx",
    );
    exercise(
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        &path,
        "ooxml_empty_zip",
    );
}

// ---- ODF ----

#[test]
fn odf_zip_missing_mimetype_entry() {
    // A ZIP with an ODF-looking content.xml but no `mimetype`
    // entry at all. The ODF family requires mimetype as the first
    // (stored) entry.
    use std::io::Write as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.odt");
    {
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        writer.start_file("content.xml", opts).unwrap();
        writer.write_all(b"<content/>").unwrap();
        writer.finish().unwrap();
    }
    exercise(
        "application/vnd.oasis.opendocument.text",
        &path,
        "odf_no_mimetype",
    );
}

// ---- EPUB ----

#[test]
fn epub_zip_with_mimetype_but_no_container_xml() {
    use std::io::Write as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.epub");
    {
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("mimetype", stored).unwrap();
        writer.write_all(b"application/epub+zip").unwrap();
        writer.finish().unwrap();
    }
    exercise("application/epub+zip", &path, "epub_no_container_xml");
}

#[test]
fn epub_container_xml_points_at_missing_opf() {
    use std::io::Write as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.epub");
    {
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let opts = zip::write::SimpleFileOptions::default();
        writer.start_file("mimetype", stored).unwrap();
        writer.write_all(b"application/epub+zip").unwrap();
        writer
            .start_file("META-INF/container.xml", opts)
            .unwrap();
        writer.write_all(br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="missing.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#).unwrap();
        writer.finish().unwrap();
    }
    exercise("application/epub+zip", &path, "epub_missing_opf");
}

// ---- AAC ----

#[test]
fn aac_adts_header_with_truncated_frame_length() {
    // 7-byte ADTS header claiming a frame length that points past
    // EOF. The ADTS walker must bail on short reads.
    let (_g, path) = fixture(
        &[
            0xFF, 0xF1, 0x50, 0x80, 0xFF, 0xFF, 0xFC, // frame_length = 0x1FFF
        ],
        "aac",
    );
    exercise("audio/aac", &path, "aac_trunc_adts");
}

// ---- WAV ----

#[test]
fn wav_riff_header_only() {
    let (_g, path) = fixture(b"RIFF\x00\x00\x00\x00WAVE", "wav");
    exercise("audio/x-wav", &path, "wav_header_only");
}

#[test]
fn wav_riff_bogus_fmt_chunk_size() {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&100u32.to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&u32::MAX.to_le_bytes());
    // Plus a handful of bytes so the parser finds the chunk body is short.
    buf.extend_from_slice(&[0; 8]);
    let (_g, path) = fixture(&buf, "wav");
    exercise("audio/x-wav", &path, "wav_bogus_fmt_size");
}

// ---- AIFF ----

#[test]
fn aiff_form_header_only() {
    let (_g, path) = fixture(b"FORM\x00\x00\x00\x04AIFF", "aiff");
    exercise("audio/x-aiff", &path, "aiff_form_only");
}

// ---- M4A ----

#[test]
fn m4a_ftyp_with_no_moov() {
    // ftyp=M4A with no following moov atom.
    let (_g, path) = fixture(
        &[
            0x00, 0x00, 0x00, 0x18, b'f', b't', b'y', b'p', b'M', b'4', b'A', b' ', 0x00, 0x00,
            0x00, 0x00, b'i', b's', b'o', b'm', b'M', b'4', b'A', b' ',
        ],
        "m4a",
    );
    exercise("audio/mp4", &path, "m4a_no_moov");
}

// ---- Opus ----

#[test]
fn opus_oggs_with_incomplete_opushead() {
    // OggS page header claiming a "OpusHead" packet but the
    // segment table says zero bytes of payload.
    let (_g, path) = fixture(
        &[
            b'O', b'g', b'g', b'S', 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, // segment count
        ],
        "opus",
    );
    exercise("audio/opus", &path, "opus_ogg_incomplete");
}

// ---- Video ----

#[test]
fn avi_riff_header_only() {
    let (_g, path) = fixture(b"RIFF\x00\x00\x00\x00AVI ", "avi");
    exercise("video/x-msvideo", &path, "avi_header_only");
}

#[test]
fn mov_ftyp_qt_no_moov() {
    let (_g, path) = fixture(
        &[
            0x00, 0x00, 0x00, 0x14, b'f', b't', b'y', b'p', b'q', b't', b' ', b' ', 0x00, 0x00,
            0x00, 0x00, b'q', b't', b' ', b' ',
        ],
        "mov",
    );
    exercise("video/quicktime", &path, "mov_no_moov");
}

#[test]
fn wmv_asf_header_only() {
    // ASF GUID for the header object: 30 26 B2 75 8E 66 CF 11 A6 D9 00 AA 00 62 CE 6C
    let (_g, path) = fixture(
        &[
            0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62,
            0xCE, 0x6C, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // size
        ],
        "wmv",
    );
    exercise("video/x-ms-wmv", &path, "wmv_header_only");
}

#[test]
fn flv_header_only() {
    // FLV header: "FLV" + version + flags + header size.
    let (_g, path) = fixture(&[b'F', b'L', b'V', 0x01, 0x05, 0, 0, 0, 0x09], "flv");
    exercise("video/x-flv", &path, "flv_header_only");
}

#[test]
fn webm_ebml_with_doctype_webm_and_truncated_segment() {
    // EBML header with DocType=webm followed by a truncated Segment
    // element whose length claims more than the file has.
    let (_g, path) = fixture(
        &[
            0x1A, 0x45, 0xDF, 0xA3, // EBML master
            0x9F, // EBML size (VINT: short form)
            0x42, 0x82, 0x84, b'w', b'e', b'b', b'm', // DocType
            0x18, 0x53, 0x80, 0x67, // Segment
            0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // VINT unknown length
        ],
        "webm",
    );
    exercise("video/webm", &path, "webm_trunc_segment");
}

#[test]
fn video_ogg_with_theora_header_followed_by_eof() {
    // OggS page claiming a Theora identification packet. We
    // provide the magic byte and truncate.
    let (_g, path) = fixture(
        &[
            b'O', b'g', b'g', b'S', 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x07,
            0x80, b't', b'h', b'e', b'o', b'r', b'a',
        ],
        "ogv",
    );
    exercise("video/ogg", &path, "video_ogg_trunc");
}

// ---- tar.* compression variants ----

#[test]
fn tar_bz2_bzip_header_only() {
    // BZh9 signature + 5-byte truncated payload. bzip2 decoder
    // should return an error rather than panicking.
    let (_g, path) = fixture(b"BZh91AY&SY", "tar.bz2");
    exercise("application/x-bzip2", &path, "tarbz2_header_only");
}

#[test]
fn tar_xz_header_only() {
    // xz magic: FD 37 7A 58 5A 00, then stream flags.
    let (_g, path) = fixture(
        &[0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00, 0x00, 0x00],
        "tar.xz",
    );
    exercise("application/x-xz", &path, "tarxz_header_only");
}

#[test]
fn tar_zst_header_only() {
    // zstd magic: 28 B5 2F FD, then frame header descriptor 0.
    let (_g, path) = fixture(&[0x28, 0xB5, 0x2F, 0xFD, 0x00, 0x00, 0x00, 0x00], "tar.zst");
    exercise("application/zstd", &path, "tarzst_header_only");
}

// ---- Format-specific pathological tags ----

#[test]
fn mp3_id3v2_with_huge_synchsafe_length() {
    // ID3v2 header with synchsafe length 0x7F 0x7F 0x7F 0x7F
    // (maximum, ~256MiB). Parser must not preallocate based on
    // the advertised size.
    let (_g, path) = fixture(
        &[
            b'I', b'D', b'3', 0x04, 0x00, 0x00, 0x7F, 0x7F, 0x7F, 0x7F, b'x',
            b'x', // 2 payload bytes instead of 256MiB
        ],
        "mp3",
    );
    exercise("audio/mpeg", &path, "mp3_huge_id3");
}

#[test]
fn flac_with_truncated_metadata_block_header() {
    // fLaC + metadata block header claiming a huge length.
    let (_g, path) = fixture(
        &[
            b'f', b'L', b'a', b'C', // signature
            0x00, 0xFF, 0xFF, 0xFF, // block type 0 (STREAMINFO), length 0xFFFFFF
            b'x', b'y', // 2 bytes of payload instead of ~16MiB
        ],
        "flac",
    );
    exercise("audio/flac", &path, "flac_bogus_block_size");
}

#[test]
fn png_iend_before_ihdr() {
    // Valid PNG signature, then IEND chunk as the first chunk
    // (should be IHDR). The walker should tolerate it.
    let mut buf = Vec::from(&b"\x89PNG\r\n\x1a\n"[..]);
    // IEND chunk: 0-length, type "IEND", CRC 0xAE426082
    buf.extend_from_slice(&[0, 0, 0, 0]);
    buf.extend_from_slice(b"IEND");
    buf.extend_from_slice(&[0xAE, 0x42, 0x60, 0x82]);
    let (_g, path) = fixture(&buf, "png");
    exercise("image/png", &path, "png_iend_first");
}
