use std::path::Path;

use lopdf::{Document, Object, ObjectId};

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

/// Catalog-level keys that carry metadata, JavaScript actions, or embedded
/// files. Anything listed here is removed from the root catalog before save.
///
/// References:
/// - ISO 32000-1 §7.7.2 (Document Catalog)
/// - mat2 does the equivalent by rasterizing every page; we instead delete
///   the subtrees directly since we're not re-rendering.
const CATALOG_KEYS_TO_STRIP: &[&[u8]] = &[
    b"Metadata",       // XMP stream
    b"OpenAction",     // Can contain JavaScript
    b"AA",             // Additional-actions (trigger-based JS/events)
    b"AcroForm",       // Form fields, signature fields, XFA
    b"StructTreeRoot", // Accessibility tree — leaks author-assigned alt text
    b"MarkInfo",       // Marked-content properties (producer fingerprint)
    b"PieceInfo",      // Producer-specific caches (Word, Acrobat, LO)
    b"PageLabels",     // Author-chosen page labeling
    b"Outlines",       // Bookmarks — author navigation intent
    b"Threads",        // Article threads
    b"SpiderInfo",     // Web-capture metadata
    b"Perms",          // Permissions/usage rights
    b"Legal",          // Legal-attestation dict
    b"Requirements",   // Viewer-requirements
    b"Collection",     // Portable-collection metadata
    b"Lang",           // Document language
    b"URI",            // Base URI
    b"NeedsRendering", // XFA flag
];

/// Names dict entries that carry author-chosen names, embedded files, or
/// scripted behavior. The dict itself may still be needed for legit page
/// labels, so we strip just these children.
const NAMES_KEYS_TO_STRIP: &[&[u8]] = &[
    b"EmbeddedFiles",
    b"JavaScript",
    b"AP",             // Appearance streams named dest
    b"AlternatePresentations",
    b"Renditions",
];

