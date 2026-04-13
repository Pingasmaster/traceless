use std::fs::File;
use std::io::{BufReader, Cursor, Read, Write};
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use zip::write::SimpleFileOptions;
use zip::ZipArchive;

use crate::error::CoreError;
use crate::metadata::{MetadataGroup, MetadataItem, MetadataSet};

use super::FormatHandler;

pub struct DocumentHandler;

/// Metadata XML paths in OOXML documents (DOCX, XLSX, PPTX).
const OOXML_META_PATHS: &[&str] = &["docProps/core.xml", "docProps/app.xml", "docProps/custom.xml"];

/// Metadata XML paths in ODF documents (ODT, ODS, ODP).
const ODF_META_PATHS: &[&str] = &["meta.xml"];

/// Metadata XML paths in EPUB documents.
const EPUB_META_PATHS: &[&str] = &["content.opf", "OEBPS/content.opf", "OPS/content.opf"];

impl FormatHandler for DocumentHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
        let file = File::open(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        let mut archive = ZipArchive::new(BufReader::new(file)).map_err(|e| CoreError::ParseError {
            path: path.to_path_buf(),
            detail: format!("Not a valid ZIP archive: {e}"),
        })?;

        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut set = MetadataSet::default();

        // Determine which meta paths to check
        let meta_paths = detect_meta_paths(&mut archive);

        for meta_path in meta_paths {
            if let Ok(mut entry) = archive.by_name(meta_path) {
                let mut contents = String::new();
                if entry.read_to_string(&mut contents).is_ok() {
                    let items = parse_xml_metadata(&contents);
                    if !items.is_empty() {
                        set.groups.push(MetadataGroup {
                            filename: format!("{filename}/{meta_path}"),
                            items,
                        });
                    }
                }
            }
        }

        Ok(set)
    }

    fn clean_metadata(
        &self,
        path: &Path,
        output_path: &Path,
    ) -> Result<(), CoreError> {
        let file = File::open(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        let mut archive = ZipArchive::new(BufReader::new(file)).map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("Not a valid ZIP archive: {e}"),
        })?;

        let meta_paths = detect_meta_paths(&mut archive);

        let out_file = File::create(output_path).map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("Failed to create output: {e}"),
        })?;

        let mut writer = zip::ZipWriter::new(out_file);

        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(|e| CoreError::CleanError {
                path: path.to_path_buf(),
                detail: format!("Failed to read ZIP entry: {e}"),
            })?;

            let entry_name = entry.name().to_string();

            if meta_paths.contains(&entry_name.as_str()) {
                // Read the metadata XML
                let mut contents = String::new();
                entry.read_to_string(&mut contents).map_err(|e| CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: format!("Failed to read entry {entry_name}: {e}"),
                })?;

                let cleaned = clean_xml_metadata_full(&contents, &entry_name);

                let options = SimpleFileOptions::default()
                    .compression_method(entry.compression());
                writer
                    .start_file(&entry_name, options)
                    .map_err(|e| CoreError::CleanError {
                        path: path.to_path_buf(),
                        detail: format!("Failed to start ZIP entry: {e}"),
                    })?;
                writer.write_all(cleaned.as_bytes()).map_err(|e| CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: format!("Failed to write ZIP entry: {e}"),
                })?;
            } else {
                // Copy entry as-is
                let options = SimpleFileOptions::default()
                    .compression_method(entry.compression());
                writer
                    .start_file(&entry_name, options)
                    .map_err(|e| CoreError::CleanError {
                        path: path.to_path_buf(),
                        detail: format!("Failed to start ZIP entry: {e}"),
                    })?;
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf).map_err(|e| CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: format!("Failed to read entry: {e}"),
                })?;
                writer.write_all(&buf).map_err(|e| CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: format!("Failed to write entry: {e}"),
                })?;
            }
        }

        writer.finish().map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("Failed to finalize ZIP: {e}"),
        })?;

        Ok(())
    }

    fn supported_mime_types(&self) -> &[&str] {
        &[
            "application/vnd.oasis.opendocument.text",
            "application/vnd.oasis.opendocument.spreadsheet",
            "application/vnd.oasis.opendocument.presentation",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            "application/epub+zip",
        ]
    }
}

fn detect_meta_paths<R: Read + std::io::Seek>(archive: &mut ZipArchive<R>) -> Vec<&'static str> {
    let names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|e| e.name().to_string()))
        .collect();

    let mut paths = Vec::new();

    // Check OOXML
    for p in OOXML_META_PATHS {
        if names.iter().any(|n| n == *p) {
            paths.push(*p);
        }
    }

    // Check ODF
    for p in ODF_META_PATHS {
        if names.iter().any(|n| n == *p) {
            paths.push(*p);
        }
    }

    // Check EPUB
    for p in EPUB_META_PATHS {
        if names.iter().any(|n| n == *p) {
            paths.push(*p);
        }
    }

    paths
}

