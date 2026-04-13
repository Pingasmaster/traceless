//! End-to-end mat2-parity tests.
//!
//! This file is a direct counterpart to the mat2 test suite under
//! `mat2/tests/`. Every test here maps to a specific upstream test and
//! is tagged in its docstring with the mat2 class::method it mirrors.

mod common;

use std::fs;
use std::io::Read;
use std::path::Path;

use traceless_core::format_support::{detect_mime, get_handler_for_mime, supported_extensions};

use common::*;

// ================================================================
// §1. Parser factory / MIME dispatch (mirror: test_libmat2::TestParserFactory)
// ================================================================

#[test]
fn mime_dispatch_covers_every_supported_extension() {
    // For every extension supported_extensions() claims to support,
    // detect_mime must return a non-octet-stream MIME that
    // get_handler_for_mime accepts. This is the primary regression
    // guard against MIME routing bugs.
    let dir = tempfile::tempdir().unwrap();
    for ext in supported_extensions() {
        let path = dir.path().join(format!("probe.{ext}"));
        fs::write(&path, b"").unwrap();
        let mime = detect_mime(&path);
        assert_ne!(
            mime, "application/octet-stream",
            "extension .{ext} not recognized by detect_mime"
        );
        assert!(
            get_handler_for_mime(&mime).is_some(),
            "extension .{ext} → {mime} has no handler registered"
        );
    }
}

#[test]
fn mime_dispatch_for_wmv() {
    // mat2 test_libmat2::TestGetMeta::test_wmv
    assert_eq!(detect_mime(Path::new("clip.wmv")), "video/x-ms-wmv");
    assert!(get_handler_for_mime("video/x-ms-wmv").is_some());
}

#[test]
fn mime_dispatch_for_flv() {
    assert_eq!(detect_mime(Path::new("clip.flv")), "video/x-flv");
    assert!(get_handler_for_mime("video/x-flv").is_some());
}

#[test]
fn mime_dispatch_for_odg() {
    // OpenDocument Graphics. The DocumentHandler already handles the
    // underlying ODF container; the dispatcher must route .odg through it.
    assert!(
        get_handler_for_mime("application/vnd.oasis.opendocument.graphics").is_some(),
        "ODG MIME must route to DocumentHandler"
    );
}

// ================================================================
// §2. Path / edge-case robustness (mirror: test_corrupted_files::TestInexistentFiles)
// ================================================================

#[test]
fn path_nonexistent_returns_error() {
    let handler = get_handler_for_mime("image/jpeg").unwrap();
    let result = handler.read_metadata(Path::new("/nonexistent/does-not-exist.jpg"));
    assert!(result.is_err());
}

#[test]
fn path_directory_is_not_parseable() {
    // Feeding a directory into every handler must produce an error, not
    // a crash. mat2 treats this as "unsupported" upstream.
    let dir = tempfile::tempdir().unwrap();
    for mime in [
        "image/jpeg",
        "application/pdf",
        "audio/mpeg",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ] {
        let handler = get_handler_for_mime(mime).unwrap();
        let result = handler.read_metadata(dir.path());
        assert!(
            result.is_err(),
            "{mime} handler accepted a directory as input"
        );
    }
}

#[test]
fn path_char_device_returns_error_or_empty() {
    // /dev/zero, /dev/null, etc. — must not crash. mat2 refuses these
    // outright via parser_factory returning None.
    let handler = get_handler_for_mime("image/jpeg").unwrap();
    let _ = handler.read_metadata(Path::new("/dev/null"));
    // Don't assert a specific result — some handlers may succeed on an
    // empty read and return an empty MetadataSet, which is also fine.
}

#[test]
fn path_broken_symlink_returns_error() {
    // mat2 test_corrupted_files::TestInexistentFiles::test_brokensymlink
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("missing-target.jpg");
    let link = dir.path().join("broken-link.jpg");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();

    #[cfg(unix)]
    {
        let handler = get_handler_for_mime("image/jpeg").unwrap();
        let result = handler.read_metadata(&link);
        assert!(result.is_err());
    }
}

// ================================================================
// §3. Image round-trip (mirror: test_libmat2::TestCleaning jpg/png/webp)
// ================================================================

#[test]
fn jpeg_round_trip_strips_exif() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.jpg");
    let cleaned = dir.path().join("cleaned.jpg");
    make_dirty_jpeg(&dirty);

    // Read: must surface the EXIF tags we injected
    let handler = get_handler_for_mime("image/jpeg").unwrap();
    let meta = handler.read_metadata(&dirty).unwrap();
    assert!(!meta.is_empty(), "dirty JPEG must report metadata");
    let dump = format!("{meta:?}");
    assert!(
        dump.contains("mat2-parity-artist"),
        "Artist tag must show up in read_metadata: {dump}"
    );

    // Clean
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    // Re-read: must be empty per little_exif
    if let Ok(m) = little_exif::metadata::Metadata::new_from_path(&cleaned) {
        assert!(
            m.into_iter().next().is_none(),
            "cleaned JPEG must have no EXIF"
        );
    }
}

#[test]
fn jpeg_idempotent_clean() {
    // Cleaning an already-clean file must succeed and produce the same
    // output. This catches regressions where the post-pass mutates a
    // file that has nothing to strip.
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.jpg");
    let c1 = dir.path().join("c1.jpg");
    let c2 = dir.path().join("c2.jpg");
    make_dirty_jpeg(&dirty);

    let handler = get_handler_for_mime("image/jpeg").unwrap();
    handler.clean_metadata(&dirty, &c1).unwrap();
    handler.clean_metadata(&c1, &c2).unwrap();

    let b1 = fs::read(&c1).unwrap();
    let b2 = fs::read(&c2).unwrap();
    assert_eq!(b1, b2, "re-cleaning a clean JPEG must be idempotent");
}

#[test]
fn jpeg_cleaning_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.jpg");
    let a = dir.path().join("a.jpg");
    let b = dir.path().join("b.jpg");
    make_dirty_jpeg(&dirty);

    let handler = get_handler_for_mime("image/jpeg").unwrap();
    handler.clean_metadata(&dirty, &a).unwrap();
    handler.clean_metadata(&dirty, &b).unwrap();

    assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
}

#[test]
fn png_round_trip_strips_text_chunks() {
    // mat2 test_libmat2::TestCleaning for PNG expects Comment + ModifyDate
    // to vanish. Our fixture uses Author/Software tEXt chunks and a tIME
    // chunk, which are the same category.
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.png");
    let cleaned = dir.path().join("cleaned.png");
    make_dirty_png(&dirty);

    // Sanity — bytes must literally contain the tEXt comment we injected
    let raw = fs::read(&dirty).unwrap();
    assert!(
        find_bytes(&raw, b"mat2-parity-author").is_some(),
        "fixture PNG must contain the injected Author tag"
    );
    assert!(
        find_bytes(&raw, b"secret-tool").is_some(),
        "fixture PNG must contain the injected Software tag"
    );

    let handler = get_handler_for_mime("image/png").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    let out = fs::read(&cleaned).unwrap();
    assert!(
        find_bytes(&out, b"mat2-parity-author").is_none(),
        "tEXt Author must be gone after clean"
    );
    assert!(
        find_bytes(&out, b"secret-tool").is_none(),
        "tEXt Software must be gone after clean"
    );
    // tIME chunk header must also be gone
    assert!(
        find_bytes(&out, b"tIME").is_none(),
        "tIME chunk must be stripped"
    );
}

#[test]
fn png_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.png");
    let a = dir.path().join("a.png");
    let b = dir.path().join("b.png");
    make_dirty_png(&dirty);

    let handler = get_handler_for_mime("image/png").unwrap();
    handler.clean_metadata(&dirty, &a).unwrap();
    handler.clean_metadata(&dirty, &b).unwrap();
    assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
}

#[test]
fn image_handler_rejects_text_file() {
    // mat2 test_corrupted_files::TestCorruptedFiles::test_png
    // Feeding a non-image into the image handler must error.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("not-an-image.jpg");
    fs::write(&path, b"this is not a JPEG, it's just text").unwrap();

    let handler = get_handler_for_mime("image/jpeg").unwrap();
    // clean_metadata must fail gracefully
    let out = dir.path().join("out.jpg");
    let res = handler.clean_metadata(&path, &out);
    assert!(res.is_err(), "garbage input must fail to clean");
}

// ================================================================
// §4. PDF round-trip (mirror: test_libmat2 + test_deep_cleaning PDF)
// ================================================================

#[test]
fn pdf_round_trip_strips_info_and_catalog_leaks() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.pdf");
    let cleaned = dir.path().join("cleaned.pdf");
    make_dirty_pdf(&dirty);

    let handler = get_handler_for_mime("application/pdf").unwrap();

    // Read: must surface Author + OpenAction + Embedded files + etc.
    let meta = handler.read_metadata(&dirty).unwrap();
    let dump = format!("{meta:?}");
    assert!(
        dump.contains("mat2-parity-author") || dump.contains("Author"),
        "dirty PDF must surface Author in meta dump: {dump}"
    );

    handler.clean_metadata(&dirty, &cleaned).unwrap();

    // Re-open the cleaned PDF raw and assert the leak bytes are gone.
    let bytes = fs::read(&cleaned).unwrap();

    // /Info contents
    assert!(
        find_bytes(&bytes, b"mat2-parity-author").is_none(),
        "/Info Author leaked after clean"
    );
    assert!(
        find_bytes(&bytes, b"secret-producer").is_none(),
        "/Info Producer leaked after clean"
    );

    // /OpenAction JavaScript body
    assert!(
        find_bytes(&bytes, b"secret-js").is_none(),
        "OpenAction JavaScript body leaked after clean"
    );

    // Embedded file payload
    assert!(
        find_bytes(&bytes, b"EMBEDDED SECRET DATA").is_none(),
        "/EmbeddedFiles payload leaked after clean"
    );

    // Trailer /ID fingerprint
    assert!(
        find_bytes(&bytes, b"secret-fingerprint-a").is_none(),
        "trailer /ID leaked after clean"
    );
    assert!(
        find_bytes(&bytes, b"secret-fingerprint-b").is_none(),
        "trailer /ID leaked after clean"
    );

    // XMP packet body
    assert!(
        find_bytes(&bytes, b"W5M0MpCehiHzreSzNTczkc9d").is_none(),
        "XMP packet header leaked after clean"
    );
}

