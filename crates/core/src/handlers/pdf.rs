use std::path::Path;

use lopdf::Document;

use crate::error::CoreError;
use crate::metadata::{MetadataGroup, MetadataItem, MetadataSet};

use super::FormatHandler;

pub struct PdfHandler;

/// Keys commonly found in the PDF /Info dictionary.
const INFO_KEYS: &[&str] = &[
    "Author",
    "Title",
    "Subject",
    "Keywords",
    "Creator",
    "Producer",
    "CreationDate",
    "ModDate",
    "Trapped",
];

impl FormatHandler for PdfHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
        let doc = Document::load(path).map_err(|e| CoreError::ParseError {
            path: path.to_path_buf(),
            detail: format!("Failed to load PDF: {e}"),
        })?;

        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut items = Vec::new();

        // Read /Info dictionary from trailer
        if let Ok(info_ref) = doc.trailer.get(b"Info")
            && let Ok(obj_ref) = info_ref.as_reference()
            && let Ok(info_obj) = doc.get_object(obj_ref)
            && let Ok(dict) = info_obj.as_dict()
        {
            for key in INFO_KEYS {
                if let Ok(val) = dict.get(key.as_bytes()) {
                    let value_str = pdf_object_to_string(val);
                    if !value_str.is_empty() {
                        items.push(MetadataItem {
                            key: key.to_string(),
                            value: value_str,
                        });
                    }
                }
            }
            // Also grab any non-standard keys
            for (k, v) in dict {
                let key_str = String::from_utf8_lossy(k).to_string();
                if !INFO_KEYS.contains(&key_str.as_str()) {
                    let value_str = pdf_object_to_string(v);
                    if !value_str.is_empty() {
                        items.push(MetadataItem {
                            key: key_str,
                            value: value_str,
                        });
                    }
                }
            }
        }

        // Check for XMP metadata stream in catalog
        if let Ok(catalog) = doc.catalog()
            && catalog.has(b"Metadata")
        {
            items.push(MetadataItem {
                key: "XMP Metadata".to_string(),
                value: "present".to_string(),
            });
        }

        let mut set = MetadataSet::default();
        if !items.is_empty() {
            set.groups.push(MetadataGroup {
                filename,
                items,
            });
        }
        Ok(set)
    }

    fn clean_metadata(
        &self,
        path: &Path,
        output_path: &Path,
    ) -> Result<(), CoreError> {
        let mut doc = Document::load(path).map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("Failed to load PDF: {e}"),
        })?;

        // Remove /Info dictionary entirely
        if let Ok(info_ref) = doc.trailer.get(b"Info")
            && let Ok(obj_ref) = info_ref.as_reference()
        {
            doc.delete_object(obj_ref);
            doc.trailer.remove(b"Info");
        }

        // Remove the XMP metadata stream and the catalog's /Metadata
        // pointer to it. Pull the object id out under a read-only borrow
        // of the catalog first so the later delete_object + catalog_mut
        // don't need to juggle overlapping mutable borrows.
        let meta_ref = doc
            .catalog()
            .ok()
            .and_then(|cat| cat.get(b"Metadata").ok().cloned())
            .and_then(|obj| obj.as_reference().ok());
        if let Some(obj_ref) = meta_ref {
            doc.delete_object(obj_ref);
            if let Ok(catalog) = doc.catalog_mut() {
                catalog.remove(b"Metadata");
            }
        }

        doc.save(output_path).map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("Failed to save PDF: {e}"),
        })?;

        Ok(())
    }

    fn supported_mime_types(&self) -> &[&str] {
        &["application/pdf"]
    }
}

fn pdf_object_to_string(obj: &lopdf::Object) -> String {
    match obj {
        lopdf::Object::String(bytes, _) => String::from_utf8_lossy(bytes).to_string(),
        lopdf::Object::Name(name) => String::from_utf8_lossy(name).to_string(),
        lopdf::Object::Integer(n) => n.to_string(),
        lopdf::Object::Real(n) => n.to_string(),
        lopdf::Object::Boolean(b) => b.to_string(),
        _ => format!("{obj:?}"),
    }
}
