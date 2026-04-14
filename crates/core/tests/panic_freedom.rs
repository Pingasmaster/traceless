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
use std::panic::{catch_unwind, AssertUnwindSafe};
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
    let handler = get_handler_for_mime(mime)
        .unwrap_or_else(|| panic!("no handler registered for {mime}"));
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
    ("application/vnd.openxmlformats-officedocument.wordprocessingml.document", "docx"),
    ("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", "xlsx"),
    ("application/vnd.openxmlformats-officedocument.presentationml.presentation", "pptx"),
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
    let (_g, path) = fixture(
        &[b'I', b'D', b'3', 0x04, 0x00, 0x00, 0, 0, 0, 0],
        "mp3",
    );
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