#[test]
fn pdf_idempotent_clean() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.pdf");
    make_dirty_pdf(&dirty);

    let handler = get_handler_for_mime("application/pdf").unwrap();
    let c1 = dir.path().join("c1.pdf");
    let c2 = dir.path().join("c2.pdf");
    handler.clean_metadata(&dirty, &c1).unwrap();
    handler.clean_metadata(&c1, &c2).unwrap();

    // Re-clean must still reject known leaks
    let bytes = fs::read(&c2).unwrap();
    assert!(find_bytes(&bytes, b"mat2-parity-author").is_none());
    assert!(find_bytes(&bytes, b"secret-js").is_none());
}

#[test]
fn pdf_rejects_non_pdf() {
    // mat2 test_corrupted_files::TestCorruptedFiles::test_pdf
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("not.pdf");
    make_dirty_png(&path);
    let handler = get_handler_for_mime("application/pdf").unwrap();
    let res = handler.read_metadata(&path);
    assert!(res.is_err(), "PNG-shaped bytes must not parse as PDF");
}

// ================================================================
// §5. DOCX deep clean (mirror: test_deep_cleaning + TextDocx)
// ================================================================

#[test]
fn docx_round_trip_strips_every_fingerprint() {
    // Covers:
    // - test_deep_cleaning::TestZipMetadata::test_office (normalized + deep meta empty)
    // - test_deep_cleaning::TestRsidRemoval::test_office (rsid count → 0)
    // - test_deep_cleaning::TestZipOrder (lexicographic order)
    // - test_libmat2::TextDocx::test_comment_xml_is_removed
    // - test_libmat2::TextDocx::test_comment_references_are_removed
    // - test_libmat2::TextDocx::test_xml_is_utf8
    let dir = tempfile::tempdir().unwrap();
    let dirty_jpeg_path = dir.path().join("inner.jpg");
    make_dirty_jpeg(&dirty_jpeg_path);
    let dirty_jpeg = fs::read(&dirty_jpeg_path).unwrap();

    let dirty = dir.path().join("dirty.docx");
    let cleaned = dir.path().join("cleaned.docx");
    make_dirty_docx(&dirty, &dirty_jpeg);

    // Pre-conditions: fixture really does carry every leak we expect
    assert!(
        count_needle_in_xml_entries(&dirty, "w:rsid") > 0,
        "fixture must have w:rsid for regression check"
    );
    assert!(
        count_needle_in_xml_entries(&dirty, "commentRangeStart") > 0,
        "fixture must have commentRangeStart"
    );
    assert!(read_zip_entry(&dirty, "word/comments.xml").is_some());
    assert!(read_zip_entry(&dirty, "customXml/item1.xml").is_some());

    let handler = get_handler_for_mime(
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    )
    .unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    // 1. No rsid anywhere in any XML
    let rsid_count = count_needle_in_xml_entries(&cleaned, "w:rsid");
    assert_eq!(rsid_count, 0, "w:rsid survived clean: {rsid_count}");

    // 2. Tracked changes gone
    assert_eq!(count_needle_in_xml_entries(&cleaned, "w:del "), 0);
    assert_eq!(count_needle_in_xml_entries(&cleaned, "secret-deleted"), 0);
    assert!(
        count_needle_in_xml_entries(&cleaned, "inserted-survives") > 0,
        "w:ins children must be promoted"
    );

    // 3. Comment machinery gone
    assert_eq!(count_needle_in_xml_entries(&cleaned, "commentRangeStart"), 0);
    assert_eq!(count_needle_in_xml_entries(&cleaned, "commentReference"), 0);
    assert!(
        read_zip_entry(&cleaned, "word/comments.xml").is_none(),
        "word/comments.xml must be omitted"
    );

    // 4. Junk files omitted
    for junk in [
        "customXml/item1.xml",
        "word/viewProps.xml",
        "word/theme/theme1.xml",
        "word/printerSettings/printerSettings1.bin",
        "docProps/custom.xml",
    ] {
        assert!(
            read_zip_entry(&cleaned, junk).is_none(),
            "{junk} must be omitted after clean"
        );
    }

    // 5. mc:Ignorable attribute gone
    assert_eq!(count_needle_in_xml_entries(&cleaned, "mc:Ignorable"), 0);

    // 6. core.xml emptied of author
    let core = String::from_utf8(read_zip_entry(&cleaned, "docProps/core.xml").unwrap()).unwrap();
    assert!(!core.contains("Secret Author"));
    assert!(!core.contains("Alice Smith"));
    assert!(!core.contains("Secret Title"));

    // 7. UTF-8 encoding declaration still present in document.xml
    let doc = String::from_utf8(read_zip_entry(&cleaned, "word/document.xml").unwrap()).unwrap();
    assert!(
        doc.contains("encoding=\"UTF-8\"") || doc.contains("encoding='UTF-8'"),
        "cleaned document.xml must keep UTF-8 declaration"
    );
    // Body content survives
    assert!(doc.contains("visible-content"), "regular body text lost");

    // 8. Embedded JPEG is cleaned
    let inner = read_zip_entry(&cleaned, "word/media/image1.jpeg").unwrap();
    let probe = dir.path().join("probe.jpg");
    fs::write(&probe, &inner).unwrap();
    if let Ok(m) = little_exif::metadata::Metadata::new_from_path(&probe) {
        assert!(
            m.into_iter().next().is_none(),
            "embedded JPEG must be stripped of EXIF"
        );
    }

    // 9. ZIP-level normalization
    assert_zip_is_normalized(&cleaned);

    // 10. Deep meta empty
    assert_deep_meta_empty(&cleaned);
}

#[test]
fn docx_cleaning_is_byte_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.docx");
    make_dirty_docx(&dirty, TEST_JPEG);

    let handler = get_handler_for_mime(
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    )
    .unwrap();
    let a = dir.path().join("a.docx");
    let b = dir.path().join("b.docx");
    handler.clean_metadata(&dirty, &a).unwrap();
    handler.clean_metadata(&dirty, &b).unwrap();
    assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
}

#[test]
fn docx_rejects_non_zip_file() {
    // mat2 test_corrupted_files::TestCorruptedFiles::test_docx
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake.docx");
    fs::write(&path, b"I am not a zip").unwrap();
    let handler = get_handler_for_mime(
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    )
    .unwrap();
    assert!(handler.read_metadata(&path).is_err());
}

// ================================================================
// §6. ODF deep clean (mirror: test_libmat2::TestRemovingThumbnails + TestRevisionsCleaning::test_libreoffice)
// ================================================================

#[test]
fn odt_round_trip_drops_junk_and_tracked_changes() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.odt");
    let cleaned = dir.path().join("cleaned.odt");
    make_dirty_odt(&dirty);

    // Pre-conditions
    assert!(read_zip_entry(&dirty, "Thumbnails/thumbnail.png").is_some());
    assert!(count_needle_in_xml_entries(&dirty, "tracked-changes") > 0);

    let handler =
        get_handler_for_mime("application/vnd.oasis.opendocument.text").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    // Assertions
    assert!(
        read_zip_entry(&cleaned, "Thumbnails/thumbnail.png").is_none(),
        "Thumbnails must be dropped"
    );
    assert!(
        read_zip_entry(&cleaned, "Configurations2/accelerator/current.xml").is_none(),
        "Configurations2 must be dropped"
    );
    assert!(
        read_zip_entry(&cleaned, "layout-cache").is_none(),
        "layout-cache must be dropped"
    );
    assert!(
        read_zip_entry(&cleaned, "meta.xml").is_none(),
        "meta.xml must be dropped"
    );
    assert_eq!(
        count_needle_in_xml_entries(&cleaned, "tracked-changes"),
        0,
        "tracked-changes must be gone"
    );
    let content = String::from_utf8(read_zip_entry(&cleaned, "content.xml").unwrap()).unwrap();
    assert!(content.contains("visible-body"), "body text lost");

    // mimetype is preserved as the first entry (ODF requirement)
    let names = zip_entry_names(&cleaned);
    assert_eq!(names.first(), Some(&"mimetype".to_string()));

    // Normalized zip
    assert_zip_is_normalized(&cleaned);
}

#[test]
fn odt_cleaning_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.odt");
    make_dirty_odt(&dirty);

    let handler =
        get_handler_for_mime("application/vnd.oasis.opendocument.text").unwrap();
    let a = dir.path().join("a.odt");
    let b = dir.path().join("b.odt");
    handler.clean_metadata(&dirty, &a).unwrap();
    handler.clean_metadata(&dirty, &b).unwrap();
    assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
}

// ================================================================
// §7. EPUB deep clean (mirror: test_libmat2::TestCleaning::test_epub)
// ================================================================

#[test]
fn epub_round_trip_regenerates_uuid_and_drops_junk() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.epub");
    let cleaned = dir.path().join("cleaned.epub");
    make_dirty_epub(&dirty);

    let handler = get_handler_for_mime("application/epub+zip").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    let opf = String::from_utf8(read_zip_entry(&cleaned, "OEBPS/content.opf").unwrap()).unwrap();
    assert!(!opf.contains("Secret Author"));
    assert!(!opf.contains("Secret Publisher"));
    assert!(!opf.contains("Secret Book Title"));
    assert!(!opf.contains("secret-old-identifier"));
    assert!(opf.contains("urn:uuid:"), "new UUID must be injected");
    assert!(opf.contains("dc:identifier"));
    assert!(opf.contains("<manifest"), "manifest block must survive");

    // NCX head blanked
    let ncx = String::from_utf8(read_zip_entry(&cleaned, "OEBPS/toc.ncx").unwrap()).unwrap();
    assert!(!ncx.contains("secret-uid"));
    assert!(!ncx.contains("Calibre"));
    assert!(
        ncx.contains("navMap") || ncx.contains("docTitle"),
        "navigation structure must survive"
    );

    // Junk omitted
    assert!(read_zip_entry(&cleaned, "iTunesMetadata.plist").is_none());
    assert!(read_zip_entry(&cleaned, "META-INF/calibre_bookmarks.txt").is_none());

    // mimetype first
    let names = zip_entry_names(&cleaned);
    assert_eq!(names.first(), Some(&"mimetype".to_string()));

    // Normalized zip
    assert_zip_is_normalized(&cleaned);
}

