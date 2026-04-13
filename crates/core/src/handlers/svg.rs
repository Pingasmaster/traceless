//! SVG metadata cleaner.
//!
//! SVG is XML. Metadata hides in a handful of well-known vectors:
//! - `<metadata>` elements (RDF-style Dublin Core blocks, typically
//!   carrying creator, title, licence, Inkscape version, …).
//! - `<title>` and `<desc>` elements — accessibility metadata but
//!   often misused for author names.
//! - `<sodipodi:namedview>` / `<inkscape:*>` nested elements — GUI
//!   editor state and author identifiers.
//! - Attributes in the `inkscape:`, `sodipodi:`, `rdf:` and `dc:`
//!   namespaces on any element — very commonly leaked.
//!
//! mat2 handles this by rasterizing the SVG through rsvg/cairo, which
//! nukes every non-pixel thing. We instead keep the vector structure
//! intact and filter at the XML event level, because:
//! 1. Re-rendering is lossy — animations, interactivity, and
//!    high-fidelity text all get baked into a pixmap.
//! 2. rsvg is a large C dep we're trying to avoid.
//! 3. SVG's metadata vectors are a fixed, well-documented set.

use std::borrow::Cow;
use std::fs;
use std::io::Cursor;
use std::path::Path;

use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use quick_xml::writer::Writer;

use crate::error::CoreError;
use crate::metadata::{MetadataGroup, MetadataItem, MetadataSet};

use super::FormatHandler;

pub struct SvgHandler;

/// Element local-names that we drop entirely (along with every child).
const DROP_ELEMENTS: &[&str] = &[
    "metadata",      // the whole RDF block
    "title",         // often contains author name
    "desc",          // often contains description + author
    "namedview",     // sodipodi editor state
];

/// Namespace prefixes whose attributes we strip from every element.
const STRIP_NS_PREFIXES: &[&str] = &[
    "inkscape:",
    "sodipodi:",
    "rdf:",
    "dc:",
    "cc:",
];

