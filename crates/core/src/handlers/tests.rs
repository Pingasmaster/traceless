use std::fs;
use std::io::Write;
use std::path::Path;
use tempfile::TempDir;

use crate::format_support::{detect_mime, get_handler_for_mime, supported_extensions};
use crate::handlers::FormatHandler;
use crate::file::{FileEntry, FileId, FileState};
use crate::file_store::FileStore;
use crate::metadata::MetadataSet;

// ===== Format Support Tests =====

#[test]
fn test_detect_mime_jpeg() {
    assert_eq!(detect_mime(Path::new("photo.jpg")), "image/jpeg");
    assert_eq!(detect_mime(Path::new("photo.jpeg")), "image/jpeg");
}

#[test]
fn test_detect_mime_png() {
    assert_eq!(detect_mime(Path::new("image.png")), "image/png");
}

#[test]
fn test_detect_mime_webp() {
    assert_eq!(detect_mime(Path::new("image.webp")), "image/webp");
}

#[test]
fn test_detect_mime_pdf() {
    assert_eq!(detect_mime(Path::new("document.pdf")), "application/pdf");
}

#[test]
fn test_detect_mime_mp3() {
    assert_eq!(detect_mime(Path::new("song.mp3")), "audio/mpeg");
}

#[test]
fn test_detect_mime_flac() {
    let mime = detect_mime(Path::new("song.flac"));
    assert!(
        mime == "audio/flac" || mime == "audio/x-flac",
        "Expected audio/flac or audio/x-flac, got: {mime}"
    );
}

#[test]
fn test_detect_mime_mp4_video() {
    assert_eq!(detect_mime(Path::new("video.mp4")), "video/mp4");
}

#[test]
fn test_detect_mime_docx() {
    let mime = detect_mime(Path::new("doc.docx"));
    assert!(
        mime.contains("officedocument") || mime.contains("zip"),
        "DOCX should be detected as office document or zip, got: {mime}"
    );
}

#[test]
fn test_detect_mime_unknown() {
    assert_eq!(
        detect_mime(Path::new("file.xyz123")),
        "application/octet-stream"
    );
}

#[test]
fn test_handler_for_jpeg() {
    assert!(get_handler_for_mime("image/jpeg").is_some());
}

#[test]
fn test_handler_for_png() {
    assert!(get_handler_for_mime("image/png").is_some());
}

#[test]
fn test_handler_for_pdf() {
    assert!(get_handler_for_mime("application/pdf").is_some());
}

#[test]
fn test_handler_for_audio() {
    assert!(get_handler_for_mime("audio/mpeg").is_some());
    assert!(get_handler_for_mime("audio/flac").is_some());
    assert!(get_handler_for_mime("audio/ogg").is_some());
}

#[test]
fn test_handler_for_video() {
    assert!(get_handler_for_mime("video/mp4").is_some());
    assert!(get_handler_for_mime("video/x-matroska").is_some());
}

#[test]
fn test_handler_for_documents() {
    assert!(get_handler_for_mime("application/vnd.oasis.opendocument.text").is_some());
    assert!(get_handler_for_mime("application/vnd.oasis.opendocument.graphics").is_some());
    assert!(get_handler_for_mime(
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    ).is_some());
}

#[test]
fn test_handler_for_unsupported() {
    assert!(get_handler_for_mime("application/octet-stream").is_none());
    // text/plain is now handled by HarmlessHandler (harmless copy).
    assert!(get_handler_for_mime("application/x-unknown-gibberish").is_none());
}

#[test]
fn test_handler_for_harmless() {
    assert!(get_handler_for_mime("text/plain").is_some());
    assert!(get_handler_for_mime("image/bmp").is_some());
    assert!(get_handler_for_mime("image/x-portable-pixmap").is_some());
}

#[test]
fn test_supported_extensions_not_empty() {
    let exts = supported_extensions();
    assert!(!exts.is_empty());
    assert!(exts.contains(&"jpg"));
    assert!(exts.contains(&"pdf"));
    assert!(exts.contains(&"mp3"));
    assert!(exts.contains(&"docx"));
    assert!(exts.contains(&"mp4"));
}

// ===== FileState Tests =====

#[test]
fn test_file_state_simple_state() {
    assert_eq!(FileState::Initializing.simple_state(), "working");
    assert_eq!(FileState::CheckingMetadata.simple_state(), "working");
    assert_eq!(FileState::RemovingMetadata.simple_state(), "working");
    assert_eq!(FileState::Unsupported.simple_state(), "error");
    assert_eq!(FileState::ErrorWhileInitializing.simple_state(), "error");
    assert_eq!(FileState::ErrorWhileCheckingMetadata.simple_state(), "error");
    assert_eq!(FileState::ErrorWhileRemovingMetadata.simple_state(), "error");
    assert_eq!(FileState::HasNoMetadata.simple_state(), "warning");
    assert_eq!(FileState::HasMetadata.simple_state(), "has-metadata");
    assert_eq!(FileState::Cleaned.simple_state(), "clean");
}

#[test]
fn test_file_state_is_cleanable() {
    assert!(FileState::HasMetadata.is_cleanable());
    assert!(FileState::HasNoMetadata.is_cleanable());
    assert!(!FileState::Initializing.is_cleanable());
    assert!(!FileState::Cleaned.is_cleanable());
    assert!(!FileState::Unsupported.is_cleanable());
}

#[test]
fn test_file_state_is_working() {
    assert!(FileState::Initializing.is_working());
    assert!(FileState::CheckingMetadata.is_working());
    assert!(FileState::RemovingMetadata.is_working());
    assert!(!FileState::HasMetadata.is_working());
    assert!(!FileState::Cleaned.is_working());
}

// ===== FileEntry Tests =====

#[test]
fn test_file_entry_new() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jpg");
    fs::write(&path, b"dummy").unwrap();

    let entry = FileEntry::new(FileId(1), &path);
    assert_eq!(entry.filename, "test.jpg");
    assert_eq!(entry.state, FileState::Initializing);
    assert!(entry.metadata.is_none());
    assert!(entry.error.is_none());
    assert_eq!(entry.total_metadata(), 0);
    assert_eq!(entry.id, FileId(1));
}

#[test]
fn test_file_entry_mime_detection() {
    let entry = FileEntry::new(FileId(1), Path::new("/tmp/photo.jpg"));
    assert_eq!(entry.mime_type, "image/jpeg");

    let entry = FileEntry::new(FileId(2), Path::new("/tmp/song.mp3"));
    assert_eq!(entry.mime_type, "audio/mpeg");
}

// ===== MetadataSet Tests =====

#[test]
fn test_metadata_set_empty() {
    let set = MetadataSet::default();
    assert!(set.is_empty());
    assert_eq!(set.total_count(), 0);
}

#[test]
fn test_metadata_set_with_items() {
    use crate::metadata::{MetadataGroup, MetadataItem};
    let set = MetadataSet {
        groups: vec![MetadataGroup {
            filename: "test.jpg".to_string(),
            items: vec![
                MetadataItem {
                    key: "Author".to_string(),
                    value: "John".to_string(),
                },
                MetadataItem {
                    key: "Date".to_string(),
                    value: "2024-01-01".to_string(),
                },
            ],
        }],
    };
    assert!(!set.is_empty());
    assert_eq!(set.total_count(), 2);
}