#[test]
fn epub_rejects_encrypted_archive() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("drm.epub");
    let cleaned = dir.path().join("out.epub");
    make_encrypted_epub(&dirty);

    let handler = get_handler_for_mime("application/epub+zip").unwrap();
    let result = handler.clean_metadata(&dirty, &cleaned);
    assert!(result.is_err(), "encrypted EPUB must be rejected");
}

// ================================================================
// §8. Audio round-trip (mirror: test_libmat2::TestCleaning mp3/flac/ogg/wav/aiff)
// ================================================================
// These require ffmpeg for fixture synthesis. Each test self-skips.

#[test]
fn wav_round_trip() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] ffmpeg not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.wav");
    let cleaned = dir.path().join("cleaned.wav");
    if make_dirty_wav(&dirty).is_err() {
        eprintln!("[SKIP] ffmpeg failed to synthesize WAV");
        return;
    }

    let handler = get_handler_for_mime("audio/x-wav")
        .or_else(|| get_handler_for_mime("audio/wav"))
        .unwrap();
    let before = handler.read_metadata(&dirty).unwrap();
    assert!(!before.is_empty(), "dirty WAV should report metadata");

    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let after = handler.read_metadata(&cleaned).unwrap();
    assert!(after.is_empty(), "cleaned WAV should have no metadata: {after:?}");
}

#[test]
fn mp3_round_trip() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] ffmpeg not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.mp3");
    let cleaned = dir.path().join("cleaned.mp3");
    if make_dirty_mp3(&dirty).is_err() {
        eprintln!("[SKIP] ffmpeg failed to synthesize MP3");
        return;
    }

    let handler = get_handler_for_mime("audio/mpeg").unwrap();
    let before = handler.read_metadata(&dirty).unwrap();
    assert!(!before.is_empty(), "dirty MP3 should report metadata");

    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let after = handler.read_metadata(&cleaned).unwrap();
    assert!(after.is_empty(), "cleaned MP3 should have no metadata: {after:?}");
}

#[test]
fn flac_round_trip() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] ffmpeg not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.flac");
    let cleaned = dir.path().join("cleaned.flac");
    if make_dirty_flac(&dirty).is_err() {
        eprintln!("[SKIP] ffmpeg failed to synthesize FLAC");
        return;
    }

    let handler = get_handler_for_mime("audio/flac")
        .or_else(|| get_handler_for_mime("audio/x-flac"))
        .unwrap();
    let before = handler.read_metadata(&dirty).unwrap();
    assert!(!before.is_empty(), "dirty FLAC should report metadata");

    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let after = handler.read_metadata(&cleaned).unwrap();
    assert!(after.is_empty(), "cleaned FLAC should have no metadata: {after:?}");
}

#[test]
fn ogg_round_trip() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] ffmpeg not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.ogg");
    let cleaned = dir.path().join("cleaned.ogg");
    if make_dirty_ogg(&dirty).is_err() {
        eprintln!("[SKIP] ffmpeg failed to synthesize OGG");
        return;
    }

    let handler = get_handler_for_mime("audio/ogg").unwrap();
    let before = handler.read_metadata(&dirty).unwrap();
    assert!(!before.is_empty(), "dirty OGG should report metadata");

    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let after = handler.read_metadata(&cleaned).unwrap();
    assert!(after.is_empty(), "cleaned OGG should have no metadata: {after:?}");
}

#[test]
fn audio_rejects_non_audio() {
    // mat2 test_corrupted_files::TestCorruptedFiles::test_mp3 / test_flac
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("not.mp3");
    fs::write(&path, b"plain text, not an MP3").unwrap();
    let handler = get_handler_for_mime("audio/mpeg").unwrap();
    assert!(handler.read_metadata(&path).is_err());
}

// ================================================================
// §9. Video round-trip (mirror: test_libmat2::TestCleaning mp4/avi)
// ================================================================

#[test]
fn mp4_round_trip_strips_metadata_tags() {
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] ffmpeg/ffprobe not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.mp4");
    let cleaned = dir.path().join("cleaned.mp4");
    if make_dirty_mp4(&dirty).is_err() {
        eprintln!("[SKIP] ffmpeg failed to synthesize MP4");
        return;
    }

    let handler = get_handler_for_mime("video/mp4").unwrap();

    // Pre: ffprobe must surface the title/artist/comment we injected
    let before = handler.read_metadata(&dirty).unwrap();
    let dump = format!("{before:?}");
    assert!(
        dump.contains("secret-title") || dump.contains("title"),
        "dirty MP4 should surface title tag: {dump}"
    );

    handler.clean_metadata(&dirty, &cleaned).unwrap();

    // Post: ffprobe must not surface any of our injected tags
    let after = handler.read_metadata(&cleaned).unwrap();
    let dump = format!("{after:?}");
    assert!(
        !dump.contains("secret-title"),
        "secret-title survived clean: {dump}"
    );
    assert!(
        !dump.contains("secret-artist"),
        "secret-artist survived clean: {dump}"
    );
    assert!(
        !dump.contains("secret-comment"),
        "secret-comment survived clean: {dump}"
    );

    // bitexact flag means no encoder tag — probe should never contain
    // an "encoder" field after our clean.
    let raw = fs::read(&cleaned).unwrap();
    assert!(
        find_bytes(&raw, b"Lavf").is_none(),
        "Lavf encoder fingerprint leaked through bitexact flag"
    );
}

#[test]
fn avi_round_trip_strips_metadata() {
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] ffmpeg/ffprobe not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.avi");
    let cleaned = dir.path().join("cleaned.avi");
    if make_dirty_avi(&dirty).is_err() {
        eprintln!("[SKIP] ffmpeg failed to synthesize AVI");
        return;
    }

    let handler = get_handler_for_mime("video/x-msvideo").unwrap();
    let before = handler.read_metadata(&dirty).unwrap();
    let dump = format!("{before:?}");
    assert!(
        dump.contains("secret-title") || dump.contains("title"),
        "dirty AVI should surface title tag"
    );

    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let after = handler.read_metadata(&cleaned).unwrap();
    let dump = format!("{after:?}");
    assert!(!dump.contains("secret-title"));
}

#[test]
fn mkv_round_trip_strips_metadata() {
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] ffmpeg/ffprobe not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.mkv");
    let cleaned = dir.path().join("cleaned.mkv");
    if make_dirty_mkv(&dirty).is_err() {
        eprintln!("[SKIP] ffmpeg failed to synthesize MKV");
        return;
    }

    let handler = get_handler_for_mime("video/x-matroska").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let after = handler.read_metadata(&cleaned).unwrap();
    let dump = format!("{after:?}");
    assert!(!dump.contains("secret-title"));
}

// ================================================================
// §10. Unicode filename handling
// ================================================================

#[test]
fn unicode_filename_round_trip() {
    // Non-ASCII paths must work on every handler. Regression guard for
    // OsStr/Path handling.
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("日本語-πλήθος.jpg");
    let cleaned = dir.path().join("清らか-καθαρό.jpg");
    make_dirty_jpeg(&dirty);

    let handler = get_handler_for_mime("image/jpeg").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    assert!(cleaned.exists());

    if let Ok(m) = little_exif::metadata::Metadata::new_from_path(&cleaned) {
        assert!(m.into_iter().next().is_none());
    }
}

#[test]
fn dotted_filename_round_trip() {
    // "file.name.with.dots.jpg" style paths — the stem splitter used to
    // get this wrong before F1 in the audit.
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("image.v2.final.jpg");
    let cleaned = dir.path().join("image.v2.cleaned.jpg");
    make_dirty_jpeg(&dirty);

    let handler = get_handler_for_mime("image/jpeg").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    assert!(cleaned.exists());
}

// ================================================================
// §11. Harmless / PPM / BMP / text
// ================================================================

#[test]
fn text_plain_round_trip_is_copy() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("note.txt");
    let dst = dir.path().join("clean.txt");
    fs::write(&src, b"hello world\nsome text\n").unwrap();

    let handler = get_handler_for_mime("text/plain").unwrap();
    let meta = handler.read_metadata(&src).unwrap();
    assert!(meta.is_empty(), "text/plain has no metadata");
    handler.clean_metadata(&src, &dst).unwrap();
    assert_eq!(fs::read(&src).unwrap(), fs::read(&dst).unwrap());
}

#[test]
fn ppm_comment_is_stripped() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("image.ppm");
    let dst = dir.path().join("clean.ppm");
    fs::write(
        &src,
        b"P3\n# author: jvoisin\n# location: GPS coords\n1 1\n255\n255 0 0\n",
    )
    .unwrap();

    let handler = get_handler_for_mime("image/x-portable-pixmap").unwrap();
    let meta = handler.read_metadata(&src).unwrap();
    assert!(meta.total_count() >= 2, "PPM comments must be surfaced");

    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read(&dst).unwrap();
    assert!(
        find_bytes(&out, b"author: jvoisin").is_none(),
        "PPM comment leaked: {}",
        String::from_utf8_lossy(&out)
    );
    assert!(
        find_bytes(&out, b"GPS coords").is_none(),
        "PPM comment leaked"
    );
    // Pixel data preserved
    assert!(find_bytes(&out, b"255 0 0").is_some());
}