/// Parse XML content and extract metadata key-value pairs.
fn parse_xml_metadata(xml: &str) -> Vec<MetadataItem> {
    let mut items = Vec::new();
    let mut reader = Reader::from_str(xml);
    let mut current_tag: Option<String> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let local_name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                current_tag = Some(local_name);
            }
            Ok(Event::Text(ref e)) => {
                if let Some(ref tag) = current_tag {
                    let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                    if !text.is_empty() && is_metadata_tag(tag) {
                        items.push(MetadataItem {
                            key: tag.clone(),
                            value: text,
                        });
                    }
                }
            }
            Ok(Event::End(_)) => {
                current_tag = None;
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    items
}

/// Check if an XML tag name represents metadata we want to display.
fn is_metadata_tag(tag: &str) -> bool {
    matches!(
        tag,
        "creator"
            | "title"
            | "subject"
            | "description"
            | "keywords"
            | "lastModifiedBy"
            | "created"
            | "modified"
            | "revision"
            | "category"
            | "Application"
            | "AppVersion"
            | "Company"
            | "Manager"
            | "TotalTime"
            | "Pages"
            | "Words"
            | "Characters"
            | "Paragraphs"
            | "Lines"
            | "initial-creator"
            | "creation-date"
            | "date"
            | "editing-cycles"
            | "editing-duration"
            | "generator"
            | "language"
            | "print-date"
            | "printed-by"
            | "identifier"
            | "rights"
            | "publisher"
            | "contributor"
    )
}

/// Full clean: produce minimal/empty metadata XML.
fn clean_xml_metadata_full(xml: &str, entry_name: &str) -> String {
    if entry_name == "meta.xml" {
        // ODF: minimal meta.xml
        r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
                      xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0"
                      office:version="1.3">
  <office:meta/>
</office:document-meta>"#
            .to_string()
    } else if entry_name == "docProps/core.xml" {
        // OOXML core: minimal
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"
                   xmlns:dc="http://purl.org/dc/elements/1.1/"
                   xmlns:dcterms="http://purl.org/dc/terms/">
</cp:coreProperties>"#
            .to_string()
    } else if entry_name == "docProps/app.xml" {
        // OOXML app: minimal
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties">
</Properties>"#
            .to_string()
    } else if entry_name == "docProps/custom.xml" {
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties">
</Properties>"#
            .to_string()
    } else {
        // EPUB or unknown: try to strip metadata elements, keep structure
        clean_xml_metadata_lightweight(xml)
    }
}

#[cfg(test)]
pub(crate) fn clean_xml_metadata_lightweight_for_tests(xml: &str) -> String {
    clean_xml_metadata_lightweight(xml)
}

const LIGHTWEIGHT_REMOVE_TAGS: &[&str] = &[
    "creator",
    "initial-creator",
    "lastModifiedBy",
    "Company",
    "Manager",
    "Application",
    "AppVersion",
    "generator",
    "printed-by",
];

/// Lightweight clean: remove only creator, date, and tool-related metadata.
///
/// Handles both the paired `<Start>…</End>` case (by suppressing all
/// events while `skip_depth > 0`) and the self-closing `<Empty/>` case
/// (by matching the local name directly and dropping just that event).
fn clean_xml_metadata_lightweight(xml: &str) -> String {
    let mut reader = Reader::from_str(xml);
    let mut writer = quick_xml::Writer::new(Cursor::new(Vec::new()));
    let mut skip_depth: usize = 0;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let local_name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                if skip_depth > 0 || LIGHTWEIGHT_REMOVE_TAGS.contains(&local_name.as_str()) {
                    skip_depth += 1;
                } else {
                    writer.write_event(Event::Start(e.clone())).ok();
                }
            }
            Ok(Event::End(ref e)) => {
                if skip_depth > 0 {
                    skip_depth -= 1;
                } else {
                    writer.write_event(Event::End(e.clone())).ok();
                }
            }
            Ok(Event::Empty(ref e)) => {
                // Self-closing tags (`<meta:generator/>`) don't open a
                // depth; we just drop the single event if it matches.
                if skip_depth > 0 {
                    continue;
                }
                let local_name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                if !LIGHTWEIGHT_REMOVE_TAGS.contains(&local_name.as_str()) {
                    writer.write_event(Event::Empty(e.clone())).ok();
                }
            }
            Ok(Event::Text(ref e)) => {
                if skip_depth == 0 {
                    writer.write_event(Event::Text(e.clone())).ok();
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(other) => {
                if skip_depth == 0 {
                    writer.write_event(other).ok();
                }
            }
        }
    }

    String::from_utf8(writer.into_inner().into_inner()).unwrap_or_else(|_| xml.to_string())
}