#[test]
fn test_metadata_set_multiple_groups() {
    use crate::metadata::{MetadataGroup, MetadataItem};
    let set = MetadataSet {
        groups: vec![
            MetadataGroup {
                filename: "file1.xml".to_string(),
                items: vec![MetadataItem {
                    key: "a".to_string(),
                    value: "b".to_string(),
                }],
            },
            MetadataGroup {
                filename: "file2.xml".to_string(),
                items: vec![
                    MetadataItem {
                        key: "c".to_string(),
                        value: "d".to_string(),
                    },
                    MetadataItem {
                        key: "e".to_string(),
                        value: "f".to_string(),
                    },
                ],
            },
        ],
    };
    assert_eq!(set.total_count(), 3);
}

// ===== FileStore Tests =====

#[test]
fn test_file_store_new() {
    let store = FileStore::new();
    assert!(store.is_empty());
    assert_eq!(store.len(), 0);
    assert_eq!(store.cleanable_count(), 0);
    assert_eq!(store.cleaned_count(), 0);
    assert!(!store.has_working());
}

#[test]
fn test_file_store_default() {
    let store = FileStore::default();
    assert!(store.is_empty());
}

#[test]
fn test_file_store_add_and_remove() {
    let dir = TempDir::new().unwrap();
    let path1 = dir.path().join("a.txt");
    let path2 = dir.path().join("b.txt");
    fs::write(&path1, b"hello").unwrap();
    fs::write(&path2, b"world").unwrap();

    let mut store = FileStore::new();
    let (tx, _rx) = async_channel::unbounded();
    store.add_files(vec![path1, path2], &tx);

    assert_eq!(store.len(), 2);
    assert!(!store.is_empty());

    store.remove_file(0);
    assert_eq!(store.len(), 1);

    store.clear();
    assert!(store.is_empty());
}

// ===== Image Handler Tests =====

#[test]
fn test_image_handler_read_empty_jpeg() {
    // Create a minimal valid JPEG (SOI + EOI)
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("minimal.jpg");
    fs::write(&path, [0xFF, 0xD8, 0xFF, 0xD9]).unwrap();

    let handler = crate::handlers::image::ImageHandler;
    // Minimal JPEG may fail to parse or produce an empty set; if it succeeds,
    // total_count must be consistent with the actual groups/items vector.
    if let Ok(set) = handler.read_metadata(&path) {
        let sum: usize = set.groups.iter().map(|g| g.items.len()).sum();
        assert_eq!(set.total_count(), sum);
    }
}

#[test]
fn test_image_handler_read_nonexistent() {
    let handler = crate::handlers::image::ImageHandler;
    let result = handler.read_metadata(Path::new("/nonexistent/photo.jpg"));
    assert!(result.is_err());
}

#[test]
fn test_image_handler_supported_types() {
    let handler = crate::handlers::image::ImageHandler;
    let types = handler.supported_mime_types();
    assert!(types.contains(&"image/jpeg"));
    assert!(types.contains(&"image/png"));
    assert!(types.contains(&"image/webp"));
}

// ===== PDF Handler Tests =====

#[test]
fn test_pdf_handler_read_nonexistent() {
    let handler = crate::handlers::pdf::PdfHandler;
    let result = handler.read_metadata(Path::new("/nonexistent/doc.pdf"));
    assert!(result.is_err());
}

#[test]
fn test_pdf_handler_read_invalid() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("invalid.pdf");
    fs::write(&path, b"not a pdf").unwrap();

    let handler = crate::handlers::pdf::PdfHandler;
    let result = handler.read_metadata(&path);
    assert!(result.is_err());
}

#[test]
fn test_pdf_handler_supported_types() {
    let handler = crate::handlers::pdf::PdfHandler;
    let types = handler.supported_mime_types();
    assert!(types.contains(&"application/pdf"));
}

// ===== Audio Handler Tests =====

#[test]
fn test_audio_handler_read_nonexistent() {
    let handler = crate::handlers::audio::AudioHandler;
    let result = handler.read_metadata(Path::new("/nonexistent/song.mp3"));
    assert!(result.is_err());
}

#[test]
fn test_audio_handler_read_invalid() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("invalid.mp3");
    fs::write(&path, b"not an mp3").unwrap();

    let handler = crate::handlers::audio::AudioHandler;
    let result = handler.read_metadata(&path);
    assert!(result.is_err());
}

#[test]
fn test_audio_handler_supported_types() {
    let handler = crate::handlers::audio::AudioHandler;
    let types = handler.supported_mime_types();
    assert!(types.contains(&"audio/mpeg"));
    assert!(types.contains(&"audio/flac"));
    assert!(types.contains(&"audio/ogg"));
}

// ===== Document Handler Tests =====

#[test]
fn test_document_handler_read_nonexistent() {
    let handler = crate::handlers::document::DocumentHandler;
    let result = handler.read_metadata(Path::new("/nonexistent/doc.docx"));
    assert!(result.is_err());
}

#[test]
fn test_document_handler_read_invalid_zip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("invalid.docx");
    fs::write(&path, b"not a zip").unwrap();

    let handler = crate::handlers::document::DocumentHandler;
    let result = handler.read_metadata(&path);
    assert!(result.is_err());
}

#[test]
fn test_document_handler_read_empty_zip() {
    // Create a minimal valid ZIP file
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("empty.docx");
    let file = fs::File::create(&path).unwrap();
    let writer = zip::ZipWriter::new(file);
    writer.finish().unwrap();

    let handler = crate::handlers::document::DocumentHandler;
    // An empty ZIP has no metadata paths; if it parses, the set must be empty.
    if let Ok(set) = handler.read_metadata(&path) {
        assert!(set.is_empty());
    }
}

#[test]
fn test_document_handler_read_odt_with_metadata() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.odt");
    let file = fs::File::create(&path).unwrap();
    let mut writer = zip::ZipWriter::new(file);

    let meta_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
                  xmlns:dc="http://purl.org/dc/elements/1.1/"
                  xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0">
  <office:meta>
<dc:creator>Test Author</dc:creator>
<meta:creation-date>2024-01-01T00:00:00</meta:creation-date>
<meta:generator>TestSuite</meta:generator>
  </office:meta>
</office:document-meta>"#;

    let options = zip::write::SimpleFileOptions::default();
    writer.start_file("meta.xml", options).unwrap();
    writer.write_all(meta_xml.as_bytes()).unwrap();
    writer.start_file("content.xml", options).unwrap();
    writer.write_all(b"<office:document-content/>").unwrap();
    writer.finish().unwrap();

    let handler = crate::handlers::document::DocumentHandler;
    let result = handler.read_metadata(&path).unwrap();
    assert!(!result.is_empty());
    assert!(result.total_count() >= 2);
}