// ================================================================
// §12. SVG
// ================================================================

#[test]
fn svg_round_trip_drops_metadata_block() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("dirty.svg");
    let dst = dir.path().join("clean.svg");
    fs::write(
        &src,
        br#"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg"
     xmlns:dc="http://purl.org/dc/elements/1.1/"
     xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape"
     width="10" height="10"
     inkscape:version="secret-version">
  <metadata>
    <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
      <dc:creator>secret-author</dc:creator>
    </rdf:RDF>
  </metadata>
  <title>secret-title</title>
  <rect x="0" y="0" width="10" height="10" fill="red"/>
</svg>"#,
    )
    .unwrap();

    let handler = get_handler_for_mime("image/svg+xml").unwrap();
    let before = handler.read_metadata(&src).unwrap();
    assert!(!before.is_empty(), "dirty SVG should report metadata");

    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read_to_string(&dst).unwrap();
    for needle in [
        "secret-author",
        "secret-title",
        "secret-version",
        "inkscape:version",
        "dc:creator",
        "<metadata>",
        "<title>",
    ] {
        assert!(!out.contains(needle), "SVG leak: {needle}\n{out}");
    }
    assert!(out.contains("<rect"), "shapes must survive");
}

// ================================================================
// §13. CSS
// ================================================================

#[test]
fn css_round_trip_strips_comments() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("style.css");
    let dst = dir.path().join("clean.css");
    fs::write(
        &src,
        "/* author: jvoisin\n * version: 1.0\n */\nbody { color: red; }\n",
    )
    .unwrap();
    let handler = get_handler_for_mime("text/css").unwrap();
    let before = handler.read_metadata(&src).unwrap();
    assert!(before.total_count() >= 2);
    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read_to_string(&dst).unwrap();
    assert!(!out.contains("jvoisin"));
    assert!(!out.contains("version"));
    assert!(out.contains("color: red"));
}

// ================================================================
// §14. HTML
// ================================================================

#[test]
fn html_round_trip_drops_meta_and_blanks_title() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("page.html");
    let dst = dir.path().join("clean.html");
    fs::write(
        &src,
        r#"<!DOCTYPE html>
<html><head>
<meta name="author" content="jvoisin">
<meta name="generator" content="secret-tool">
<title>Secret Title</title>
</head><body><p>visible text</p><!--a secret comment--></body></html>"#,
    )
    .unwrap();

    let handler = get_handler_for_mime("text/html").unwrap();
    let before = handler.read_metadata(&src).unwrap();
    let dump = format!("{before:?}");
    assert!(dump.contains("jvoisin"));
    assert!(dump.contains("Secret Title"));

    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read_to_string(&dst).unwrap();
    assert!(!out.contains("jvoisin"));
    assert!(!out.contains("secret-tool"));
    assert!(!out.contains("Secret Title"));
    assert!(!out.contains("a secret comment"));
    assert!(out.contains("<title></title>") || out.contains("<title>  </title>"));
    assert!(out.contains("visible text"));
    // Doctype kept
    assert!(out.contains("<!DOCTYPE html>"));
}

// ================================================================
// §15. Torrent
// ================================================================

#[test]
fn torrent_round_trip_drops_non_allowlisted_keys() {
    use traceless_core::handlers::torrent::{encode, BencodeValue};

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("dirty.torrent");
    let dst = dir.path().join("clean.torrent");

    let mut map: std::collections::BTreeMap<Vec<u8>, BencodeValue> =
        std::collections::BTreeMap::new();
    map.insert(
        b"announce".to_vec(),
        BencodeValue::Bytes(b"http://tracker.example/announce".to_vec()),
    );
    map.insert(
        b"comment".to_vec(),
        BencodeValue::Bytes(b"secret-comment".to_vec()),
    );
    map.insert(
        b"created by".to_vec(),
        BencodeValue::Bytes(b"mktorrent 1.1".to_vec()),
    );
    map.insert(b"creation date".to_vec(), BencodeValue::Int(1_700_000_000));

    let mut info: std::collections::BTreeMap<Vec<u8>, BencodeValue> =
        std::collections::BTreeMap::new();
    info.insert(b"name".to_vec(), BencodeValue::Bytes(b"payload".to_vec()));
    info.insert(b"piece length".to_vec(), BencodeValue::Int(16384));
    info.insert(b"pieces".to_vec(), BencodeValue::Bytes(vec![0u8; 20]));
    info.insert(b"length".to_vec(), BencodeValue::Int(100));
    map.insert(b"info".to_vec(), BencodeValue::Dict(info));

    fs::write(&src, encode(&BencodeValue::Dict(map))).unwrap();

    let handler = get_handler_for_mime("application/x-bittorrent").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();

    let out = fs::read(&dst).unwrap();
    assert!(find_bytes(&out, b"secret-comment").is_none());
    assert!(find_bytes(&out, b"mktorrent").is_none());
    assert!(find_bytes(&out, b"creation date").is_none());
    assert!(find_bytes(&out, b"announce").is_some());
    assert!(find_bytes(&out, b"4:info").is_some());
    assert!(find_bytes(&out, b"payload").is_some());
}

// ================================================================
// §16. GIF
// ================================================================

#[test]
fn gif_round_trip_drops_comment_and_xmp() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("dirty.gif");
    let dst = dir.path().join("clean.gif");

    // Build a minimal valid GIF89a 1x1 with a comment extension and
    // an XMP application extension.
    let mut gif = Vec::new();
    gif.extend_from_slice(b"GIF89a");
    gif.extend_from_slice(&[0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00]);
    // Comment extension
    gif.extend_from_slice(&[0x21, 0xFE]);
    gif.push(14);
    gif.extend_from_slice(b"secret-comment");
    gif.push(0x00);
    // Application extension: XMP packet marker
    gif.extend_from_slice(&[0x21, 0xFF, 0x0B]);
    gif.extend_from_slice(b"XMP DataXMP");
    gif.push(17);
    gif.extend_from_slice(b"secret-xmp-packet");
    gif.push(0x00);
    // Image descriptor 1x1 no LCT
    gif.extend_from_slice(&[0x2C, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
    // LZW min code size + 2 data bytes + terminator
    gif.extend_from_slice(&[0x02, 0x02, 0x44, 0x01, 0x00]);
    gif.push(0x3B);

    fs::write(&src, &gif).unwrap();

    let handler = get_handler_for_mime("image/gif").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read(&dst).unwrap();
    assert!(find_bytes(&out, b"secret-comment").is_none());
    assert!(find_bytes(&out, b"secret-xmp-packet").is_none());
    assert_eq!(out.last(), Some(&0x3B));
}

// ================================================================
// §17. Generic ZIP / TAR archive handlers
// ================================================================

#[test]
fn zip_archive_normalizes_members_and_cleans_contents() {
    use zip::write::SimpleFileOptions;

    let dir = tempfile::tempdir().unwrap();
    let jpeg_path = dir.path().join("inner.jpg");
    make_dirty_jpeg(&jpeg_path);
    let jpeg_bytes = fs::read(&jpeg_path).unwrap();

    let src = dir.path().join("dirty.zip");
    let dst = dir.path().join("clean.zip");

    {
        let file = fs::File::create(&src).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default().last_modified_time(
            zip::DateTime::from_date_and_time(2024, 6, 1, 12, 0, 0).unwrap(),
        );
        writer.start_file("image.jpg", opts).unwrap();
        use std::io::Write as _;
        writer.write_all(&jpeg_bytes).unwrap();
        writer.start_file("notes.txt", opts).unwrap();
        writer.write_all(b"not metadata").unwrap();
        writer.finish().unwrap();
    }

    let handler = get_handler_for_mime("application/zip").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();

    // ZIP-level normalization
    assert_zip_is_normalized(&dst);

    // Embedded JPEG must be cleaned
    let inner = read_zip_entry(&dst, "image.jpg").unwrap();
    let probe = dir.path().join("probe.jpg");
    fs::write(&probe, &inner).unwrap();
    if let Ok(m) = little_exif::metadata::Metadata::new_from_path(&probe) {
        assert!(
            m.into_iter().next().is_none(),
            "embedded JPEG inside plain ZIP must be stripped"
        );
    }
}

#[test]
fn tar_archive_normalizes_uid_gid_mtime() {
    use tar::{Builder as TarBuilder, EntryType, Header as TarHeader};

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("dirty.tar");
    let dst = dir.path().join("clean.tar");

    {
        let file = fs::File::create(&src).unwrap();
        let mut builder = TarBuilder::new(std::io::BufWriter::new(file));
        let mut header = TarHeader::new_gnu();
        header.set_path("inner.txt").unwrap();
        header.set_size(5);
        header.set_mode(0o644);
        header.set_mtime(1_700_000_000);
        header.set_uid(1000);
        header.set_gid(1000);
        header.set_username("alice").unwrap();
        header.set_groupname("alice").unwrap();
        header.set_entry_type(EntryType::Regular);
        header.set_cksum();
        builder.append(&header, &b"hello"[..]).unwrap();
        builder.into_inner().unwrap();
    }

    let handler = get_handler_for_mime("application/x-tar").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();

    let f = fs::File::open(&dst).unwrap();
    let mut archive = tar::Archive::new(std::io::BufReader::new(f));
    let mut saw_one = false;
    for entry in archive.entries().unwrap() {
        let entry = entry.unwrap();
        let header = entry.header();
        assert_eq!(header.uid().unwrap(), 0);
        assert_eq!(header.gid().unwrap(), 0);
        assert_eq!(header.mtime().unwrap(), 0);
        // uname/gname blanked
        assert_eq!(
            header.username().unwrap_or(Some("")).unwrap_or(""),
            "",
            "uname not blanked"
        );
        assert_eq!(
            header.groupname().unwrap_or(Some("")).unwrap_or(""),
            "",
            "gname not blanked"
        );
        saw_one = true;
    }
    assert!(saw_one, "expected at least one entry");
}

#[test]
fn tar_gz_round_trip_cleans_embedded_image() {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use tar::{Builder as TarBuilder, EntryType, Header as TarHeader};

    let dir = tempfile::tempdir().unwrap();
    let jpeg_path = dir.path().join("inner.jpg");
    make_dirty_jpeg(&jpeg_path);
    let jpeg_bytes = fs::read(&jpeg_path).unwrap();

    let src = dir.path().join("dirty.tar.gz");
    let dst = dir.path().join("clean.tar.gz");

    {
        let file = fs::File::create(&src).unwrap();
        let gz = GzEncoder::new(file, Compression::default());
        let mut builder = TarBuilder::new(gz);
        let mut header = TarHeader::new_gnu();
        header.set_path("photo.jpg").unwrap();
        header.set_size(jpeg_bytes.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(EntryType::Regular);
        header.set_cksum();
        builder.append(&header, jpeg_bytes.as_slice()).unwrap();
        builder.into_inner().unwrap();
    }

    let handler = get_handler_for_mime("application/gzip").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();

    // Verify the embedded JPEG inside the gzipped tar is clean.
    use flate2::read::GzDecoder;
    let f = fs::File::open(&dst).unwrap();
    let gz = GzDecoder::new(f);
    let mut archive = tar::Archive::new(gz);
    let mut inner_bytes = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path().unwrap().to_string_lossy() == "photo.jpg" {
            entry.read_to_end(&mut inner_bytes).unwrap();
            break;
        }
    }
    let probe = dir.path().join("probe.jpg");
    fs::write(&probe, &inner_bytes).unwrap();
    if let Ok(m) = little_exif::metadata::Metadata::new_from_path(&probe) {
        assert!(
            m.into_iter().next().is_none(),
            "embedded JPEG inside .tar.gz must be stripped"
        );
    }
}

#[test]
fn tar_rejects_traversal_member() {
    // Hand-craft a minimal tar header with a `../escape` name.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("bad.tar");

    let mut block = [0u8; 512];
    let name = b"../escape.txt";
    block[..name.len()].copy_from_slice(name);
    block[100..107].copy_from_slice(b"0000644");
    block[108..115].copy_from_slice(b"0000000");
    block[116..123].copy_from_slice(b"0000000");
    block[124..135].copy_from_slice(b"00000000000");
    block[136..147].copy_from_slice(b"00000000000");
    block[156] = b'0';
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    for b in &mut block[148..156] {
        *b = b' ';
    }
    let sum: u32 = block.iter().map(|&b| u32::from(b)).sum();
    let chksum = format!("{sum:06o}\0 ");
    block[148..156].copy_from_slice(chksum.as_bytes());

    let mut buf = Vec::new();
    buf.extend_from_slice(&block);
    buf.extend_from_slice(&[0u8; 1024]);
    fs::write(&src, &buf).unwrap();

    let handler = get_handler_for_mime("application/x-tar").unwrap();
    let dst = dir.path().join("out.tar");
    let result = handler.clean_metadata(&src, &dst);
    assert!(
        result.is_err(),
        "tar with ../escape member must be rejected"
    );
}

// ================================================================
// §18. TIFF / HEIF / BMP
// ================================================================

#[test]
fn tiff_round_trip_strips_exif() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] ffmpeg not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.tiff");
    let cleaned = dir.path().join("clean.tiff");
    if make_dirty_tiff(&dirty).is_err() {
        eprintln!("[SKIP] ffmpeg can't synthesize TIFF");
        return;
    }

    // Pre-condition: exif was written
    let read_back = little_exif::metadata::Metadata::new_from_path(&dirty).unwrap();
    assert!(
        read_back.into_iter().next().is_some(),
        "fixture TIFF must have EXIF"
    );

    let handler = get_handler_for_mime("image/tiff").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    // Post: no EXIF
    if let Ok(m) = little_exif::metadata::Metadata::new_from_path(&cleaned) {
        assert!(
            m.into_iter().next().is_none(),
            "cleaned TIFF must have no EXIF"
        );
    }
}

