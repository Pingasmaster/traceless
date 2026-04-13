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
    assert!(get_handler_for_mime(
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    ).is_some());
}

#[test]
fn test_handler_for_unsupported() {
    assert!(get_handler_for_mime("application/octet-stream").is_none());
    assert!(get_handler_for_mime("text/plain").is_none());
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