/// Per-page keys to remove. `/Annots` holds sticky notes, review comments,
/// stamp authors; per-page `/Metadata` is a full XMP packet; `/PieceInfo`
/// mirrors the catalog one.
const PAGE_KEYS_TO_STRIP: &[&[u8]] = &[
    b"Metadata",
    b"Annots",
    b"PieceInfo",
    b"UserUnit",
    b"ID",
    b"AA",
    b"B",           // Beads (article threads)
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
                            key: (*key).to_string(),
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

        // Surface the high-impact catalog leaks to the UI so the user
        // actually sees them in the "metadata" list.
        // First, parse the XMP packet in the catalog /Metadata stream
        // so its individual fields (dc:creator, xmp:CreatorTool, …)
        // show up instead of a single "XMP Metadata: present" line.
        let xmp_bytes: Option<Vec<u8>> = doc
            .catalog()
            .ok()
            .and_then(|cat| cat.get(b"Metadata").ok().and_then(|o| o.as_reference().ok()))
            .and_then(|id| doc.get_object(id).ok())
            .and_then(|obj| obj.as_stream().ok())
            .map(|s| {
                s.decompressed_content()
                    .unwrap_or_else(|_| s.content.clone())
            });
        if let Some(xmp) = &xmp_bytes {
            let fields = super::xmp::parse_xmp_fields(xmp);
            if fields.is_empty() {
                items.push(MetadataItem {
                    key: "XMP Metadata".to_string(),
                    value: "present".to_string(),
                });
            } else {
                items.extend(fields);
            }
        }

        if let Ok(catalog) = doc.catalog() {
            for key in [
                (&b"OpenAction"[..], "OpenAction (may run script on open)"),
                (&b"AA"[..], "Additional actions"),
                (&b"AcroForm"[..], "Form fields / signatures"),
                (&b"StructTreeRoot"[..], "Accessibility structure tree"),
                (&b"PieceInfo"[..], "Producer-specific piece info"),
                (&b"Outlines"[..], "Outline / bookmarks"),
                (&b"PageLabels"[..], "Page labels"),
                (&b"Perms"[..], "Usage permissions"),
            ] {
                if catalog.has(key.0) {
                    items.push(MetadataItem {
                        key: key.1.to_string(),
                        value: "present".to_string(),
                    });
                }
            }

            // /Names subtree: surface embedded files + JS specifically.
            if let Ok(names) = catalog.get(b"Names").and_then(Object::as_reference)
                && let Ok(names_dict) = doc.get_dictionary(names)
            {
                if names_dict.has(b"EmbeddedFiles") {
                    items.push(MetadataItem {
                        key: "Embedded files".to_string(),
                        value: "present".to_string(),
                    });
                }
                if names_dict.has(b"JavaScript") {
                    items.push(MetadataItem {
                        key: "JavaScript actions".to_string(),
                        value: "present".to_string(),
                    });
                }
            }
        }

        // Walk pages and report per-page annots / metadata.
        let page_count = doc.page_iter().count();
        let mut pages_with_annots = 0usize;
        let mut pages_with_xmp = 0usize;
        for page_id in doc.page_iter() {
            if let Ok(page) = doc.get_dictionary(page_id) {
                if page.has(b"Annots") {
                    pages_with_annots += 1;
                }
                if page.has(b"Metadata") {
                    pages_with_xmp += 1;
                }
            }
        }
        if pages_with_annots > 0 {
            items.push(MetadataItem {
                key: "Annotations".to_string(),
                value: format!("{pages_with_annots} of {page_count} pages"),
            });
        }
        if pages_with_xmp > 0 {
            items.push(MetadataItem {
                key: "Per-page XMP metadata".to_string(),
                value: format!("{pages_with_xmp} of {page_count} pages"),
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

        // --- 1. Drop the entire /Info dict from the trailer. ---------------
        let info_obj_ref = doc
            .trailer
            .get(b"Info")
            .ok()
            .and_then(|o| o.as_reference().ok());
        if let Some(id) = info_obj_ref {
            doc.delete_object(id);
        }
        doc.trailer.remove(b"Info");

        // --- 2. Trailer /ID is a per-document fingerprint. Null it. --------
        // Some viewers require /ID to be present, so we replace it with a
        // deterministic pair of zero-byte strings rather than removing it.
        let zero = Object::string_literal("");
        doc.trailer.set(
            "ID",
            Object::Array(vec![zero.clone(), zero]),
        );

        // --- 3. Walk the catalog and remove every metadata-bearing key. ----
        // First collect the referenced object ids so we can delete those
        // objects after releasing the catalog borrow.
        let mut catalog_refs_to_delete: Vec<ObjectId> = Vec::new();
        if let Ok(catalog) = doc.catalog() {
            for key in CATALOG_KEYS_TO_STRIP {
                if let Ok(obj) = catalog.get(key)
                    && let Ok(id) = obj.as_reference()
                {
                    catalog_refs_to_delete.push(id);
                }
            }
        }
        if let Ok(catalog) = doc.catalog_mut() {
            for key in CATALOG_KEYS_TO_STRIP {
                catalog.remove(key);
            }
        }
        for id in catalog_refs_to_delete {
            doc.delete_object(id);
        }

        // --- 4. /Names subtree: strip EmbeddedFiles, JavaScript, etc. ------
        let names_id = doc
            .catalog()
            .ok()
            .and_then(|c| c.get(b"Names").ok().and_then(|o| o.as_reference().ok()));
        if let Some(names_id) = names_id {
            let mut child_ids_to_delete: Vec<ObjectId> = Vec::new();
            if let Ok(names_dict) = doc.get_dictionary(names_id) {
                for key in NAMES_KEYS_TO_STRIP {
                    if let Ok(obj) = names_dict.get(key)
                        && let Ok(id) = obj.as_reference()
                    {
                        child_ids_to_delete.push(id);
                    }
                }
            }
            if let Ok(names_dict) = doc.get_dictionary_mut(names_id) {
                for key in NAMES_KEYS_TO_STRIP {
                    names_dict.remove(key);
                }
            }
            for id in child_ids_to_delete {
                doc.delete_object(id);
            }
        }

        // --- 5. Per-page cleaning ------------------------------------------
        // Copy the list of page ids out first so we can mutate the dicts
        // without holding an iterator into the object map.
        let page_ids: Vec<ObjectId> = doc.page_iter().collect();
        for page_id in page_ids {
            let mut per_page_refs_to_delete: Vec<ObjectId> = Vec::new();
            if let Ok(page) = doc.get_dictionary(page_id) {
                for key in PAGE_KEYS_TO_STRIP {
                    if let Ok(obj) = page.get(key) {
                        match obj {
                            Object::Reference(id) => per_page_refs_to_delete.push(*id),
                            Object::Array(arr) => {
                                for item in arr {
                                    if let Ok(id) = item.as_reference() {
                                        per_page_refs_to_delete.push(id);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            if let Ok(page) = doc.get_dictionary_mut(page_id) {
                for key in PAGE_KEYS_TO_STRIP {
                    page.remove(key);
                }
            }
            for id in per_page_refs_to_delete {
                doc.delete_object(id);
            }
        }

        // --- 6. Strip /Metadata from every XObject stream (image / form) ---
        // Digital-camera images embedded in a PDF ship their own XMP via a
        // /Metadata key on the image XObject. Drop that dict; we leave the
        // pixel stream intact.
        let xobject_ids: Vec<ObjectId> = doc
            .objects
            .iter()
            .filter_map(|(id, obj)| match obj {
                Object::Stream(s) => {
                    if matches!(s.dict.get(b"Type"), Ok(Object::Name(n)) if n == b"XObject") {
                        Some(*id)
                    } else if s.dict.has(b"Subtype") && s.dict.has(b"Width") {
                        // Image XObjects in the wild often omit /Type but
                        // always have /Subtype /Image + /Width + /Height.
                        Some(*id)
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .collect();
        for id in xobject_ids {
            if let Some(Object::Stream(stream)) = doc.objects.get_mut(&id) {
                stream.dict.remove(b"Metadata");
                stream.dict.remove(b"LastModified");
                stream.dict.remove(b"OC"); // Optional content (layers)
                stream.dict.remove(b"PieceInfo");
            }
        }

        // --- 7. Prune orphaned objects after all the deletions -------------
        doc.prune_objects();

        // --- 8. Save -------------------------------------------------------
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