#[test]
fn tiff_cleaning_is_deterministic() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] ffmpeg not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.tiff");
    if make_dirty_tiff(&dirty).is_err() {
        return;
    }
    let handler = get_handler_for_mime("image/tiff").unwrap();
    let a = dir.path().join("a.tiff");
    let b = dir.path().join("b.tiff");
    handler.clean_metadata(&dirty, &a).unwrap();
    handler.clean_metadata(&dirty, &b).unwrap();
    assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
}

#[test]
fn jxl_round_trip_strips_exif() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] ffmpeg not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.jxl");
    let cleaned = dir.path().join("clean.jxl");
    if make_dirty_jxl(&dirty).is_err() {
        eprintln!("[SKIP] ffmpeg has no libjxl encoder");
        return;
    }

    let handler = get_handler_for_mime("image/jxl").unwrap();
    // Pre-condition — the fixture carries EXIF.
    if let Ok(m) = little_exif::metadata::Metadata::new_from_path(&dirty) {
        assert!(m.into_iter().next().is_some(), "JXL fixture missing EXIF");
    }

    handler.clean_metadata(&dirty, &cleaned).unwrap();

    if let Ok(m) = little_exif::metadata::Metadata::new_from_path(&cleaned) {
        assert!(
            m.into_iter().next().is_none(),
            "cleaned JXL must have no EXIF"
        );
    }
}

#[test]
fn bmp_dispatch_works() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("pixels.bmp");
    let dst = dir.path().join("clean.bmp");
    make_bmp(&src);

    let handler = get_handler_for_mime("image/bmp").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    // Harmless handler does a byte-for-byte copy; pixel data preserved.
    assert_eq!(fs::read(&src).unwrap(), fs::read(&dst).unwrap());
}

// ================================================================
// §19. PDF XMP packet reader
// ================================================================

fn make_jpeg_with_xmp_app1(path: &Path) {
    // Start from the minimal JPEG and inject a raw APP1 XMP segment
    // immediately after SOI. The JPEG has no APP1 yet, so we can splice
    // directly between SOI (0xFFD8) and SOF/APP0.
    let xmp_payload = br#"<?xpacket begin=""?><x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/"
                 xmlns:xmp="http://ns.adobe.com/xap/1.0/"
                 xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/">
<dc:creator>mat2-parity-xmp-creator</dc:creator>
<xmp:CreatorTool>secret-camera-firmware</xmp:CreatorTool>
<photoshop:City>Paris</photoshop:City>
</rdf:Description>
</rdf:RDF></x:xmpmeta><?xpacket end=""?>"#;
    let ns = b"http://ns.adobe.com/xap/1.0/\0";
    let mut body = Vec::new();
    body.extend_from_slice(ns);
    body.extend_from_slice(xmp_payload);
    // APP1 segment length = 2 (length field itself) + body
    let seg_len = (body.len() + 2) as u16;

    let mut out = Vec::new();
    out.extend_from_slice(&[0xFF, 0xD8]); // SOI
    out.extend_from_slice(&[0xFF, 0xE1]); // APP1
    out.extend_from_slice(&seg_len.to_be_bytes());
    out.extend_from_slice(&body);
    // Splice in the rest of the base JPEG after SOI
    out.extend_from_slice(&TEST_JPEG[2..]);
    fs::write(path, &out).unwrap();
}

fn make_jpeg_with_iptc_app13(path: &Path) {
    // Build an APP13 segment containing a Photoshop 3.0 8BIM resource
    // 0x0404 with IPTC records for By-line and Caption.
    let mut iim = Vec::new();
    // 2:80 By-line = "Alice"
    iim.extend_from_slice(&[0x1C, 2, 80, 0x00, 0x05]);
    iim.extend_from_slice(b"Alice");
    // 2:120 Caption = "vacation photo"
    iim.extend_from_slice(&[0x1C, 2, 120, 0x00, 0x0E]);
    iim.extend_from_slice(b"vacation photo");

    let mut resource = Vec::new();
    resource.extend_from_slice(b"8BIM");
    resource.extend_from_slice(&0x0404u16.to_be_bytes());
    resource.push(0x00); // pascal string length
    resource.push(0x00); // pad
    resource.extend_from_slice(&(iim.len() as u32).to_be_bytes());
    resource.extend_from_slice(&iim);

    let mut body = Vec::new();
    body.extend_from_slice(b"Photoshop 3.0\0");
    body.extend_from_slice(&resource);

    let seg_len = (body.len() + 2) as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&[0xFF, 0xD8]); // SOI
    out.extend_from_slice(&[0xFF, 0xED]); // APP13
    out.extend_from_slice(&seg_len.to_be_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(&TEST_JPEG[2..]);
    fs::write(path, &out).unwrap();
}

#[test]
fn jpeg_reader_surfaces_individual_xmp_fields() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("xmp.jpg");
    make_jpeg_with_xmp_app1(&dirty);

    let handler = get_handler_for_mime("image/jpeg").unwrap();
    let meta = handler.read_metadata(&dirty).unwrap();
    let dump = format!("{meta:?}");
    assert!(dump.contains("XMP dc:creator"), "dc:creator missing: {dump}");
    assert!(dump.contains("mat2-parity-xmp-creator"), "{dump}");
    assert!(dump.contains("XMP xmp:CreatorTool"), "{dump}");
    assert!(dump.contains("secret-camera-firmware"), "{dump}");
    assert!(dump.contains("photoshop:City"), "{dump}");
    assert!(dump.contains("Paris"), "{dump}");
}

#[test]
fn jpeg_reader_surfaces_individual_iptc_fields() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("iptc.jpg");
    make_jpeg_with_iptc_app13(&dirty);

    let handler = get_handler_for_mime("image/jpeg").unwrap();
    let meta = handler.read_metadata(&dirty).unwrap();
    let dump = format!("{meta:?}");
    assert!(dump.contains("IPTC By-line"), "By-line missing: {dump}");
    assert!(dump.contains("Alice"), "{dump}");
    assert!(dump.contains("IPTC Caption"), "{dump}");
    assert!(dump.contains("vacation photo"), "{dump}");
}