impl FormatHandler for SvgHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
        let bytes = fs::read(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        let mut reader = Reader::from_reader(bytes.as_slice());
        let mut items: Vec<MetadataItem> = Vec::new();
        let mut current_tag: Option<String> = None;

        loop {
            match reader.read_event() {
                Ok(Event::Start(ref e)) => {
                    let name = local_name_of(e);
                    let qname = full_name_of(e);

                    // Expose inkscape/sodipodi/rdf/dc attributes as leaks
                    for attr in e.attributes().filter_map(Result::ok) {
                        let key_str = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
                        if STRIP_NS_PREFIXES
                            .iter()
                            .any(|p| key_str.starts_with(p))
                        {
                            let value = String::from_utf8_lossy(attr.value.as_ref()).into_owned();
                            items.push(MetadataItem {
                                key: format!("<{qname}> {key_str}"),
                                value,
                            });
                        }
                    }

                    if DROP_ELEMENTS.contains(&name.as_str()) {
                        current_tag = Some(qname);
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    for attr in e.attributes().filter_map(Result::ok) {
                        let key_str = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
                        if STRIP_NS_PREFIXES
                            .iter()
                            .any(|p| key_str.starts_with(p))
                        {
                            let value = String::from_utf8_lossy(attr.value.as_ref()).into_owned();
                            items.push(MetadataItem {
                                key: format!("<{}> {key_str}", full_name_of(e)),
                                value,
                            });
                        }
                    }
                }
                Ok(Event::Text(ref t)) => {
                    if let Some(tag) = &current_tag {
                        let text = String::from_utf8_lossy(t.as_ref()).trim().to_string();
                        if !text.is_empty() {
                            items.push(MetadataItem {
                                key: format!("<{tag}>"),
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

        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut set = MetadataSet::default();
        if !items.is_empty() {
            set.groups.push(MetadataGroup { filename, items });
        }
        Ok(set)
    }

    fn clean_metadata(&self, path: &Path, output_path: &Path) -> Result<(), CoreError> {
        let bytes = fs::read(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        let mut reader = Reader::from_reader(bytes.as_slice());
        reader.config_mut().expand_empty_elements = false;

        let mut writer = Writer::new(Cursor::new(Vec::new()));
        let mut skip_depth: usize = 0;

        loop {
            match reader.read_event() {
                Ok(Event::Start(ref e)) => {
                    if skip_depth > 0 {
                        skip_depth += 1;
                        continue;
                    }
                    let name = local_name_of(e);
                    if DROP_ELEMENTS.contains(&name.as_str()) {
                        skip_depth = 1;
                        continue;
                    }
                    let sanitized = sanitize_attributes(e);
                    writer
                        .write_event(Event::Start(sanitized))
                        .map_err(|err| CoreError::CleanError {
                            path: path.to_path_buf(),
                            detail: format!("SVG write error: {err}"),
                        })?;
                }
                Ok(Event::End(ref e)) => {
                    if skip_depth > 0 {
                        skip_depth -= 1;
                        continue;
                    }
                    writer
                        .write_event(Event::End(e.clone()))
                        .map_err(|err| CoreError::CleanError {
                            path: path.to_path_buf(),
                            detail: format!("SVG write error: {err}"),
                        })?;
                }
                Ok(Event::Empty(ref e)) => {
                    if skip_depth > 0 {
                        continue;
                    }
                    let name = local_name_of(e);
                    if DROP_ELEMENTS.contains(&name.as_str()) {
                        continue;
                    }
                    let sanitized = sanitize_attributes(e);
                    writer
                        .write_event(Event::Empty(sanitized))
                        .map_err(|err| CoreError::CleanError {
                            path: path.to_path_buf(),
                            detail: format!("SVG write error: {err}"),
                        })?;
                }
                Ok(Event::Text(ref t)) => {
                    if skip_depth == 0 {
                        writer
                            .write_event(Event::Text(t.clone()))
                            .map_err(|err| CoreError::CleanError {
                                path: path.to_path_buf(),
                                detail: format!("SVG write error: {err}"),
                            })?;
                    }
                }
                Ok(Event::Comment(_)) => {
                    // SVG comments are a metadata vector — drop them.
                }
                Ok(Event::Eof) => break,
                Ok(other) => {
                    if skip_depth == 0 {
                        writer.write_event(other).map_err(|err| CoreError::CleanError {
                            path: path.to_path_buf(),
                            detail: format!("SVG write error: {err}"),
                        })?;
                    }
                }
                Err(e) => {
                    return Err(CoreError::ParseError {
                        path: path.to_path_buf(),
                        detail: format!("SVG parse error: {e}"),
                    });
                }
            }
        }

        let cleaned = writer.into_inner().into_inner();
        fs::write(output_path, &cleaned).map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("Failed to write cleaned SVG: {e}"),
        })?;
        Ok(())
    }

    fn supported_mime_types(&self) -> &[&str] {
        &["image/svg+xml"]
    }
}

/// Rewrite a start tag's attributes, dropping any whose key starts with
/// an `inkscape:` / `sodipodi:` / `rdf:` / `dc:` / `cc:` prefix.
fn sanitize_attributes(start: &BytesStart<'_>) -> BytesStart<'static> {
    let name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let mut out = BytesStart::new(name);

    for attr in start.attributes().filter_map(Result::ok) {
        let key_bytes = attr.key.as_ref().to_vec();
        let key_str = String::from_utf8_lossy(&key_bytes);

        if STRIP_NS_PREFIXES.iter().any(|p| key_str.starts_with(p)) {
            continue;
        }
        // Drop xmlns declarations for dropped namespaces to keep the
        // output tidy — they otherwise dangle as unused prefix bindings.
        if let Some(prefix) = key_str.strip_prefix("xmlns:")
            && STRIP_NS_PREFIXES.iter().any(|p| p.trim_end_matches(':') == prefix)
        {
            continue;
        }

        let value = attr.value.into_owned();
        out.push_attribute(Attribute {
            key: quick_xml::name::QName(&key_bytes),
            value: Cow::Owned(value),
        });
    }
    out
}

fn local_name_of(start: &BytesStart<'_>) -> String {
    let full = start.name();
    let full_bytes = full.as_ref();
    match full_bytes.iter().position(|&b| b == b':') {
        Some(i) => String::from_utf8_lossy(&full_bytes[i + 1..]).into_owned(),
        None => String::from_utf8_lossy(full_bytes).into_owned(),
    }
}

fn full_name_of(start: &BytesStart<'_>) -> String {
    String::from_utf8_lossy(start.name().as_ref()).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_dirty(path: &Path) {
        let xml = br#"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg"
     xmlns:dc="http://purl.org/dc/elements/1.1/"
     xmlns:cc="http://creativecommons.org/ns#"
     xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:sodipodi="http://sodipodi.sourceforge.net/DTD/sodipodi-0.0.dtd"
     xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape"
     width="10" height="10"
     inkscape:version="1.2 (secret-machine)"
     sodipodi:docname="secret.svg">
  <!-- a secret comment -->
  <metadata>
    <rdf:RDF>
      <cc:Work>
        <dc:creator>Secret Author</dc:creator>
        <dc:title>Secret Title</dc:title>
      </cc:Work>
    </rdf:RDF>
  </metadata>
  <title>Secret Title</title>
  <desc>Secret description containing author email</desc>
  <sodipodi:namedview id="view1" inkscape:pageopacity="1"/>
  <g inkscape:label="secret-layer">
    <rect x="0" y="0" width="10" height="10" fill="red"/>
  </g>
</svg>"#;
        fs::write(path, xml).unwrap();
    }

    #[test]
    fn svg_read_surfaces_metadata() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("dirty.svg");
        write_dirty(&src);
        let h = SvgHandler;
        let meta = h.read_metadata(&src).unwrap();
        assert!(!meta.is_empty(), "dirty SVG should report metadata");
        let dump = format!("{meta:?}");
        assert!(dump.contains("inkscape:version"));
        assert!(dump.contains("sodipodi:docname"));
    }

    #[test]
    fn svg_clean_drops_every_vector() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("dirty.svg");
        let dst = dir.path().join("clean.svg");
        write_dirty(&src);

        let h = SvgHandler;
        h.clean_metadata(&src, &dst).unwrap();

        let out = fs::read_to_string(&dst).unwrap();
        for needle in [
            "Secret Author",
            "Secret Title",
            "secret-machine",
            "secret.svg",
            "secret comment",
            "secret-layer",
            "secret description",
            "dc:creator",
            "inkscape:version",
            "sodipodi:docname",
            "<metadata>",
            "<title>",
            "<desc>",
        ] {
            assert!(
                !out.contains(needle),
                "'{needle}' leaked through SVG clean: {out}"
            );
        }
        // Structural content survives
        assert!(out.contains("<rect"), "shapes must survive: {out}");
    }
}