#[test]
fn test_document_handler_clean_odt() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.odt");
    let output = dir.path().join("cleaned.odt");

    let file = fs::File::create(&path).unwrap();
    let mut writer = zip::ZipWriter::new(file);

    let meta_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
                  xmlns:dc="http://purl.org/dc/elements/1.1/"
                  xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0">
  <office:meta>
<dc:creator>Secret Author</dc:creator>
<meta:generator>SecretTool</meta:generator>
  </office:meta>
</office:document-meta>"#;

    let options = zip::write::SimpleFileOptions::default();
    writer.start_file("meta.xml", options).unwrap();
    writer.write_all(meta_xml.as_bytes()).unwrap();
    writer.start_file("content.xml", options).unwrap();
    writer.write_all(b"<office:document-content/>").unwrap();
    writer.finish().unwrap();

    let handler = crate::handlers::document::DocumentHandler;

    // Full clean
    handler.clean_metadata(&path, &output).unwrap();
    assert!(output.exists());

    // Verify cleaned file has less/no metadata
    let cleaned_result = handler.read_metadata(&output).unwrap();
    let original_result = handler.read_metadata(&path).unwrap();
    assert!(cleaned_result.total_count() < original_result.total_count());
}

#[test]
fn test_document_handler_clean_xml_lightweight_drops_empty_tags() {
    // Self-closing metadata tags must be dropped by the lightweight
    // XML cleaner (F9). The OOXML producers that blank a single field
    // typically collapse it to `<Manager/>` rather than removing the
    // element, and the legacy cleaner walked past those verbatim.
    use crate::handlers::document::clean_xml_metadata_lightweight_for_tests;

    let xml = r#"<?xml version="1.0"?>
<Properties>
  <Manager/>
  <Application>LibreOffice</Application>
  <Company>Evil Corp</Company>
  <AppVersion/>
  <Keeper>kept</Keeper>
</Properties>"#;
    let out = clean_xml_metadata_lightweight_for_tests(xml);
    assert!(!out.contains("Manager"), "empty <Manager/> should be removed, got: {out}");
    assert!(!out.contains("Application"), "open/close Application should be removed");
    assert!(!out.contains("Company"), "empty <Company/> variant handling must still drop Company");
    assert!(!out.contains("AppVersion"), "empty <AppVersion/> should be removed");
    assert!(out.contains("Keeper"), "unrelated elements must survive");
    assert!(out.contains("kept"), "unrelated text nodes must survive");
}