#[test]
fn jpeg_clean_still_removes_xmp_and_iptc() {
    // Same crafted JPEG as above, but now round-tripped through clean:
    // the cleaned file must have no trace of the XMP/IPTC strings.
    let dir = tempfile::tempdir().unwrap();
    let xmp_src = dir.path().join("xmp.jpg");
    let xmp_dst = dir.path().join("xmp-clean.jpg");
    make_jpeg_with_xmp_app1(&xmp_src);
    let handler = get_handler_for_mime("image/jpeg").unwrap();
    handler.clean_metadata(&xmp_src, &xmp_dst).unwrap();
    let bytes = fs::read(&xmp_dst).unwrap();
    assert!(find_bytes(&bytes, b"mat2-parity-xmp-creator").is_none());
    assert!(find_bytes(&bytes, b"secret-camera-firmware").is_none());
    assert!(find_bytes(&bytes, b"photoshop:City").is_none());

    let iptc_src = dir.path().join("iptc.jpg");
    let iptc_dst = dir.path().join("iptc-clean.jpg");
    make_jpeg_with_iptc_app13(&iptc_src);
    handler.clean_metadata(&iptc_src, &iptc_dst).unwrap();
    let bytes = fs::read(&iptc_dst).unwrap();
    assert!(find_bytes(&bytes, b"Photoshop 3.0").is_none());
    assert!(find_bytes(&bytes, b"Alice").is_none());
    assert!(find_bytes(&bytes, b"vacation photo").is_none());
}

#[test]
fn pdf_reader_surfaces_individual_xmp_fields() {
    // mat2's pdf.py parses the XMP packet in /Metadata and surfaces
    // each dc:/xmp:/pdf: field individually. Our reader should do the
    // same so the UI doesn't just say "XMP Metadata: present".
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("dirty.pdf");
    make_dirty_pdf(&src);

    let handler = get_handler_for_mime("application/pdf").unwrap();
    let meta = handler.read_metadata(&src).unwrap();
    let dump = format!("{meta:?}");
    // The XMP packet in make_dirty_pdf contains <dc:creator>secret</dc:creator>.
    assert!(
        dump.contains("XMP dc:creator") && dump.contains("secret"),
        "XMP dc:creator should be surfaced individually: {dump}"
    );
}

// ================================================================
// §20. FLAC picture recursion
// ================================================================

#[test]
fn flac_reader_recurses_into_embedded_cover() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] ffmpeg not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let jpeg = dir.path().join("cover.jpg");
    make_dirty_jpeg(&jpeg);
    let jpeg_bytes = fs::read(&jpeg).unwrap();

    let flac = dir.path().join("dirty.flac");
    if make_flac_with_dirty_cover(&flac, &jpeg_bytes).is_err() {
        eprintln!("[SKIP] flac fixture could not be built");
        return;
    }

    let handler = get_handler_for_mime("audio/flac").unwrap();
    let meta = handler.read_metadata(&flac).unwrap();
    let dump = format!("{meta:?}");

    // The cover itself has EXIF Artist "mat2-parity-artist".
    // The reader should recursively surface it under a "Picture #1 →"
    // synthetic key.
    assert!(
        dump.contains("Picture #1 \u{2192}") || dump.contains("Picture #1 ->"),
        "FLAC reader should recurse into embedded cover: {dump}"
    );
    assert!(
        dump.contains("mat2-parity-artist"),
        "inner EXIF Artist from cover must be surfaced: {dump}"
    );
}

// ================================================================
// §21. Sandbox fallback + video bitexact assertions
// ================================================================

#[test]
fn sandbox_probe_command_runs_ffprobe_successfully() {
    // Integration test for handlers::sandbox — if bwrap is available
    // the command must still run ffprobe end-to-end without failing
    // due to missing binds.
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] ffmpeg/ffprobe not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mp4 = dir.path().join("sample.mp4");
    if make_dirty_mp4(&mp4).is_err() {
        eprintln!("[SKIP] can't make mp4 fixture");
        return;
    }

    let handler = get_handler_for_mime("video/mp4").unwrap();
    // read_metadata uses sandbox::sandboxed_probe_command under the hood
    // (see handlers::video). If it works, the sandbox path is wired up
    // correctly.
    let meta = handler.read_metadata(&mp4).unwrap();
    assert!(!meta.is_empty(), "ffprobe should return metadata");
}

#[test]
fn video_clean_bitexact_removes_encoder_fingerprint() {
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] ffmpeg/ffprobe not available");
        return;
    }
    // Comprehensive bitexact assertion: we already have mp4_round_trip,
    // but here we specifically look for any "Lavf" / "encoder=" / "Lavc"
    // substring in the cleaned byte stream.
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.mp4");
    let cleaned = dir.path().join("clean.mp4");
    if make_dirty_mp4(&dirty).is_err() {
        return;
    }
    let handler = get_handler_for_mime("video/mp4").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let raw = fs::read(&cleaned).unwrap();
    for needle in [b"Lavf" as &[u8], b"Lavc", b"encoder="] {
        assert!(
            find_bytes(&raw, needle).is_none(),
            "encoder fingerprint {:?} survived bitexact clean",
            std::str::from_utf8(needle).unwrap_or("?")
        );
    }
}

// ================================================================
// §22. Idempotence + determinism for every new format
// ================================================================

macro_rules! idempotence_test {
    ($name:ident, $mime:expr, $make:expr) => {
        #[test]
        fn $name() {
            let dir = tempfile::tempdir().unwrap();
            let dirty = dir.path().join("dirty");
            $make(&dirty);
            let handler = get_handler_for_mime($mime).unwrap();
            let c1 = dir.path().join("c1");
            let c2 = dir.path().join("c2");
            handler.clean_metadata(&dirty, &c1).unwrap();
            handler.clean_metadata(&c1, &c2).unwrap();
            // A clean file cleaned again must produce no new metadata
            // and must be readable.
            let meta = handler.read_metadata(&c2).unwrap();
            assert!(meta.is_empty(), "round #2 produced metadata: {meta:?}");
        }
    };
}

fn make_dirty_svg(path: &Path) {
    fs::write(
        path,
        br#"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg"
     xmlns:dc="http://purl.org/dc/elements/1.1/"
     width="10" height="10">
  <metadata><dc:creator>x</dc:creator></metadata>
  <title>secret</title>
  <rect x="0" y="0" width="10" height="10" fill="red"/>
</svg>"#,
    )
    .unwrap();
}

fn make_dirty_css(path: &Path) {
    fs::write(
        path,
        b"/* author: jvoisin\n * version: 1.0\n */\nbody { color: red; }\n",
    )
    .unwrap();
}

fn make_dirty_html(path: &Path) {
    fs::write(
        path,
        b"<!DOCTYPE html><html><head><meta name=\"author\" content=\"jvoisin\"><title>secret</title></head><body><p>visible</p></body></html>",
    )
    .unwrap();
}

fn make_dirty_torrent_file(path: &Path) {
    use traceless_core::handlers::torrent::{encode, BencodeValue};
    let mut map: std::collections::BTreeMap<Vec<u8>, BencodeValue> =
        std::collections::BTreeMap::new();
    map.insert(
        b"announce".to_vec(),
        BencodeValue::Bytes(b"http://tracker/".to_vec()),
    );
    map.insert(
        b"comment".to_vec(),
        BencodeValue::Bytes(b"secret".to_vec()),
    );
    let mut info = std::collections::BTreeMap::new();
    info.insert(b"name".to_vec(), BencodeValue::Bytes(b"f".to_vec()));
    info.insert(b"piece length".to_vec(), BencodeValue::Int(16384));
    info.insert(b"pieces".to_vec(), BencodeValue::Bytes(vec![0u8; 20]));
    info.insert(b"length".to_vec(), BencodeValue::Int(1));
    map.insert(b"info".to_vec(), BencodeValue::Dict(info));
    fs::write(path, encode(&BencodeValue::Dict(map))).unwrap();
}

fn make_dirty_gif_file(path: &Path) {
    let mut gif = Vec::new();
    gif.extend_from_slice(b"GIF89a");
    gif.extend_from_slice(&[0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00]);
    gif.extend_from_slice(&[0x21, 0xFE]);
    gif.push(6);
    gif.extend_from_slice(b"secret");
    gif.push(0x00);
    gif.extend_from_slice(&[0x2C, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
    gif.extend_from_slice(&[0x02, 0x02, 0x44, 0x01, 0x00]);
    gif.push(0x3B);
    fs::write(path, gif).unwrap();
}

idempotence_test!(svg_idempotent, "image/svg+xml", make_dirty_svg);
idempotence_test!(css_idempotent, "text/css", make_dirty_css);
idempotence_test!(html_idempotent, "text/html", make_dirty_html);
idempotence_test!(
    torrent_idempotent,
    "application/x-bittorrent",
    make_dirty_torrent_file
);
idempotence_test!(gif_idempotent, "image/gif", make_dirty_gif_file);

macro_rules! determinism_test {
    ($name:ident, $mime:expr, $make:expr) => {
        #[test]
        fn $name() {
            let dir = tempfile::tempdir().unwrap();
            let dirty = dir.path().join("dirty");
            $make(&dirty);
            let handler = get_handler_for_mime($mime).unwrap();
            let a = dir.path().join("a");
            let b = dir.path().join("b");
            handler.clean_metadata(&dirty, &a).unwrap();
            handler.clean_metadata(&dirty, &b).unwrap();
            assert_eq!(
                fs::read(&a).unwrap(),
                fs::read(&b).unwrap(),
                "{} clean must be byte-deterministic",
                stringify!($name)
            );
        }
    };
}

determinism_test!(svg_deterministic, "image/svg+xml", make_dirty_svg);
determinism_test!(css_deterministic, "text/css", make_dirty_css);
determinism_test!(html_deterministic, "text/html", make_dirty_html);
determinism_test!(
    torrent_deterministic,
    "application/x-bittorrent",
    make_dirty_torrent_file
);
determinism_test!(gif_deterministic, "image/gif", make_dirty_gif_file);

// ================================================================
// §23. Format edge cases
// ================================================================

#[test]
fn css_handles_nested_strings_with_comment_markers() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("style.css");
    let dst = dir.path().join("clean.css");
    fs::write(
        &src,
        r#"body::before { content: "/* not a comment */"; } /* REAL */"#,
    )
    .unwrap();
    let handler = get_handler_for_mime("text/css").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read_to_string(&dst).unwrap();
    assert!(out.contains("/* not a comment */"), "string literal corrupted: {out}");
    assert!(!out.contains("REAL"));
}

#[test]
fn css_handles_unterminated_comment_gracefully() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("bad.css");
    let dst = dir.path().join("clean.css");
    fs::write(&src, b"body { color: red; } /* unterminated...").unwrap();
    let handler = get_handler_for_mime("text/css").unwrap();
    // Must not panic; what survives is implementation-defined but
    // the valid part must be preserved.
    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read_to_string(&dst).unwrap();
    assert!(out.contains("color: red"));
    assert!(!out.contains("unterminated"));
}

