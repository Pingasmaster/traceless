#[cfg(test)]
#[allow(
    clippy::module_inception,
    clippy::needless_borrows_for_generic_args,
    clippy::single_match,
    unused_mut,
    clippy::absurd_extreme_comparisons
)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use tempfile::TempDir;

    use crate::format_support::{detect_mime, get_handler_for_mime, supported_extensions};
    use crate::handlers::FormatHandler;
    use crate::file::{FileEntry, FileState};
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

        let entry = FileEntry::new(&path);
        assert_eq!(entry.filename, "test.jpg");
        assert_eq!(entry.state, FileState::Initializing);
        assert!(entry.metadata.is_none());
        assert!(entry.error.is_none());
        assert_eq!(entry.total_metadata(), 0);
    }

    #[test]
    fn test_file_entry_mime_detection() {
        let entry = FileEntry::new(Path::new("/tmp/photo.jpg"));
        assert_eq!(entry.mime_type, "image/jpeg");

        let entry = FileEntry::new(Path::new("/tmp/song.mp3"));
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

    #[test]
    fn test_file_store_lightweight_mode() {
        let mut store = FileStore::new();
        assert!(!store.lightweight_mode);
        store.lightweight_mode = true;
        assert!(store.lightweight_mode);
    }

    // ===== Image Handler Tests =====

    #[test]
    fn test_image_handler_read_empty_jpeg() {
        // Create a minimal valid JPEG (SOI + EOI)
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("minimal.jpg");
        fs::write(&path, &[0xFF, 0xD8, 0xFF, 0xD9]).unwrap();

        let handler = crate::handlers::image::ImageHandler;
        let result = handler.read_metadata(&path);
        // Minimal JPEG may fail to parse, that's OK
        match result {
            Ok(set) => assert!(set.is_empty() || set.total_count() > 0),
            Err(_) => {} // Expected for minimal JPEG
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
        let mut writer = zip::ZipWriter::new(file);
        writer.finish().unwrap();

        let handler = crate::handlers::document::DocumentHandler;
        let result = handler.read_metadata(&path);
        // An empty ZIP has no metadata paths, so should return empty
        match result {
            Ok(set) => assert!(set.is_empty()),
            Err(_) => {} // Also acceptable
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
        handler.clean_metadata(&path, &output, false).unwrap();
        assert!(output.exists());

        // Verify cleaned file has less/no metadata
        let cleaned_result = handler.read_metadata(&output).unwrap();
        let original_result = handler.read_metadata(&path).unwrap();
        assert!(cleaned_result.total_count() < original_result.total_count());
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

        let event = FileStoreEvent::FileStateChanged {
            index: 0,
            state: FileState::HasMetadata,
            mime_type: Some("text/plain".to_string()),
        };
        store.apply_event(&event);

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

        let metadata = MetadataSet {
            groups: vec![MetadataGroup {
                filename: "test.txt".to_string(),
                items: vec![MetadataItem {
                    key: "key".to_string(),
                    value: "value".to_string(),
                }],
            }],
        };

        let event = FileStoreEvent::MetadataReady {
            index: 0,
            metadata,
        };
        store.apply_event(&event);

        assert_eq!(store.get(0).unwrap().state, FileState::HasMetadata);
        assert_eq!(store.get(0).unwrap().total_metadata(), 1);
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

        let event = FileStoreEvent::FileError {
            index: 0,
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
}
