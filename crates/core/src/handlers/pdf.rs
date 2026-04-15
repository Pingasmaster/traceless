use std::path::Path;

use lopdf::{Document, Object, ObjectId};
use rand::Rng;

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
    b"AP", // Appearance streams named dest
    b"AlternatePresentations",
    b"Renditions",
];

/// Where the catalog `/Names` dictionary lives. ISO 32000-1 allows the
/// entry to be either an indirect reference to a dictionary object or a
/// direct dictionary embedded in the catalog itself; the cleaner must
/// handle both or it silently leaks the subtree on the direct-dict path.
#[derive(Clone, Copy)]
enum NamesLoc {
    Indirect(ObjectId),
    Direct,
}

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
    b"B", // Beads (article threads)
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
            .and_then(|cat| {
                cat.get(b"Metadata")
                    .ok()
                    .and_then(|o| o.as_reference().ok())
            })
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
            set.groups.push(MetadataGroup { filename, items });
        }
        Ok(set)
    }

    fn clean_metadata(&self, path: &Path, output_path: &Path) -> Result<(), CoreError> {
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

        // --- 2. Trailer /ID is a per-document fingerprint. Replace it. -----
        // ISO 32000-1 §14.4 defines /ID as a two-element array of byte
        // strings. Some viewers require /ID to be present, so we write a
        // fresh pair of 16-byte random strings rather than removing it
        // or leaving zero-byte literals. Randomizing instead of zeroing
        // matches mat2's behaviour and prevents cleaned PDFs from
        // sharing a trivially-detectable "`/ID []`" marker across every
        // file cleaned by traceless, which would otherwise enable batch
        // linking attacks against a corpus of cleaned files.
        let mut rng = rand::rng();
        let mut id_a = [0u8; 16];
        let mut id_b = [0u8; 16];
        rng.fill_bytes(&mut id_a);
        rng.fill_bytes(&mut id_b);
        doc.trailer.set(
            "ID",
            Object::Array(vec![
                Object::String(id_a.to_vec(), lopdf::StringFormat::Hexadecimal),
                Object::String(id_b.to_vec(), lopdf::StringFormat::Hexadecimal),
            ]),
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
        // ISO 32000-1 §7.11 allows /Names to be either an indirect
        // reference OR a direct dictionary embedded in the catalog.
        // Most producers emit an indirect ref, but a crafted fixture
        // (or a hand-written PDF) can put the dict inline and used to
        // slip its children past the stripper.
        let names_loc: Option<NamesLoc> = doc.catalog().ok().and_then(|c| match c.get(b"Names") {
            Ok(Object::Reference(id)) => Some(NamesLoc::Indirect(*id)),
            Ok(Object::Dictionary(_)) => Some(NamesLoc::Direct),
            _ => None,
        });
        if let Some(loc) = names_loc {
            let mut child_ids_to_delete: Vec<ObjectId> = Vec::new();

            // Phase 1: read-only scan for indirect children we orphan.
            {
                let names_dict_ref: Option<&lopdf::Dictionary> = match loc {
                    NamesLoc::Indirect(id) => doc.get_dictionary(id).ok(),
                    NamesLoc::Direct => doc.catalog().ok().and_then(|c| match c.get(b"Names") {
                        Ok(Object::Dictionary(d)) => Some(d),
                        _ => None,
                    }),
                };
                if let Some(nd) = names_dict_ref {
                    for key in NAMES_KEYS_TO_STRIP {
                        if let Ok(obj) = nd.get(key)
                            && let Ok(id) = obj.as_reference()
                        {
                            child_ids_to_delete.push(id);
                        }
                    }
                }
            }

            // Phase 2: prune the Names dict in place at its location.
            match loc {
                NamesLoc::Indirect(id) => {
                    if let Ok(nd) = doc.get_dictionary_mut(id) {
                        for key in NAMES_KEYS_TO_STRIP {
                            nd.remove(key);
                        }
                    }
                }
                NamesLoc::Direct => {
                    if let Ok(catalog) = doc.catalog_mut()
                        && let Ok(Object::Dictionary(nd)) = catalog.get_mut(b"Names")
                    {
                        for key in NAMES_KEYS_TO_STRIP {
                            nd.remove(key);
                        }
                    }
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

#[cfg(test)]
#[allow(clippy::too_many_lines, clippy::unwrap_used)]
mod tests {
    use super::*;
    use lopdf::dictionary;
    use lopdf::{Dictionary, Document as PdfDoc, Object, Stream};
    use tempfile::TempDir;

    /// Build a tiny valid PDF with the requested catalog entries set.
    /// Only the fields explicitly listed here are present; everything
    /// else stays at whatever lopdf's default is. Always sets a valid
    /// one-page tree so `clean_metadata` can walk it.
    fn make_pdf_with_catalog_keys(path: &std::path::Path, extra_catalog_keys: &[(&[u8], Object)]) {
        let mut doc = PdfDoc::with_version("1.7");

        let info_id = doc.add_object(dictionary! {
            "Author" => Object::string_literal("leak-author"),
            "Producer" => Object::string_literal("leak-producer"),
        });
        doc.trailer.set("Info", Object::Reference(info_id));

        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => Object::Name(b"Page".to_vec()),
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(612), Object::Integer(792),
                ]),
                "Resources" => Object::Dictionary(Dictionary::new()),
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => Object::Name(b"Pages".to_vec()),
                "Count" => Object::Integer(1),
                "Kids" => Object::Array(vec![Object::Reference(page_id)]),
            }),
        );

        let mut catalog = dictionary! {
            "Type" => Object::Name(b"Catalog".to_vec()),
            "Pages" => Object::Reference(pages_id),
        };
        for (k, v) in extra_catalog_keys {
            catalog.set(std::str::from_utf8(k).unwrap(), v.clone());
        }
        let catalog_id = doc.add_object(catalog);
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc.trailer.set(
            "ID",
            Object::Array(vec![
                Object::string_literal("fingerprint-a"),
                Object::string_literal("fingerprint-b"),
            ]),
        );

        doc.save(path).unwrap();
    }

    /// Open a freshly cleaned PDF and return its catalog dict so the
    /// test can assert that a given key is absent.
    fn reload_catalog(path: &std::path::Path) -> PdfDoc {
        PdfDoc::load(path).expect("cleaned PDF must still load")
    }

    #[test]
    fn pdf_object_to_string_handles_primitive_variants() {
        assert_eq!(pdf_object_to_string(&Object::string_literal("hi")), "hi");
        assert_eq!(pdf_object_to_string(&Object::Name(b"Foo".to_vec())), "Foo");
        assert_eq!(pdf_object_to_string(&Object::Integer(42)), "42");
        assert_eq!(pdf_object_to_string(&Object::Real(3.5)), "3.5");
        assert_eq!(pdf_object_to_string(&Object::Boolean(true)), "true");
    }

    #[test]
    fn clean_strips_info_dict_from_trailer() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("in.pdf");
        let dst = dir.path().join("out.pdf");
        make_pdf_with_catalog_keys(&src, &[]);

        PdfHandler.clean_metadata(&src, &dst).unwrap();
        let reloaded = reload_catalog(&dst);
        assert!(
            reloaded.trailer.get(b"Info").is_err(),
            "cleaned PDF must have no /Info in the trailer"
        );
    }

    #[test]
    fn clean_randomizes_trailer_id() {
        // Regression for MEDIUM-1 in the audit: the trailer /ID used to
        // be set to a deterministic pair of zero-byte string literals.
        // That wiped the original fingerprint (good) but made every
        // cleaned PDF byte-identical on /ID, which is itself a "cleaned
        // by traceless" marker and weaker than mat2's randomized /ID
        // against cross-file batch linking. The cleaner now writes two
        // fresh 16-byte random strings on every run.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("in.pdf");
        let dst_a = dir.path().join("out_a.pdf");
        let dst_b = dir.path().join("out_b.pdf");
        make_pdf_with_catalog_keys(&src, &[]);

        PdfHandler.clean_metadata(&src, &dst_a).unwrap();
        PdfHandler.clean_metadata(&src, &dst_b).unwrap();

        let id_bytes = |path: &std::path::Path| -> Vec<Vec<u8>> {
            let reloaded = reload_catalog(path);
            let id = reloaded
                .trailer
                .get(b"ID")
                .expect("trailer /ID must still exist (required by some readers)");
            let arr = id.as_array().unwrap();
            assert_eq!(arr.len(), 2);
            arr.iter()
                .map(|entry| match entry {
                    Object::String(bytes, _) => bytes.clone(),
                    other => panic!("/ID entry must be a string, got {other:?}"),
                })
                .collect()
        };

        let a = id_bytes(&dst_a);
        let b = id_bytes(&dst_b);

        // Original fingerprints are gone.
        assert!(
            !a[0].windows(13).any(|w| w == b"fingerprint-a"),
            "original ID leaked into cleaned /ID[0]"
        );
        assert!(
            !a[1].windows(13).any(|w| w == b"fingerprint-b"),
            "original ID leaked into cleaned /ID[1]"
        );
        // Non-empty and 16 bytes each.
        assert_eq!(a[0].len(), 16, "/ID[0] must be 16 random bytes");
        assert_eq!(a[1].len(), 16, "/ID[1] must be 16 random bytes");
        // Two independent clean runs produce different /ID values.
        // (Birthday collision on 16 random bytes is ~1 in 2^128.)
        assert_ne!(a[0], b[0], "/ID[0] must differ between clean runs");
        assert_ne!(a[1], b[1], "/ID[1] must differ between clean runs");
        // The two halves are themselves independent.
        assert_ne!(a[0], a[1], "/ID[0] and /ID[1] must be independent");
    }

    /// Parameterised stripping test: for each catalog key we claim to
    /// strip, build a PDF with that key populated, clean, and assert
    /// the key is absent from the reloaded catalog.
    #[test]
    fn every_catalog_key_in_strip_list_is_removed() {
        for key in CATALOG_KEYS_TO_STRIP {
            let dir = TempDir::new().unwrap();
            let src = dir.path().join("in.pdf");
            let dst = dir.path().join("out.pdf");

            // A stand-in object for each key. Use a reference to a
            // throwaway dict for the ones that expect a dict; strings
            // are accepted for the primitive keys.
            let placeholder = Object::string_literal("leak");
            make_pdf_with_catalog_keys(&src, &[(key, placeholder)]);

            PdfHandler.clean_metadata(&src, &dst).unwrap();
            let reloaded = reload_catalog(&dst);
            let catalog = reloaded.catalog().unwrap();
            assert!(
                catalog.get(key).is_err(),
                "catalog key {} must be removed",
                String::from_utf8_lossy(key)
            );
        }
    }

    /// Build a PDF whose catalog has `/Names` as an **indirect
    /// reference** to a dictionary object containing every key in
    /// `NAMES_KEYS_TO_STRIP`. The cleaner must delete all of them.
    fn make_pdf_with_names_indirect(path: &std::path::Path) {
        let mut doc = PdfDoc::with_version("1.7");

        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => Object::Name(b"Page".to_vec()),
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(612), Object::Integer(792),
                ]),
                "Resources" => Object::Dictionary(Dictionary::new()),
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => Object::Name(b"Pages".to_vec()),
                "Count" => Object::Integer(1),
                "Kids" => Object::Array(vec![Object::Reference(page_id)]),
            }),
        );

        let mut names_dict = Dictionary::new();
        for key in NAMES_KEYS_TO_STRIP {
            names_dict.set(
                std::str::from_utf8(key).unwrap(),
                Object::string_literal("leak"),
            );
        }
        let names_id = doc.add_object(Object::Dictionary(names_dict));

        let catalog = dictionary! {
            "Type" => Object::Name(b"Catalog".to_vec()),
            "Pages" => Object::Reference(pages_id),
            "Names" => Object::Reference(names_id),
        };
        let catalog_id = doc.add_object(catalog);
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc.save(path).unwrap();
    }

    /// Build a PDF whose catalog has `/Names` as a **direct dictionary**
    /// (not an indirect reference). Before the NamesLoc fix this path
    /// was silently skipped by the cleaner because `as_reference()`
    /// failed, leaving /EmbeddedFiles, /JavaScript, etc. intact.
    fn make_pdf_with_names_direct(path: &std::path::Path) {
        let mut doc = PdfDoc::with_version("1.7");

        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => Object::Name(b"Page".to_vec()),
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(612), Object::Integer(792),
                ]),
                "Resources" => Object::Dictionary(Dictionary::new()),
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => Object::Name(b"Pages".to_vec()),
                "Count" => Object::Integer(1),
                "Kids" => Object::Array(vec![Object::Reference(page_id)]),
            }),
        );

        let mut names_dict = Dictionary::new();
        for key in NAMES_KEYS_TO_STRIP {
            names_dict.set(
                std::str::from_utf8(key).unwrap(),
                Object::string_literal("leak"),
            );
        }

        let catalog = dictionary! {
            "Type" => Object::Name(b"Catalog".to_vec()),
            "Pages" => Object::Reference(pages_id),
            "Names" => Object::Dictionary(names_dict),
        };
        let catalog_id = doc.add_object(catalog);
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc.save(path).unwrap();
    }

    fn assert_names_children_gone(path: &std::path::Path) {
        let reloaded = reload_catalog(path);
        let catalog = reloaded.catalog().unwrap();
        let Ok(names_obj) = catalog.get(b"Names") else {
            // Whole /Names entry may have been pruned along with its
            // orphaned children; that's an acceptable stronger result.
            return;
        };
        let nd: &Dictionary = match names_obj {
            Object::Reference(id) => reloaded
                .get_dictionary(*id)
                .expect("indirect /Names must resolve"),
            Object::Dictionary(d) => d,
            other => panic!("unexpected /Names variant: {other:?}"),
        };
        for key in NAMES_KEYS_TO_STRIP {
            assert!(
                nd.get(key).is_err(),
                "/Names child {} must be removed",
                String::from_utf8_lossy(key)
            );
        }
    }

    #[test]
    fn every_names_key_in_strip_list_is_removed_indirect() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("in.pdf");
        let dst = dir.path().join("out.pdf");
        make_pdf_with_names_indirect(&src);
        PdfHandler.clean_metadata(&src, &dst).unwrap();
        assert_names_children_gone(&dst);
    }

    #[test]
    fn every_names_key_in_strip_list_is_removed_direct() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("in.pdf");
        let dst = dir.path().join("out.pdf");
        make_pdf_with_names_direct(&src);
        PdfHandler.clean_metadata(&src, &dst).unwrap();
        assert_names_children_gone(&dst);
    }

    #[test]
    fn clean_strips_per_page_metadata_and_annots() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("in.pdf");
        let dst = dir.path().join("out.pdf");

        // Build a PDF whose single page carries per-page /Metadata and
        // /Annots. make_pdf_with_catalog_keys only sets catalog keys,
        // so we do this one by hand.
        let mut doc = PdfDoc::with_version("1.7");
        let xmp_stream = doc.add_object(Object::Stream(Stream::new(
            Dictionary::new(),
            b"leak xmp".to_vec(),
        )));
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => Object::Name(b"Page".to_vec()),
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(612), Object::Integer(792),
                ]),
                "Resources" => Object::Dictionary(Dictionary::new()),
                "Metadata" => Object::Reference(xmp_stream),
                "Annots" => Object::Array(vec![]),
                "PieceInfo" => dictionary! { "App" => Object::string_literal("leak") },
                "UserUnit" => Object::Real(1.25),
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => Object::Name(b"Pages".to_vec()),
                "Count" => Object::Integer(1),
                "Kids" => Object::Array(vec![Object::Reference(page_id)]),
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => Object::Name(b"Catalog".to_vec()),
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc.save(&src).unwrap();

        PdfHandler.clean_metadata(&src, &dst).unwrap();
        let reloaded = reload_catalog(&dst);
        let pages: Vec<ObjectId> = reloaded.page_iter().collect();
        assert_eq!(pages.len(), 1);
        let page = reloaded.get_dictionary(pages[0]).unwrap();
        for key in PAGE_KEYS_TO_STRIP {
            assert!(
                page.get(key).is_err(),
                "per-page key {} must be removed",
                String::from_utf8_lossy(key)
            );
        }
    }

    #[test]
    fn clean_does_not_crash_on_missing_info_reference() {
        // Build a PDF whose trailer /Info points at an object ID that
        // doesn't exist. The handler must handle this without
        // panicking.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("in.pdf");
        let dst = dir.path().join("out.pdf");

        let mut doc = PdfDoc::with_version("1.7");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => Object::Name(b"Page".to_vec()),
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(612), Object::Integer(792),
                ]),
                "Resources" => Object::Dictionary(Dictionary::new()),
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => Object::Name(b"Pages".to_vec()),
                "Count" => Object::Integer(1),
                "Kids" => Object::Array(vec![Object::Reference(page_id)]),
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => Object::Name(b"Catalog".to_vec()),
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        // Dangling /Info reference
        doc.trailer.set("Info", Object::Reference((9999, 0)));
        doc.save(&src).unwrap();

        // Should not panic. Result can be ok or err, but not a panic.
        let _ = PdfHandler.clean_metadata(&src, &dst);
    }

    #[test]
    fn read_metadata_surfaces_info_fields_before_clean() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("in.pdf");
        make_pdf_with_catalog_keys(&src, &[]);

        let meta = PdfHandler.read_metadata(&src).unwrap();
        let keys: Vec<&str> = meta
            .groups
            .iter()
            .flat_map(|g| g.items.iter().map(|i| i.key.as_str()))
            .collect();
        assert!(keys.contains(&"Author"), "expected Author in {keys:?}");
        assert!(keys.contains(&"Producer"), "expected Producer in {keys:?}");
    }

    #[test]
    fn read_metadata_on_empty_info_is_empty() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("in.pdf");
        let dst = dir.path().join("out.pdf");
        make_pdf_with_catalog_keys(&src, &[]);

        // After clean, reading must surface no Author/Producer.
        PdfHandler.clean_metadata(&src, &dst).unwrap();
        let meta = PdfHandler.read_metadata(&dst).unwrap();
        let items: Vec<(&str, &str)> = meta
            .groups
            .iter()
            .flat_map(|g| g.items.iter().map(|i| (i.key.as_str(), i.value.as_str())))
            .collect();
        assert!(
            !items.iter().any(|(k, _)| *k == "Author"),
            "cleaned PDF still surfaces Author: {items:?}"
        );
        assert!(
            !items.iter().any(|(k, _)| *k == "Producer"),
            "cleaned PDF still surfaces Producer: {items:?}"
        );
    }

    #[test]
    fn clean_strips_xobject_metadata() {
        // Image XObjects in the wild carry their own XMP via /Metadata
        // on the stream dict. The cleaner walks `doc.objects` and
        // strips /Metadata, /LastModified, /OC, /PieceInfo from each
        // XObject stream. Build one and verify.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("in.pdf");
        let dst = dir.path().join("out.pdf");

        let mut doc = PdfDoc::with_version("1.7");

        // A tagged XObject stream carrying a /Metadata ref.
        let xmp_id = doc.add_object(Object::Stream(Stream::new(
            Dictionary::new(),
            b"xmp leak".to_vec(),
        )));
        let mut xobj_dict = Dictionary::new();
        xobj_dict.set("Type", Object::Name(b"XObject".to_vec()));
        xobj_dict.set("Subtype", Object::Name(b"Image".to_vec()));
        xobj_dict.set("Width", Object::Integer(1));
        xobj_dict.set("Height", Object::Integer(1));
        xobj_dict.set("Metadata", Object::Reference(xmp_id));
        xobj_dict.set("LastModified", Object::string_literal("D:20240101000000Z"));
        let xobj_id = doc.add_object(Object::Stream(Stream::new(xobj_dict, b"pixel".to_vec())));

        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => Object::Name(b"Page".to_vec()),
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(612), Object::Integer(792),
                ]),
                "Resources" => dictionary! {
                    "XObject" => dictionary! {
                        "Im1" => Object::Reference(xobj_id),
                    },
                },
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => Object::Name(b"Pages".to_vec()),
                "Count" => Object::Integer(1),
                "Kids" => Object::Array(vec![Object::Reference(page_id)]),
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => Object::Name(b"Catalog".to_vec()),
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc.save(&src).unwrap();

        PdfHandler.clean_metadata(&src, &dst).unwrap();
        let reloaded = reload_catalog(&dst);
        // Walk every stream object and assert no /Metadata or
        // /LastModified survived on any XObject.
        for obj in reloaded.objects.values() {
            if let Object::Stream(s) = obj {
                let is_xobject = matches!(s.dict.get(b"Type"), Ok(Object::Name(n)) if n == b"XObject")
                    || (s.dict.has(b"Subtype") && s.dict.has(b"Width"));
                if is_xobject {
                    assert!(
                        s.dict.get(b"Metadata").is_err(),
                        "XObject retained /Metadata after clean"
                    );
                    assert!(
                        s.dict.get(b"LastModified").is_err(),
                        "XObject retained /LastModified after clean"
                    );
                }
            }
        }
    }

    #[test]
    fn clean_with_no_catalog_does_not_panic() {
        // Build the minimum viable PDF that lopdf will save, then
        // remove the Root reference so the catalog is unreachable.
        // Edge case: the handler must not crash, only return an Err.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("in.pdf");
        let dst = dir.path().join("out.pdf");

        let mut doc = PdfDoc::with_version("1.7");
        let page_id = doc.new_object_id();
        let pages_id = doc.add_object(dictionary! {
            "Type" => Object::Name(b"Pages".to_vec()),
            "Count" => Object::Integer(1),
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
        });
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => Object::Name(b"Page".to_vec()),
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(612), Object::Integer(792),
                ]),
                "Resources" => Object::Dictionary(Dictionary::new()),
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => Object::Name(b"Catalog".to_vec()),
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc.save(&src).unwrap();

        let result = std::panic::catch_unwind(|| {
            let _ = PdfHandler.clean_metadata(&src, &dst);
        });
        assert!(result.is_ok(), "handler panicked on degenerate PDF");
    }
}