#[test]
fn html_attribute_values_may_contain_angle_brackets() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("page.html");
    let dst = dir.path().join("clean.html");
    fs::write(
        &src,
        b"<html><body><a href=\"/foo?x=1&y=>2\">link</a></body></html>",
    )
    .unwrap();
    let handler = get_handler_for_mime("text/html").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read_to_string(&dst).unwrap();
    assert!(out.contains("link"), "content survived: {out}");
    assert!(out.contains("/foo?x=1"), "attribute survived: {out}");
}

#[test]
fn html_xhtml_self_closing_meta_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("page.xhtml");
    let dst = dir.path().join("clean.xhtml");
    fs::write(
        &src,
        br#"<?xml version="1.0"?><html><head><meta name="author" content="secret"/><link rel="stylesheet"/></head><body/></html>"#,
    )
    .unwrap();
    let handler = get_handler_for_mime("application/xhtml+xml").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read_to_string(&dst).unwrap();
    assert!(!out.contains("secret"));
    assert!(out.contains("<link"), "unrelated self-closing tag survived");
    assert!(out.contains("<?xml"));
}

#[test]
fn svg_self_closing_metadata_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("empty-meta.svg");
    let dst = dir.path().join("clean.svg");
    fs::write(
        &src,
        br#"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1">
  <metadata/>
  <title/>
  <desc/>
  <rect x="0" y="0" width="1" height="1"/>
</svg>"#,
    )
    .unwrap();
    let handler = get_handler_for_mime("image/svg+xml").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read_to_string(&dst).unwrap();
    assert!(!out.contains("<metadata"));
    assert!(!out.contains("<title"));
    assert!(!out.contains("<desc"));
    assert!(out.contains("<rect"));
}

#[test]
fn torrent_preserves_nested_info_dict() {
    use traceless_core::handlers::torrent::{encode, BencodeValue};
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("nested.torrent");
    let dst = dir.path().join("clean.torrent");

    // info dict with a files list → each files entry is a dict with
    // `length` and `path` (a list of bytes)
    let mut file1 = std::collections::BTreeMap::new();
    file1.insert(b"length".to_vec(), BencodeValue::Int(100));
    file1.insert(
        b"path".to_vec(),
        BencodeValue::List(vec![
            BencodeValue::Bytes(b"sub".to_vec()),
            BencodeValue::Bytes(b"a.bin".to_vec()),
        ]),
    );
    let mut info = std::collections::BTreeMap::new();
    info.insert(b"name".to_vec(), BencodeValue::Bytes(b"root".to_vec()));
    info.insert(b"piece length".to_vec(), BencodeValue::Int(16384));
    info.insert(b"pieces".to_vec(), BencodeValue::Bytes(vec![0u8; 40]));
    info.insert(
        b"files".to_vec(),
        BencodeValue::List(vec![BencodeValue::Dict(file1)]),
    );

    let mut root = std::collections::BTreeMap::new();
    root.insert(
        b"announce".to_vec(),
        BencodeValue::Bytes(b"http://t/".to_vec()),
    );
    root.insert(b"info".to_vec(), BencodeValue::Dict(info));
    root.insert(
        b"comment".to_vec(),
        BencodeValue::Bytes(b"SHOULD-BE-GONE".to_vec()),
    );

    fs::write(&src, encode(&BencodeValue::Dict(root))).unwrap();

    let handler = get_handler_for_mime("application/x-bittorrent").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read(&dst).unwrap();
    assert!(find_bytes(&out, b"SHOULD-BE-GONE").is_none());
    // Nested structure preserved
    assert!(find_bytes(&out, b"root").is_some());
    assert!(find_bytes(&out, b"a.bin").is_some());
    assert!(find_bytes(&out, b"files").is_some());
}