#[test]
fn test_document_handler_supported_types() {
    let handler = crate::handlers::document::DocumentHandler;
    let types = handler.supported_mime_types();
    assert!(types.contains(&"application/vnd.oasis.opendocument.text"));
    assert!(types.contains(
        &"application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    ));
}

// ===== Deep-clean coverage tests (mat2 parity) =====
//
// These verify the P0 silent-leak fixes: recursive media cleaning, rsid/
// tracked-change / comment-ref stripping, junk-file omission, ODF
// thumbnail removal, EPUB UUID regeneration, and deterministic ZIP
// output.

/// Build an OOXML-like DOCX with embedded rsid fingerprints, tracked
/// changes, comment anchors, a thumbnail, and an embedded dirty JPEG.
fn build_fake_docx(path: &std::path::Path, jpeg_bytes: &[u8]) {
    let file = fs::File::create(path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();

    writer.start_file("[Content_Types].xml", options).unwrap();
    writer.write_all(b"<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>").unwrap();

    writer.start_file("docProps/core.xml", options).unwrap();
    writer.write_all(br#"<?xml version="1.0"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"
                   xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:creator>Secret Author</dc:creator>
  <cp:lastModifiedBy>Alice</cp:lastModifiedBy>
</cp:coreProperties>"#).unwrap();

    writer.start_file("word/document.xml", options).unwrap();
    writer.write_all(br#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" mc:Ignorable="w14">
  <w:body>
    <w:p w:rsidR="00112233" w:rsidRDefault="00AABBCC">
      <w:commentRangeStart w:id="1"/>
      <w:r><w:t>visible</w:t></w:r>
      <w:commentRangeEnd w:id="1"/>
      <w:commentReference w:id="1"/>
    </w:p>
    <w:p>
      <w:del w:id="2" w:author="bob"><w:r><w:t>removed-secret</w:t></w:r></w:del>
      <w:ins w:id="3" w:author="alice"><w:r><w:t>inserted-ok</w:t></w:r></w:ins>
    </w:p>
  </w:body>
</w:document>"#).unwrap();

    writer.start_file("customXml/item1.xml", options).unwrap();
    writer.write_all(b"<junk>should be dropped</junk>").unwrap();

    writer.start_file("word/comments.xml", options).unwrap();
    writer.write_all(b"<junk>should also be dropped</junk>").unwrap();

    writer.start_file("word/media/image1.jpeg", options).unwrap();
    writer.write_all(jpeg_bytes).unwrap();

    writer.finish().unwrap();
}

/// Read an entry from a cleaned zip archive, returning None if absent.
fn read_entry(zip_path: &std::path::Path, entry: &str) -> Option<Vec<u8>> {
    let file = fs::File::open(zip_path).ok()?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).ok()?;
    let mut e = archive.by_name(entry).ok()?;
    let mut buf = Vec::new();
    e.read_to_end(&mut buf).ok()?;
    Some(buf)
}

use std::io::BufReader;
use std::io::Read as _;

#[test]
fn test_docx_deep_clean_strips_rsid_revisions_and_junk() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("doc.docx");
    let output = dir.path().join("cleaned.docx");

    // Use a minimal valid JPEG (no EXIF) for this particular test; the
    // EXIF-recursion check has its own test below.
    build_fake_docx(&path, TEST_JPEG);

    let handler = crate::handlers::document::DocumentHandler;
    handler.clean_metadata(&path, &output).unwrap();

    // core.xml must be empty-stubbed
    let core = String::from_utf8(read_entry(&output, "docProps/core.xml").unwrap()).unwrap();
    assert!(!core.contains("Secret Author"), "creator must be gone: {core}");
    assert!(!core.contains("Alice"), "lastModifiedBy must be gone");

    // document.xml must have rsid / tracked changes / comment refs gone
    let doc = String::from_utf8(read_entry(&output, "word/document.xml").unwrap()).unwrap();
    assert!(!doc.contains("rsidR"), "rsid attribute leaked: {doc}");
    assert!(!doc.contains("rsidRDefault"), "rsid attribute leaked");
    assert!(!doc.contains("removed-secret"), "w:del content leaked: {doc}");
    assert!(!doc.contains("w:del"), "w:del wrapper leaked");
    assert!(doc.contains("inserted-ok"), "w:ins children must survive: {doc}");
    assert!(!doc.contains("commentRangeStart"), "comment range leaked");
    assert!(!doc.contains("commentReference"), "comment ref leaked");
    assert!(!doc.contains("mc:Ignorable"), "mc:Ignorable leaked: {doc}");
    assert!(doc.contains("visible"), "regular text must survive: {doc}");

    // junk files must be gone
    assert!(
        read_entry(&output, "customXml/item1.xml").is_none(),
        "customXml/item1.xml must be omitted"
    );
    assert!(
        read_entry(&output, "word/comments.xml").is_none(),
        "word/comments.xml must be omitted"
    );
}

#[test]
fn test_docx_embedded_jpeg_exif_is_stripped() {
    // Create a dirty JPEG on disk, embed it, clean the DOCX, verify the
    // EXIF is gone from the inner copy. This is the P0 §1.1 fix — the
    // previous implementation copied embedded images verbatim.
    let dir = TempDir::new().unwrap();
    let jpeg_path = dir.path().join("dirty.jpg");
    make_jpeg_with_exif(&jpeg_path);
    let dirty_bytes = fs::read(&jpeg_path).unwrap();

    let docx_path = dir.path().join("doc.docx");
    let output = dir.path().join("cleaned.docx");
    build_fake_docx(&docx_path, &dirty_bytes);

    let handler = crate::handlers::document::DocumentHandler;
    handler.clean_metadata(&docx_path, &output).unwrap();

    let cleaned_jpeg = read_entry(&output, "word/media/image1.jpeg").unwrap();
    // Write to a temp file so little_exif can parse it; then assert empty.
    let temp_jpeg = dir.path().join("probe.jpg");
    fs::write(&temp_jpeg, &cleaned_jpeg).unwrap();
    if let Ok(m) = little_exif::metadata::Metadata::new_from_path(&temp_jpeg) {
        assert!(
            m.into_iter().next().is_none(),
            "embedded JPEG must be stripped of EXIF"
        );
    }
}

#[test]
fn test_docx_with_unparseable_png_errors_instead_of_leaking() {
    // Regression test for the "silent dirty-bytes fallthrough" bug:
    // when `strip_embedded_image` could not parse an embedded image the
    // old code returned the original bytes via `.unwrap_or(raw)` and
    // shipped them into the cleaned document. That defeated the whole
    // point of the embedded-image cleaner. The fix is to propagate a
    // `CleanError`, which this test asserts.
    let dir = TempDir::new().unwrap();
    let docx_path = dir.path().join("doc.docx");
    let output = dir.path().join("cleaned.docx");

    {
        let file = fs::File::create(&docx_path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();

        writer.start_file("[Content_Types].xml", options).unwrap();
        writer.write_all(b"<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>").unwrap();

        writer.start_file("docProps/core.xml", options).unwrap();
        writer
            .write_all(b"<?xml version=\"1.0\"?><cp:coreProperties xmlns:cp=\"x\"/>")
            .unwrap();

        writer.start_file("word/document.xml", options).unwrap();
        writer
            .write_all(b"<?xml version=\"1.0\"?><w:document xmlns:w=\"x\"><w:body/></w:document>")
            .unwrap();

        // Garbage bytes with a `.png` extension: img-parts cannot parse
        // them, so `strip_embedded_image` returns `None`. Before the fix
        // the clean path copied these through silently; now it must
        // surface a `CleanError`.
        writer.start_file("word/media/image1.png", options).unwrap();
        writer.write_all(b"not a real png, just random bytes").unwrap();

        writer.finish().unwrap();
    }

    let handler = crate::handlers::document::DocumentHandler;
    let result = handler.clean_metadata(&docx_path, &output);
    assert!(
        result.is_err(),
        "DOCX clean must fail when an embedded PNG cannot be parsed; \
         otherwise the dirty bytes would be shipped into the cleaned archive"
    );
    let err_string = format!("{}", result.unwrap_err());
    assert!(
        err_string.contains("image1.png") || err_string.contains("image/png"),
        "error must name the offending embedded image: {err_string}"
    );
}

#[test]
fn test_docx_clean_is_deterministic() {
    // Two cleans of the same input, a moment apart, must produce
    // byte-identical output. This catches regressions in ZIP member
    // time/uid/creator normalization.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("doc.docx");
    build_fake_docx(&path, TEST_JPEG);

    let a = dir.path().join("a.docx");
    let b = dir.path().join("b.docx");

    let handler = crate::handlers::document::DocumentHandler;
    handler.clean_metadata(&path, &a).unwrap();
    handler.clean_metadata(&path, &b).unwrap();

    let ba = fs::read(&a).unwrap();
    let bb = fs::read(&b).unwrap();
    assert_eq!(
        ba, bb,
        "cleaned DOCX output must be deterministic across runs"
    );
}

#[test]
fn test_odf_drops_thumbnails_and_tracked_changes() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("doc.odt");
    let output = dir.path().join("cleaned.odt");

    let file = fs::File::create(&path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();

    writer.start_file("mimetype", options).unwrap();
    writer.write_all(b"application/vnd.oasis.opendocument.text").unwrap();

    writer.start_file("meta.xml", options).unwrap();
    writer.write_all(br#"<office:document-meta xmlns:office="o"><dc:creator xmlns:dc="d">Secret</dc:creator></office:document-meta>"#).unwrap();

    writer.start_file("content.xml", options).unwrap();
    writer.write_all(br#"<office:document-content xmlns:office="o" xmlns:text="t">
        <office:body><office:text>
            <text:tracked-changes><text:p>deleted</text:p></text:tracked-changes>
            <text:p>kept</text:p>
        </office:text></office:body>
    </office:document-content>"#).unwrap();

    writer.start_file("Thumbnails/thumbnail.png", options).unwrap();
    writer.write_all(b"fake thumbnail").unwrap();

    writer.start_file("Configurations2/accelerator/current.xml", options).unwrap();
    writer.write_all(b"<junk/>").unwrap();

    writer.finish().unwrap();

    let handler = crate::handlers::document::DocumentHandler;
    handler.clean_metadata(&path, &output).unwrap();

    assert!(read_entry(&output, "meta.xml").is_none(), "meta.xml must be dropped");
    assert!(
        read_entry(&output, "Thumbnails/thumbnail.png").is_none(),
        "Thumbnails/ must be dropped"
    );
    assert!(
        read_entry(&output, "Configurations2/accelerator/current.xml").is_none(),
        "Configurations2/ must be dropped"
    );

    let content = String::from_utf8(read_entry(&output, "content.xml").unwrap()).unwrap();
    assert!(!content.contains("tracked-changes"), "tracked-changes must be gone: {content}");
    assert!(!content.contains("deleted"), "deleted text must be gone");
    assert!(content.contains("kept"), "kept text must survive");
}

#[test]
fn test_epub_regenerates_uuid_and_rejects_encryption() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("book.epub");
    let output = dir.path().join("cleaned.epub");

    let file = fs::File::create(&path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();

    writer.start_file("mimetype", options).unwrap();
    writer.write_all(b"application/epub+zip").unwrap();
    writer.start_file("META-INF/container.xml", options).unwrap();
    writer.write_all(br#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"#).unwrap();
    writer.start_file("content.opf", options).unwrap();
    writer.write_all(br#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Secret Book</dc:title>
    <dc:creator>Jane Doe</dc:creator>
    <dc:identifier>old-id-leaking-info</dc:identifier>
  </metadata>
  <manifest/>
</package>"#).unwrap();
    writer.finish().unwrap();

    let handler = crate::handlers::document::DocumentHandler;
    handler.clean_metadata(&path, &output).unwrap();

    let opf = String::from_utf8(read_entry(&output, "content.opf").unwrap()).unwrap();
    assert!(!opf.contains("Jane Doe"), "author must be removed: {opf}");
    assert!(!opf.contains("Secret Book"), "title must be removed");
    assert!(!opf.contains("old-id-leaking-info"), "old identifier must be gone");
    assert!(opf.contains("urn:uuid:"), "fresh UUID must be present: {opf}");

    // Second EPUB: encrypted — must fail
    let enc_path = dir.path().join("enc.epub");
    let enc_output = dir.path().join("enc-cleaned.epub");
    let file = fs::File::create(&enc_path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    writer.start_file("mimetype", options).unwrap();
    writer.write_all(b"application/epub+zip").unwrap();
    writer.start_file("META-INF/encryption.xml", options).unwrap();
    writer.write_all(b"<encryption/>").unwrap();
    writer.finish().unwrap();

    let result = handler.clean_metadata(&enc_path, &enc_output);
    assert!(result.is_err(), "encrypted EPUB must be rejected");
}

// ===== Video Handler Tests =====

#[test]
fn test_video_handler_supported_types() {
    let handler = crate::handlers::video::VideoHandler;
    let types = handler.supported_mime_types();
    assert!(types.contains(&"video/mp4"));
    assert!(types.contains(&"video/x-matroska"));
    assert!(types.contains(&"video/webm"));
}

#[test]
fn test_video_handler_read_nonexistent() {
    let handler = crate::handlers::video::VideoHandler;
    let result = handler.read_metadata(Path::new("/nonexistent/video.mp4"));
    assert!(result.is_err());
}

// ===== Error Type Tests =====

#[test]
fn test_core_error_display() {
    use crate::error::CoreError;
    use std::path::PathBuf;

    let err = CoreError::UnsupportedFormat {
        mime_type: "text/plain".to_string(),
    };
    assert!(err.to_string().contains("text/plain"));

    let err = CoreError::NotFound {
        path: PathBuf::from("/foo/bar"),
    };
    assert!(err.to_string().contains("/foo/bar"));

    let err = CoreError::ToolNotFound {
        tool: "ffmpeg".to_string(),
    };
    assert!(err.to_string().contains("ffmpeg"));
}

// ===== FileStore Event Tests =====

#[test]
fn test_file_store_apply_event_state_changed() {
    use crate::file_store::FileStoreEvent;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.txt");
    fs::write(&path, b"hello").unwrap();

    let mut store = FileStore::new();
    let (tx, _rx) = async_channel::unbounded();
    store.add_files(vec![path], &tx);
    let id = store.get(0).unwrap().id;

    let event = FileStoreEvent::FileStateChanged {
        id,
        state: FileState::HasMetadata,
        mime_type: Some("text/plain".to_string()),
    };
    let pos = store.apply_event(&event);
    assert_eq!(pos, Some(0));

    assert_eq!(store.get(0).unwrap().state, FileState::HasMetadata);
    assert_eq!(store.get(0).unwrap().mime_type, "text/plain");
}

#[test]
fn test_file_store_apply_event_metadata_ready() {
    use crate::file_store::FileStoreEvent;
    use crate::metadata::{MetadataGroup, MetadataItem};

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.txt");
    fs::write(&path, b"hello").unwrap();

    let mut store = FileStore::new();
    let (tx, _rx) = async_channel::unbounded();
    store.add_files(vec![path], &tx);
    let id = store.get(0).unwrap().id;

    let metadata = MetadataSet {
        groups: vec![MetadataGroup {
            filename: "test.txt".to_string(),
            items: vec![MetadataItem {
                key: "key".to_string(),
                value: "value".to_string(),
            }],
        }],
    };

    let event = FileStoreEvent::MetadataReady { id, metadata };
    store.apply_event(&event);

    assert_eq!(store.get(0).unwrap().state, FileState::HasMetadata);
    assert_eq!(store.get(0).unwrap().total_metadata(), 1);
}

#[test]
fn test_file_store_apply_event_stale_id_dropped() {
    // A stale event for a removed file is silently ignored. This is the
    // core invariant behind F4: worker events survive `remove_file` calls
    // without corrupting the surviving rows.
    use crate::file_store::FileStoreEvent;

    let dir = TempDir::new().unwrap();
    let p1 = dir.path().join("a.txt");
    let p2 = dir.path().join("b.txt");
    fs::write(&p1, b"a").unwrap();
    fs::write(&p2, b"b").unwrap();

    let mut store = FileStore::new();
    let (tx, _rx) = async_channel::unbounded();
    store.add_files(vec![p1, p2], &tx);
    let stale_id = store.get(0).unwrap().id;
    let surviving_id = store.get(1).unwrap().id;

    // Remove row 0; the stale_id no longer corresponds to any row.
    store.remove_file(0);
    assert_eq!(store.len(), 1);
    assert_eq!(store.get(0).unwrap().id, surviving_id);

    // Sending an event for the removed id must not touch the survivor.
    let event = FileStoreEvent::FileStateChanged {
        id: stale_id,
        state: FileState::Cleaned,
        mime_type: None,
    };
    let pos = store.apply_event(&event);
    assert_eq!(pos, None, "stale id must produce None");
    assert_ne!(
        store.get(0).unwrap().state,
        FileState::Cleaned,
        "stale event must not clobber surviving row"
    );
}

#[test]
fn test_file_store_apply_event_error() {
    use crate::file_store::FileStoreEvent;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.txt");
    fs::write(&path, b"hello").unwrap();

    let mut store = FileStore::new();
    let (tx, _rx) = async_channel::unbounded();
    store.add_files(vec![path], &tx);
    let id = store.get(0).unwrap().id;

    let event = FileStoreEvent::FileError {
        id,
        state: FileState::ErrorWhileCheckingMetadata,
        message: "test error".to_string(),
    };
    store.apply_event(&event);

    assert_eq!(
        store.get(0).unwrap().state,
        FileState::ErrorWhileCheckingMetadata
    );
    assert_eq!(store.get(0).unwrap().error.as_deref(), Some("test error"));
}

// ===== Integration: Directory Scanning =====

#[test]
fn test_file_store_add_directory() {
    let dir = TempDir::new().unwrap();
    let sub = dir.path().join("subdir");
    fs::create_dir(&sub).unwrap();

    fs::write(dir.path().join("a.txt"), b"a").unwrap();
    fs::write(dir.path().join("b.txt"), b"b").unwrap();
    fs::write(sub.join("c.txt"), b"c").unwrap();

    let mut store = FileStore::new();
    let (tx, _rx) = async_channel::unbounded();

    // Non-recursive: should find 2 files
    store.add_directory(dir.path(), false, &tx);
    assert_eq!(store.len(), 2);

    store.clear();

    // Recursive: should find 3 files
    store.add_directory(dir.path(), true, &tx);
    assert_eq!(store.len(), 3);
}

// ===== End-to-end clean path tests =====
//
// These exist specifically to catch the temp-file-extension regression
// (F1 in the audit): little_exif, lofty and ffmpeg all dispatch on the
// output file extension, so the temp-path helper in `file_store` must
// preserve it. The tests drive the full FileStore → handler → temp-file
// → rename path; they do not call the handler directly.

/// 4×4 red JPEG produced by `convert -size 4x4 xc:red`. Small, valid,
/// containing only a JFIF APP0 marker — any EXIF we add is stripable.
const TEST_JPEG: &[u8] = &[
    0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00, 0x01,
    0x00, 0x01, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x03, 0x02, 0x02, 0x02, 0x02, 0x02, 0x03,
    0x02, 0x02, 0x02, 0x03, 0x03, 0x03, 0x03, 0x04, 0x06, 0x04, 0x04, 0x04, 0x04, 0x04, 0x08, 0x06,
    0x06, 0x05, 0x06, 0x09, 0x08, 0x0A, 0x0A, 0x09, 0x08, 0x09, 0x09, 0x0A, 0x0C, 0x0F, 0x0C, 0x0A,
    0x0B, 0x0E, 0x0B, 0x09, 0x09, 0x0D, 0x11, 0x0D, 0x0E, 0x0F, 0x10, 0x10, 0x11, 0x10, 0x0A, 0x0C,
    0x12, 0x13, 0x12, 0x10, 0x13, 0x0F, 0x10, 0x10, 0x10, 0xFF, 0xDB, 0x00, 0x43, 0x01, 0x03, 0x03,
    0x03, 0x04, 0x03, 0x04, 0x08, 0x04, 0x04, 0x08, 0x10, 0x0B, 0x09, 0x0B, 0x10, 0x10, 0x10, 0x10,
    0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10,
    0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10,
    0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0xFF, 0xC0,
    0x00, 0x11, 0x08, 0x00, 0x04, 0x00, 0x04, 0x03, 0x01, 0x11, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11,
    0x01, 0xFF, 0xC4, 0x00, 0x14, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0xFF, 0xC4, 0x00, 0x14, 0x10, 0x01, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xC4, 0x00,
    0x15, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x07, 0x09, 0xFF, 0xC4, 0x00, 0x14, 0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xDA, 0x00, 0x0C, 0x03, 0x01,
    0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3F, 0x00, 0x3A, 0x03, 0x15, 0x4D, 0xFF, 0xD9,
];

/// Pump events from the worker channel until `predicate` returns true
/// or `max_events` have been processed. Fails the test on timeout.
fn drain_until(
    store: &mut FileStore,
    rx: &async_channel::Receiver<crate::file_store::FileStoreEvent>,
    max_events: usize,
    predicate: impl Fn(&FileStore) -> bool,
) {
    for _ in 0..max_events {
        if predicate(store) {
            return;
        }
        match rx.recv_blocking() {
            Ok(event) => {
                store.apply_event(&event);
            }
            Err(_) => break,
        }
    }
    assert!(predicate(store), "predicate not satisfied after {max_events} events");
}

/// Writes the embedded test JPEG, adds an Artist EXIF tag via `little_exif`,
/// and returns the on-disk path. Verifies EXIF was actually embedded.
fn make_jpeg_with_exif(path: &std::path::Path) {
    use little_exif::exif_tag::ExifTag;
    use little_exif::metadata::Metadata as ExifMetadata;

    fs::write(path, TEST_JPEG).unwrap();

    let mut exif = ExifMetadata::new();
    exif.set_tag(ExifTag::Artist("audit-test".to_string()));
    exif.write_to_file(path).unwrap();

    // Sanity: reading the file back should see the tag.
    let read_back = ExifMetadata::new_from_path(path).unwrap();
    assert!(
        read_back.into_iter().next().is_some(),
        "pre-condition: EXIF should be present before cleaning"
    );
}

#[test]
fn test_clean_jpeg_full_end_to_end() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("photo.jpg");
    make_jpeg_with_exif(&path);

    let mut store = FileStore::new();
    let (tx, rx) = async_channel::unbounded();
    store.add_files(vec![path.clone()], &tx);

    drain_until(&mut store, &rx, 20, |s| {
        matches!(
            s.get(0).map(|f| f.state),
            Some(FileState::HasMetadata | FileState::HasNoMetadata)
        )
    });

    store.clean_files(&tx);
    drain_until(&mut store, &rx, 20, |s| {
        s.get(0).map(|f| f.state) == Some(FileState::Cleaned)
    });

    // Original path still exists and now carries no EXIF.
    assert!(path.exists(), "cleaned file must be at the original path");
    // `Err` from little_exif means "no parseable EXIF segment", which is
    // also an acceptable cleaned state.
    if let Ok(m) = little_exif::metadata::Metadata::new_from_path(&path) {
        assert!(
            m.into_iter().next().is_none(),
            "cleaned JPEG must have no EXIF tags"
        );
    }
}

// ===== Non-UTF-8 XML member tests (regression for the stub-bypass bug) =====

/// Encode an ASCII string as UTF-16 LE with a leading byte-order mark.
/// Used by the non-UTF-8 regression tests below.
fn utf16_le_bom(ascii: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for ch in ascii.chars() {
        let mut buf = [0u16; 2];
        let units = ch.encode_utf16(&mut buf);
        for &u in units.iter() {
            out.extend_from_slice(&u.to_le_bytes());
        }
    }
    out
}

#[test]
fn test_document_clean_replaces_non_utf8_core_xml_with_stub() {
    // A DOCX whose `docProps/core.xml` is UTF-16 LE with a BOM used to
    // slip through the cleaner entirely because `clean_entry` returned
    // `Ok(raw)` on a non-UTF-8 decode. The fix routes stub paths ahead
    // of the decode so a hostile `core.xml` still gets replaced with
    // the empty property stub, regardless of encoding.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("stub-bypass.docx");
    let output = dir.path().join("cleaned.docx");

    let core_xml_utf16 = utf16_le_bom(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:creator>LeakedAuthor</dc:creator></cp:coreProperties>"#,
    );

    {
        let file = fs::File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();

        writer.start_file("[Content_Types].xml", options).unwrap();
        writer
            .write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>")
            .unwrap();
        writer.start_file("docProps/core.xml", options).unwrap();
        writer.write_all(&core_xml_utf16).unwrap();
        writer.start_file("word/document.xml", options).unwrap();
        writer
            .write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body/></w:document>")
            .unwrap();
        writer.finish().unwrap();
    }

    let handler = crate::handlers::document::DocumentHandler;
    handler.clean_metadata(&path, &output).unwrap();

    // Extract the cleaned archive and look at docProps/core.xml
    // directly. We can't string-search the raw archive bytes because
    // the member is deflate-compressed, so the needle bytes only
    // survive verbatim inside the decompressed member body.
    let cleaned_file = fs::File::open(&output).unwrap();
    let mut cleaned_zip = zip::ZipArchive::new(cleaned_file).unwrap();
    let mut core_xml = Vec::new();
    {
        use std::io::Read;
        let mut entry = cleaned_zip.by_name("docProps/core.xml").unwrap();
        entry.read_to_end(&mut core_xml).unwrap();
    }

    // The leaked author name must not appear in the member body in
    // any encoding: UTF-8 (would mean the cleaner copied the stub AND
    // the original), UTF-16 LE (would mean the raw bytes were shipped
    // through unchanged).
    let has_utf8 = core_xml.windows(12).any(|w| w == b"LeakedAuthor");
    let utf16_needle = utf16_le_bom("LeakedAuthor");
    let has_utf16 = core_xml
        .windows(utf16_needle.len() - 2)
        .any(|w| w == &utf16_needle[2..]);
    assert!(
        !has_utf8 && !has_utf16,
        "cleaned core.xml still contains the leaked author name: {core_xml:?}"
    );

    // The stub must be in place: the cleaned archive must contain the
    // minimal cp:coreProperties element emitted by `ooxml::CORE_STUB`.
    let as_str = String::from_utf8_lossy(&core_xml);
    assert!(
        as_str.contains("<cp:coreProperties"),
        "cleaned core.xml must carry the coreProperties stub, got: {as_str}"
    );
}

#[test]
fn test_document_clean_rejects_non_utf8_non_stub_xml() {
    // A non-stub XML member (e.g. a `.rels` file) with invalid UTF-8
    // bytes must produce a hard `CleanError` instead of being silently
    // passed through. We don't try to recover; we bail and let the
    // caller surface the error.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bad-rels.docx");
    let output = dir.path().join("cleaned.docx");

    {
        let file = fs::File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();

        writer.start_file("[Content_Types].xml", options).unwrap();
        writer
            .write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>")
            .unwrap();
        // A .rels file is XML-like, so it hits the XML branch, but it
        // is NOT a stub path, so the cleaner must refuse to ship it.
        // 0xFF / 0xFE / 0xFD are all invalid UTF-8 lead bytes.
        writer
            .start_file("_rels/.rels", options)
            .unwrap();
        writer.write_all(&[0xFFu8, 0xFE, 0xFD, 0x00, 0xFF]).unwrap();
        writer.start_file("word/document.xml", options).unwrap();
        writer
            .write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body/></w:document>")
            .unwrap();
        writer.finish().unwrap();
    }

    let handler = crate::handlers::document::DocumentHandler;
    let result = handler.clean_metadata(&path, &output);
    assert!(
        result.is_err(),
        "cleaner must refuse non-UTF-8 non-stub XML members"
    );
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("not valid UTF-8"),
        "error message should mention UTF-8, got: {msg}"
    );
}

// ===== WebP XMP chunk regression tests =====

/// Build a minimum-viable WebP RIFF container with a trailing `XMP `
/// chunk containing `xmp_body`. The VP8L chunk body is a placeholder -
/// img-parts 0.4 treats it as opaque bytes for parse/encode, so a
/// syntactically valid-but-semantically-nonsense body is enough to
/// exercise the metadata code path without pulling in a real WebP
/// encoder. Used by the two tests below.
fn write_webp_with_xmp(path: &std::path::Path, xmp_body: &[u8]) {
    let vp8l_body: &[u8] = &[0x2F, 0x00, 0x00, 0x00, 0x00, 0x00];

    let mut inner = Vec::new();
    inner.extend_from_slice(b"WEBP");

    // VP8L chunk
    inner.extend_from_slice(b"VP8L");
    inner.extend_from_slice(&(u32::try_from(vp8l_body.len()).unwrap()).to_le_bytes());
    inner.extend_from_slice(vp8l_body);
    if vp8l_body.len() % 2 == 1 {
        inner.push(0);
    }

    // XMP chunk
    inner.extend_from_slice(b"XMP ");
    inner.extend_from_slice(&(u32::try_from(xmp_body.len()).unwrap()).to_le_bytes());
    inner.extend_from_slice(xmp_body);
    if xmp_body.len() % 2 == 1 {
        inner.push(0);
    }

    // RIFF wrapper: the size field is the number of bytes after it,
    // which is 4 ("WEBP") + payload chunks. `inner` already starts
    // with "WEBP" so `inner.len()` is the correct value.
    let mut buf = Vec::new();
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(u32::try_from(inner.len()).unwrap()).to_le_bytes());
    buf.extend_from_slice(&inner);

    fs::write(path, buf).unwrap();
}

#[test]
fn test_webp_xmp_chunk_is_stripped() {
    // Regression for the img-parts 0.4 gap: `DynImage::set_exif` +
    // `set_icc_profile` only clear WebP's EXIF and ICCP chunks; the
    // XMP chunk is left intact. A WebP exported from Lightroom /
    // Photoshop / Affinity carries its author, instance-id, GPS, etc.
    // in that chunk and used to sail through `clean_metadata`.
    let dir = TempDir::new().unwrap();
    let dirty = dir.path().join("dirty.webp");
    let cleaned = dir.path().join("clean.webp");

    let xmp = br#"<?xpacket begin='' id=''?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description><dc:creator xmlns:dc="http://purl.org/dc/elements/1.1/">SECRET_XMP_PAYLOAD</dc:creator></rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end=''?>"#;
    write_webp_with_xmp(&dirty, xmp);

    // Sanity: the dirty fixture really does contain the payload.
    let raw = fs::read(&dirty).unwrap();
    assert!(
        raw.windows(18).any(|w| w == b"SECRET_XMP_PAYLOAD"),
        "fixture precondition: dirty WebP must contain the XMP payload"
    );
    assert!(
        raw.windows(4).any(|w| w == b"XMP "),
        "fixture precondition: dirty WebP must contain the XMP chunk id"
    );

    // Clean
    let handler = crate::handlers::image::ImageHandler;
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    // The chunk id and its body must both be gone from the cleaned
    // output. The `XMP ` chunk id is unique to XMP in WebP so we can
    // scan bytes directly without worrying about false positives.
    let out = fs::read(&cleaned).unwrap();
    assert!(
        !out.windows(18).any(|w| w == b"SECRET_XMP_PAYLOAD"),
        "WebP XMP payload survived clean"
    );
    assert!(
        !out.windows(4).any(|w| w == b"XMP "),
        "WebP XMP chunk id survived clean"
    );
    // The output must still be a structurally valid WebP.
    assert!(out.starts_with(b"RIFF"));
    assert_eq!(&out[8..12], b"WEBP");
}

#[test]
fn test_webp_reader_surfaces_xmp_fields() {
    // The reader must flag WebP XMP so the user sees what the cleaner
    // is about to strip. The JPEG reader already does this via its
    // APP1 walk; Bug 9's fix extends the same treatment to WebP.
    let dir = TempDir::new().unwrap();
    let dirty = dir.path().join("dirty.webp");

    let xmp = br#"<?xpacket begin=''?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description><dc:creator xmlns:dc="http://purl.org/dc/elements/1.1/">webp-reader-author</dc:creator></rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end=''?>"#;
    write_webp_with_xmp(&dirty, xmp);

    let handler = crate::handlers::image::ImageHandler;
    let meta = handler.read_metadata(&dirty).unwrap();
    let dump = format!("{meta:?}");
    assert!(
        dump.contains("webp-reader-author"),
        "WebP reader must surface dc:creator from XMP chunk, got: {dump}"
    );
}

#[test]
fn test_docx_embedded_webp_xmp_is_stripped() {
    // Same bug vector, reached via the document handler's
    // `strip_embedded_image` path. A DOCX that embeds a `.webp`
    // inside `word/media/` must have the inner WebP's XMP chunk
    // stripped exactly like the standalone image case.
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    let dir = TempDir::new().unwrap();
    let inner_webp = dir.path().join("inner.webp");
    let xmp = br#"<?xpacket begin=''?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description><dc:creator xmlns:dc="http://purl.org/dc/elements/1.1/">DOCX_WEBP_SECRET</dc:creator></rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end=''?>"#;
    write_webp_with_xmp(&inner_webp, xmp);
    let webp_bytes = fs::read(&inner_webp).unwrap();

    let src = dir.path().join("dirty.docx");
    let dst = dir.path().join("cleaned.docx");

    {
        let file = fs::File::create(&src).unwrap();
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default();

        writer.start_file("[Content_Types].xml", options).unwrap();
        writer
            .write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>")
            .unwrap();
        writer.start_file("word/document.xml", options).unwrap();
        writer
            .write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body/></w:document>")
            .unwrap();
        writer.start_file("word/media/image1.webp", options).unwrap();
        writer.write_all(&webp_bytes).unwrap();
        writer.finish().unwrap();
    }

    let handler = crate::handlers::document::DocumentHandler;
    handler.clean_metadata(&src, &dst).unwrap();

    // Pull the cleaned WebP back out of the archive and byte-scan it.
    let cleaned_file = fs::File::open(&dst).unwrap();
    let mut cleaned_zip = zip::ZipArchive::new(cleaned_file).unwrap();
    let mut cleaned_webp = Vec::new();
    {
        use std::io::Read;
        let mut entry = cleaned_zip.by_name("word/media/image1.webp").unwrap();
        entry.read_to_end(&mut cleaned_webp).unwrap();
    }

    assert!(
        !cleaned_webp.windows(16).any(|w| w == b"DOCX_WEBP_SECRET"),
        "embedded WebP XMP payload survived docx clean"
    );
    assert!(
        !cleaned_webp.windows(4).any(|w| w == b"XMP "),
        "embedded WebP XMP chunk id survived docx clean"
    );
    // Output must still be a structurally valid WebP.
    assert!(cleaned_webp.starts_with(b"RIFF"));
    assert_eq!(&cleaned_webp[8..12], b"WEBP");
}

// Bug 14 regression: the non-JPEG reader branch used to gate the
// "EXIF data: present" fallback line on `items.is_empty()`. An ICC
// chunk pushed earlier in the same pass made `items` non-empty and
// silently masked the EXIF-present line, so a WebP / PNG / TIFF /
// HEIF / JXL carrying both ICC and EXIF appeared to the reader as
// "ICC only" even though the cleaner would still strip the EXIF.
// The fix tracks whether `little_exif` surfaced any concrete tags
// in a dedicated bool and gates the fallback on that instead. The
// logic is factored out into `generic_dynimage_lines` so we can
// exercise every combination directly, without depending on the
// exact interplay of two parser libraries on a synthetic fixture.

#[test]
fn test_image_reader_reports_exif_when_icc_is_also_present() {
    use crate::handlers::image::generic_dynimage_lines;

    // ICC and EXIF both present, little_exif surfaced nothing.
    // Both generic lines must fire.
    let (icc, exif) = generic_dynimage_lines(true, true, false);
    let icc = icc.expect("ICC line must fire when an ICC chunk is present");
    assert_eq!(icc.key, "ICC Profile");
    let exif =
        exif.expect("EXIF line must fire when EXIF is present and no concrete tags were surfaced");
    assert_eq!(exif.key, "EXIF data");
}

#[test]
fn test_image_reader_suppresses_generic_exif_when_tags_already_surfaced() {
    use crate::handlers::image::generic_dynimage_lines;

    // little_exif already surfaced individual tags, so the
    // generic fallback would be redundant and must stay suppressed.
    // ICC is orthogonal and must still fire.
    let (icc, exif) = generic_dynimage_lines(true, true, true);
    assert!(
        icc.is_some(),
        "ICC line must fire even when little_exif surfaced tags"
    );
    assert!(
        exif.is_none(),
        "EXIF fallback must not duplicate concrete tags"
    );
}

#[test]
fn test_image_reader_icc_only() {
    use crate::handlers::image::generic_dynimage_lines;

    // ICC only, no EXIF at all. Only the ICC line fires.
    let (icc, exif) = generic_dynimage_lines(true, false, false);
    assert!(icc.is_some());
    assert!(exif.is_none());
}

#[test]
fn test_image_reader_exif_only() {
    use crate::handlers::image::generic_dynimage_lines;

    // EXIF only (not parseable by little_exif), no ICC.
    // Only the EXIF fallback fires.
    let (icc, exif) = generic_dynimage_lines(false, true, false);
    assert!(icc.is_none());
    assert!(exif.is_some());
}

#[test]
fn test_image_reader_empty_when_nothing_present() {
    use crate::handlers::image::generic_dynimage_lines;

    let (icc, exif) = generic_dynimage_lines(false, false, false);
    assert!(icc.is_none());
    assert!(exif.is_none());
}

#[test]
fn test_file_store_handles_large_batch_without_panic() {
    // Regression for the unbounded-thread-spawn bug: FileStore used to
    // spawn one OS thread per added path. A user dropping a few
    // thousand files (a photo library, a Downloads folder) hit
    // `RLIMIT_NPROC` and panicked the calling thread via
    // `thread::spawn`'s internal `expect("failed to spawn thread")`,
    // crashing the frontend. The fix submits jobs to a shared worker
    // pool bounded at `min(available_parallelism(), 8)`, so a 300-
    // file batch now queues 300 jobs behind 8-or-fewer workers instead
    // of opening 300 thread handles at once.
    let dir = TempDir::new().unwrap();
    let mut paths = Vec::with_capacity(300);
    for i in 0..300 {
        let p = dir.path().join(format!("note{i:03}.txt"));
        fs::write(&p, format!("note {i}\n").as_bytes()).unwrap();
        paths.push(p);
    }

    let mut store = FileStore::new();
    let (tx, rx) = async_channel::unbounded();
    store.add_files(paths, &tx);

    // add_files must return immediately after enqueuing the jobs,
    // not panic, and every entry must be in the store.
    assert_eq!(store.len(), 300);

    // Drain events until every file reaches a terminal state. The
    // text/plain handler reports `HasNoMetadata` so all 300 end up
    // in non-working states. Cap the drain at ~20 events per file
    // so a regression that leaves files stuck in `Working` shows up
    // as a test failure, not a hang.
    drain_until(&mut store, &rx, 6000, |s| {
        s.files().iter().all(|f| !f.state.is_working())
    });

    assert_eq!(store.len(), 300);
    assert!(
        store.files().iter().all(|f| !f.state.is_working()),
        "every file must reach a terminal state"
    );
}