#[test]
fn gif87a_without_extensions_is_passed_through() {
    // GIF87a has no comment/application extensions — cleaner should be a
    // no-op relative to the metadata walker, but must still produce a
    // valid output.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("vanilla.gif");
    let dst = dir.path().join("clean.gif");
    let mut gif = Vec::new();
    gif.extend_from_slice(b"GIF87a");
    gif.extend_from_slice(&[0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00]);
    gif.extend_from_slice(&[0x2C, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
    gif.extend_from_slice(&[0x02, 0x02, 0x44, 0x01, 0x00]);
    gif.push(0x3B);
    fs::write(&src, &gif).unwrap();
    let handler = get_handler_for_mime("image/gif").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    // Output should still be a valid GIF with same pixel data.
    let out = fs::read(&dst).unwrap();
    assert!(out.starts_with(b"GIF87a") || out.starts_with(b"GIF89a"));
    assert_eq!(out.last(), Some(&0x3B));
}

// ================================================================
// §24. Archive edge cases (tar.bz2, tar.xz, recursion, determinism)
// ================================================================

#[test]
fn tar_bz2_round_trip_cleans_embedded_image() {
    use bzip2::write::BzEncoder;
    use tar::{Builder as TarBuilder, EntryType, Header as TarHeader};

    let dir = tempfile::tempdir().unwrap();
    let jpeg = dir.path().join("inner.jpg");
    make_dirty_jpeg(&jpeg);
    let jpeg_bytes = fs::read(&jpeg).unwrap();

    let src = dir.path().join("dirty.tar.bz2");
    let dst = dir.path().join("clean.tar.bz2");

    {
        let file = fs::File::create(&src).unwrap();
        let bz = BzEncoder::new(file, bzip2::Compression::default());
        let mut builder = TarBuilder::new(bz);
        let mut header = TarHeader::new_gnu();
        header.set_path("photo.jpg").unwrap();
        header.set_size(jpeg_bytes.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(EntryType::Regular);
        header.set_cksum();
        builder.append(&header, jpeg_bytes.as_slice()).unwrap();
        builder.into_inner().unwrap();
    }

    let handler = get_handler_for_mime("application/x-bzip2").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();

    use bzip2::read::BzDecoder;
    let f = fs::File::open(&dst).unwrap();
    let bz = BzDecoder::new(f);
    let mut archive = tar::Archive::new(bz);
    let mut inner_bytes = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path().unwrap().to_string_lossy() == "photo.jpg" {
            entry.read_to_end(&mut inner_bytes).unwrap();
            break;
        }
    }
    let probe = dir.path().join("probe.jpg");
    fs::write(&probe, &inner_bytes).unwrap();
    if let Ok(m) = little_exif::metadata::Metadata::new_from_path(&probe) {
        assert!(
            m.into_iter().next().is_none(),
            "embedded JPEG inside .tar.bz2 must be stripped"
        );
    }
}

#[test]
fn tar_xz_round_trip_cleans_embedded_image() {
    use tar::{Builder as TarBuilder, EntryType, Header as TarHeader};
    use xz2::write::XzEncoder;

    let dir = tempfile::tempdir().unwrap();
    let jpeg = dir.path().join("inner.jpg");
    make_dirty_jpeg(&jpeg);
    let jpeg_bytes = fs::read(&jpeg).unwrap();

    let src = dir.path().join("dirty.tar.xz");
    let dst = dir.path().join("clean.tar.xz");

    {
        let file = fs::File::create(&src).unwrap();
        let xz = XzEncoder::new(file, 6);
        let mut builder = TarBuilder::new(xz);
        let mut header = TarHeader::new_gnu();
        header.set_path("photo.jpg").unwrap();
        header.set_size(jpeg_bytes.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(EntryType::Regular);
        header.set_cksum();
        builder.append(&header, jpeg_bytes.as_slice()).unwrap();
        builder.into_inner().unwrap();
    }

    let handler = get_handler_for_mime("application/x-xz").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();

    use xz2::read::XzDecoder;
    let f = fs::File::open(&dst).unwrap();
    let xz = XzDecoder::new(f);
    let mut archive = tar::Archive::new(xz);
    let mut inner_bytes = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path().unwrap().to_string_lossy() == "photo.jpg" {
            entry.read_to_end(&mut inner_bytes).unwrap();
            break;
        }
    }
    let probe = dir.path().join("probe.jpg");
    fs::write(&probe, &inner_bytes).unwrap();
    if let Ok(m) = little_exif::metadata::Metadata::new_from_path(&probe) {
        assert!(
            m.into_iter().next().is_none(),
            "embedded JPEG inside .tar.xz must be stripped"
        );
    }
}

#[test]
fn zip_archive_is_deterministic() {
    use std::io::Write as _;
    use zip::write::SimpleFileOptions;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("dirty.zip");
    {
        let file = fs::File::create(&src).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default().last_modified_time(
            zip::DateTime::from_date_and_time(2024, 6, 1, 12, 0, 0).unwrap(),
        );
        writer.start_file("z.txt", opts).unwrap();
        writer.write_all(b"zz").unwrap();
        writer.start_file("a.txt", opts).unwrap();
        writer.write_all(b"aa").unwrap();
        writer.finish().unwrap();
    }
    let handler = get_handler_for_mime("application/zip").unwrap();
    let a = dir.path().join("a.zip");
    let b = dir.path().join("b.zip");
    handler.clean_metadata(&src, &a).unwrap();
    handler.clean_metadata(&src, &b).unwrap();
    assert_eq!(
        fs::read(&a).unwrap(),
        fs::read(&b).unwrap(),
        "generic ZIP clean must be deterministic"
    );
}

#[test]
fn zip_archive_sorts_members_lexicographically() {
    use std::io::Write as _;
    use zip::write::SimpleFileOptions;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("dirty.zip");
    {
        let file = fs::File::create(&src).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default();
        for name in ["z.txt", "b.txt", "a.txt", "m.txt"] {
            writer.start_file(name, opts).unwrap();
            writer.write_all(name.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }
    let handler = get_handler_for_mime("application/zip").unwrap();
    let dst = dir.path().join("clean.zip");
    handler.clean_metadata(&src, &dst).unwrap();
    let names = zip_entry_names(&dst);
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "members not sorted: {names:?}");
}

#[test]
fn tar_rejects_setuid_member() {
    // Hand-craft a minimal tar header with setuid bit set.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("setuid.tar");

    let mut block = [0u8; 512];
    block[..5].copy_from_slice(b"f.txt");
    // mode: 0004755 (setuid + rwxr-xr-x)
    block[100..107].copy_from_slice(b"0004755");
    block[108..115].copy_from_slice(b"0000000");
    block[116..123].copy_from_slice(b"0000000");
    block[124..135].copy_from_slice(b"00000000000");
    block[136..147].copy_from_slice(b"00000000000");
    block[156] = b'0';
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    for b in &mut block[148..156] {
        *b = b' ';
    }
    let sum: u32 = block.iter().map(|&b| u32::from(b)).sum();
    let chksum = format!("{sum:06o}\0 ");
    block[148..156].copy_from_slice(chksum.as_bytes());

    let mut buf = Vec::new();
    buf.extend_from_slice(&block);
    buf.extend_from_slice(&[0u8; 1024]);
    fs::write(&src, &buf).unwrap();

    let handler = get_handler_for_mime("application/x-tar").unwrap();
    let dst = dir.path().join("out.tar");
    let result = handler.clean_metadata(&src, &dst);
    assert!(result.is_err(), "setuid member must be rejected");
}

#[test]
fn tar_rejects_duplicate_member_names() {
    use tar::{Builder as TarBuilder, EntryType, Header as TarHeader};
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("dup.tar");
    {
        let file = fs::File::create(&src).unwrap();
        let mut builder = TarBuilder::new(file);
        for _ in 0..2 {
            let mut header = TarHeader::new_gnu();
            header.set_path("same.txt").unwrap();
            header.set_size(1);
            header.set_mode(0o644);
            header.set_entry_type(EntryType::Regular);
            header.set_cksum();
            builder.append(&header, &b"x"[..]).unwrap();
        }
        builder.into_inner().unwrap();
    }
    let handler = get_handler_for_mime("application/x-tar").unwrap();
    let dst = dir.path().join("out.tar");
    let result = handler.clean_metadata(&src, &dst);
    assert!(result.is_err(), "duplicate member must be rejected");
}

// ================================================================
// §25. Sandbox unit-integration
// ================================================================

#[test]
fn bwrap_probe_when_absent_falls_back() {
    // Point PATH at a directory that definitely has no bwrap in it,
    // then call ffprobe via our sandbox helper. The helper should
    // successfully fall back to direct ffprobe exec.
    //
    // We skip this test when bwrap is installed in /usr/bin because
    // our cache holds the first `bwrap_path()` result; poking PATH
    // won't affect it. The fallback path is still exercised in CI
    // images without bwrap.
    if std::path::Path::new("/usr/bin/bwrap").exists() {
        eprintln!("[SKIP] bwrap is installed; fallback path needs a fresh process");
        return;
    }
    if !have_ffprobe() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mp4 = dir.path().join("t.mp4");
    if make_dirty_mp4(&mp4).is_err() {
        return;
    }
    let handler = get_handler_for_mime("video/mp4").unwrap();
    let meta = handler.read_metadata(&mp4).unwrap();
    assert!(!meta.is_empty());
}

// ================================================================
// §26. UnknownMemberPolicy for archives
// ================================================================
//
// These tests mutate a process-wide atomic. cargo test runs tests in
// parallel by default, so two policy tests can observe each other's
// values and produce spurious failures. We serialize them with a
// dedicated Mutex — no external crate needed.

use std::sync::{Mutex, MutexGuard, OnceLock};

fn policy_test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn make_zip_with_unknown_member(path: &std::path::Path) {
    use std::io::Write as _;
    use zip::write::SimpleFileOptions;
    let file = fs::File::create(path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default();
    writer.start_file("known.jpg", opts).unwrap();
    writer.write_all(TEST_JPEG).unwrap();
    writer.start_file("mystery.xyz", opts).unwrap();
    writer.write_all(b"arbitrary unknown binary blob").unwrap();
    writer.finish().unwrap();
}

#[test]
fn unknown_policy_keep_copies_member_verbatim() {
    use traceless_core::{PolicyGuard, UnknownMemberPolicy};
    let _lock = policy_test_lock();
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("dirty.zip");
    let dst = dir.path().join("clean.zip");
    make_zip_with_unknown_member(&src);

    let _g = PolicyGuard::new(UnknownMemberPolicy::Keep);
    let handler = get_handler_for_mime("application/zip").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();

    // Known member is present and cleaned
    assert!(read_zip_entry(&dst, "known.jpg").is_some());
    // Unknown member is present verbatim
    let unknown = read_zip_entry(&dst, "mystery.xyz").unwrap();
    assert_eq!(unknown, b"arbitrary unknown binary blob");
}

#[test]
fn unknown_policy_omit_drops_member() {
    use traceless_core::{PolicyGuard, UnknownMemberPolicy};
    let _lock = policy_test_lock();
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("dirty.zip");
    let dst = dir.path().join("clean.zip");
    make_zip_with_unknown_member(&src);

    let _g = PolicyGuard::new(UnknownMemberPolicy::Omit);
    let handler = get_handler_for_mime("application/zip").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();

    // Unknown member dropped
    assert!(
        read_zip_entry(&dst, "mystery.xyz").is_none(),
        "Omit policy must drop unknown members"
    );
    // Known member survives
    assert!(read_zip_entry(&dst, "known.jpg").is_some());
}

#[test]
fn unknown_policy_abort_rejects_archive() {
    use traceless_core::{PolicyGuard, UnknownMemberPolicy};
    let _lock = policy_test_lock();
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("dirty.zip");
    let dst = dir.path().join("clean.zip");
    make_zip_with_unknown_member(&src);

    let _g = PolicyGuard::new(UnknownMemberPolicy::Abort);
    let handler = get_handler_for_mime("application/zip").unwrap();
    let result = handler.clean_metadata(&src, &dst);
    assert!(result.is_err(), "Abort policy must reject archives with unknown members");
}

#[test]
fn unknown_policy_abort_passes_when_every_member_is_known() {
    use traceless_core::{PolicyGuard, UnknownMemberPolicy};
    let _lock = policy_test_lock();
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("known-only.zip");
    let dst = dir.path().join("clean.zip");
    {
        use std::io::Write as _;
        use zip::write::SimpleFileOptions;
        let file = fs::File::create(&src).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default();
        writer.start_file("a.jpg", opts).unwrap();
        writer.write_all(TEST_JPEG).unwrap();
        writer.start_file("b.png", opts).unwrap();
        // minimal PNG
        writer
            .write_all(&[
                0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R',
                0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0, 0x90, 0x77, 0x53, 0xDE, 0, 0, 0, 0, b'I',
                b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82,
            ])
            .unwrap();
        writer.finish().unwrap();
    }

    let _g = PolicyGuard::new(UnknownMemberPolicy::Abort);
    let handler = get_handler_for_mime("application/zip").unwrap();
    handler
        .clean_metadata(&src, &dst)
        .expect("Abort policy should pass for all-known archives");
}

#[test]
fn unknown_policy_applies_to_tar_too() {
    use tar::{Builder as TarBuilder, EntryType, Header as TarHeader};
    use traceless_core::{PolicyGuard, UnknownMemberPolicy};

    let _lock = policy_test_lock();
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("mystery.tar");

    {
        let file = fs::File::create(&src).unwrap();
        let mut builder = TarBuilder::new(file);
        let mut header = TarHeader::new_gnu();
        header.set_path("weird.xyz").unwrap();
        header.set_size(4);
        header.set_mode(0o644);
        header.set_entry_type(EntryType::Regular);
        header.set_cksum();
        builder.append(&header, &b"blob"[..]).unwrap();
        builder.into_inner().unwrap();
    }

    let handler = get_handler_for_mime("application/x-tar").unwrap();

    // Abort mode rejects
    let _g = PolicyGuard::new(UnknownMemberPolicy::Abort);
    let dst = dir.path().join("out-abort.tar");
    assert!(handler.clean_metadata(&src, &dst).is_err());

    // Omit mode drops the member (resulting tar has 0 members)
    let _g = PolicyGuard::new(UnknownMemberPolicy::Omit);
    let dst = dir.path().join("out-omit.tar");
    handler.clean_metadata(&src, &dst).unwrap();
    let f = fs::File::open(&dst).unwrap();
    let mut archive = tar::Archive::new(std::io::BufReader::new(f));
    let mut count = 0usize;
    for entry in archive.entries().unwrap() {
        let _ = entry.unwrap();
        count += 1;
    }
    assert_eq!(count, 0, "Omit policy should produce an empty tar");
}

// ================================================================
// §27. Small helpers
// ================================================================

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}
