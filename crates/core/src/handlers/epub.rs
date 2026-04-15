//! Deep-clean helpers for EPUB.
//!
//! mat2's `libmat2/epub.py::EPUBParser` does three things we replicate:
//! 1. Replace the `<metadata>` block in `content.opf` with a minimal one
//!    carrying a fresh random `dc:identifier`, an empty `dc:language`, and
//!    an empty `dc:title`. These three elements are mandatory per EPUB
//!    spec; everything else (dc:creator, dc:publisher, dc:date, …) is
//!    fingerprinting.
//! 2. Clear the `<head>` of `OEBPS/toc.ncx` and any `OPS/*.xml`. The
//!    `<head>` section of an NCX file holds metadata tags like the
//!    book identifier; we blank them.
//! 3. Reject archives containing `META-INF/encryption.xml` (DRM / encrypted
//!    fonts — mat2 refuses to process these because it can't safely
//!    re-pack them).
//!
//! The archive-level drops (iTunesMetadata.plist, calibre_bookmarks.txt)
//! live in `zip_util::is_office_junk_path`.

use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::reader::Reader;
use quick_xml::writer::Writer;
use rand::Rng;
use std::io::Cursor;

use crate::error::CoreError;

use super::xml_util::local_name;

fn clean_err(detail: impl Into<String>) -> CoreError {
    CoreError::CleanError {
        path: std::path::PathBuf::new(),
        detail: detail.into(),
    }
}

/// Returns true if the archive member path is an OPF file. We treat
/// every `*.opf` as a content.opf candidate because publishers use
/// varying directory layouts (OEBPS/, OPS/, bare root, hmh.opf, …).
#[must_use]
pub fn is_opf_path(name: &str) -> bool {
    name.ends_with(".opf")
}

/// Returns true if the archive member path is an NCX navigation file.
#[must_use]
pub fn is_ncx_path(name: &str) -> bool {
    name.ends_with(".ncx")
}

/// Returns true for EPUB content documents whose `<head>` we want to
/// blank (OPS/*.xml, OEBPS/*.xml per mat2 epub.py line 23).
#[must_use]
pub fn is_ops_xml_path(name: &str) -> bool {
    (name.starts_with("OPS/") || name.starts_with("OEBPS/"))
        && (name.ends_with(".xml") || name.ends_with(".xhtml"))
        && !name.ends_with(".opf")
        && !name.ends_with(".ncx")
}

/// Rewrite `content.opf` with a minimal metadata block containing only
/// a fresh UUID identifier, an empty dc:language, and an empty dc:title.
///
/// # Errors
///
/// Returns `CoreError::CleanError` on any XML parse or write failure so
/// a crafted-malformed `content.opf` cannot slip past the cleaner. The
/// pre-F2 behaviour was to return the original bytes on parse error,
/// which shipped unstripped `dc:creator` / `dc:publisher` metadata
/// straight through.
pub fn clean_opf(xml: &str) -> Result<String, CoreError> {
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut skip_depth: usize = 0;
    let mut rng = rand::rng();
    let uuid = generate_urn_uuid_v4(&mut rng);
    let mut replaced_metadata = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                if skip_depth > 0 {
                    skip_depth += 1;
                    continue;
                }
                let ln = local_name(e);
                if ln == "metadata" {
                    // Re-emit just the outer <metadata> with the same
                    // attributes, then inject our three mandatory children,
                    // then consume up to </metadata>.
                    writer
                        .write_event(Event::Start(e.clone()))
                        .map_err(|err| clean_err(format!("OPF write error: {err}")))?;
                    write_minimal_metadata(&mut writer, &uuid)?;
                    writer
                        .write_event(Event::End(BytesEnd::new(
                            String::from_utf8_lossy(e.name().as_ref()).into_owned(),
                        )))
                        .map_err(|err| clean_err(format!("OPF write error: {err}")))?;
                    // Skip until the corresponding </metadata>
                    skip_depth = 1;
                    replaced_metadata = true;
                    continue;
                }
                writer
                    .write_event(Event::Start(e.clone()))
                    .map_err(|err| clean_err(format!("OPF write error: {err}")))?;
            }
            Ok(Event::End(ref e)) => {
                if skip_depth > 0 {
                    skip_depth -= 1;
                    continue;
                }
                writer
                    .write_event(Event::End(e.clone()))
                    .map_err(|err| clean_err(format!("OPF write error: {err}")))?;
            }
            Ok(Event::Empty(ref e)) => {
                if skip_depth > 0 {
                    continue;
                }
                writer
                    .write_event(Event::Empty(e.clone()))
                    .map_err(|err| clean_err(format!("OPF write error: {err}")))?;
            }
            Ok(Event::Text(ref t)) => {
                if skip_depth == 0 {
                    writer
                        .write_event(Event::Text(t.clone()))
                        .map_err(|err| clean_err(format!("OPF write error: {err}")))?;
                }
            }
            Ok(Event::Eof) => break,
            Ok(other) => {
                if skip_depth == 0 {
                    writer
                        .write_event(other)
                        .map_err(|err| clean_err(format!("OPF write error: {err}")))?;
                }
            }
            Err(err) => {
                return Err(clean_err(format!("OPF parse error: {err}")));
            }
        }
    }

    // If the OPF had no <metadata> block at all, the source file is
    // structurally unusable. Refuse rather than shipping the original.
    if !replaced_metadata {
        return Err(clean_err(
            "content.opf has no <metadata> block; refusing to emit an \
             unstripped OPF",
        ));
    }
    String::from_utf8(writer.into_inner().into_inner())
        .map_err(|err| clean_err(format!("OPF cleaned output was not UTF-8: {err}")))
}

/// Blank the `<head>` section of an NCX or OPS XML file. We preserve
/// the rest of the document (which contains the actual text / navigation
/// structure).
///
/// # Errors
///
/// Returns `CoreError::CleanError` on any XML parse or write failure.
/// A pre-F2 silent fallback to `xml.to_string()` let a malformed NCX
/// ship with its `<head>` metadata (dtb:uid, Calibre generator string)
/// intact.
pub fn clean_head_only(xml: &str) -> Result<String, CoreError> {
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut skip_depth: usize = 0;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                if skip_depth > 0 {
                    skip_depth += 1;
                    continue;
                }
                let ln = local_name(e);
                if ln == "head" {
                    // Re-emit the head with no children so the document
                    // structure stays intact.
                    writer
                        .write_event(Event::Start(e.clone()))
                        .map_err(|err| clean_err(format!("NCX/OPS write error: {err}")))?;
                    writer
                        .write_event(Event::End(BytesEnd::new(
                            String::from_utf8_lossy(e.name().as_ref()).into_owned(),
                        )))
                        .map_err(|err| clean_err(format!("NCX/OPS write error: {err}")))?;
                    skip_depth = 1;
                    continue;
                }
                writer
                    .write_event(Event::Start(e.clone()))
                    .map_err(|err| clean_err(format!("NCX/OPS write error: {err}")))?;
            }
            Ok(Event::End(ref e)) => {
                if skip_depth > 0 {
                    skip_depth -= 1;
                    continue;
                }
                writer
                    .write_event(Event::End(e.clone()))
                    .map_err(|err| clean_err(format!("NCX/OPS write error: {err}")))?;
            }
            Ok(Event::Empty(ref e)) => {
                if skip_depth > 0 {
                    continue;
                }
                writer
                    .write_event(Event::Empty(e.clone()))
                    .map_err(|err| clean_err(format!("NCX/OPS write error: {err}")))?;
            }
            Ok(Event::Text(ref t)) => {
                if skip_depth == 0 {
                    writer
                        .write_event(Event::Text(t.clone()))
                        .map_err(|err| clean_err(format!("NCX/OPS write error: {err}")))?;
                }
            }
            Ok(Event::Eof) => break,
            Ok(other) => {
                if skip_depth == 0 {
                    writer
                        .write_event(other)
                        .map_err(|err| clean_err(format!("NCX/OPS write error: {err}")))?;
                }
            }
            Err(err) => {
                return Err(clean_err(format!("NCX/OPS parse error: {err}")));
            }
        }
    }

    String::from_utf8(writer.into_inner().into_inner())
        .map_err(|err| clean_err(format!("NCX/OPS cleaned output was not UTF-8: {err}")))
}

fn write_minimal_metadata(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    urn: &str,
) -> Result<(), CoreError> {
    let err = |e: std::io::Error| clean_err(format!("OPF metadata-stub write error: {e}"));
    // <dc:identifier id="id">urn:uuid:…</dc:identifier>
    let mut ident = BytesStart::new("dc:identifier");
    ident.push_attribute(("id", "id"));
    writer.write_event(Event::Start(ident.clone())).map_err(err)?;
    writer
        .write_event(Event::Text(BytesText::new(urn)))
        .map_err(err)?;
    writer
        .write_event(Event::End(BytesEnd::new("dc:identifier")))
        .map_err(err)?;

    // Empty dc:language
    writer
        .write_event(Event::Start(BytesStart::new("dc:language")))
        .map_err(err)?;
    writer
        .write_event(Event::End(BytesEnd::new("dc:language")))
        .map_err(err)?;

    // Empty dc:title
    writer
        .write_event(Event::Start(BytesStart::new("dc:title")))
        .map_err(err)?;
    writer
        .write_event(Event::End(BytesEnd::new("dc:title")))
        .map_err(err)?;
    Ok(())
}

/// Generate a `urn:uuid:xxxxxxxx-…` string with 128 random bits shaped
/// into a v4 UUID. We don't pull in the `uuid` crate for this — 30
/// lines of `rng.random()` plus a format string is plenty.
fn generate_urn_uuid_v4(rng: &mut impl Rng) -> String {
    let mut bytes = [0u8; 16];
    rng.fill_bytes(&mut bytes);
    // Version 4: set the high nibble of byte 6 to 0x4
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    // Variant RFC 4122: set the top two bits of byte 8 to 10
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    format!(
        "urn:uuid:{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn opf_metadata_is_replaced_with_uuid() {
        let xml = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Secret Book</dc:title>
    <dc:creator>Jane Doe</dc:creator>
    <dc:publisher>Secret Press</dc:publisher>
  </metadata>
  <manifest/>
</package>"#;
        let out = clean_opf(xml).unwrap();
        assert!(!out.contains("Jane Doe"), "author must be removed: {out}");
        assert!(
            !out.contains("Secret Press"),
            "publisher must be removed: {out}"
        );
        assert!(!out.contains("Secret Book"), "title must be removed: {out}");
        assert!(
            out.contains("urn:uuid:"),
            "new UUID must be injected: {out}"
        );
        assert!(out.contains("dc:identifier"), "identifier element required");
        assert!(out.contains("dc:language"), "language element required");
        assert!(out.contains("dc:title"), "title element required (empty)");
        assert!(out.contains("<manifest"), "other elements must survive");
    }

    #[test]
    fn ncx_head_is_blanked() {
        let xml = r#"<?xml version="1.0"?>
<ncx xmlns="n">
  <head>
    <meta name="dtb:uid" content="secret-identifier"/>
    <meta name="dtb:generator" content="Calibre 5.0"/>
  </head>
  <docTitle><text>Title</text></docTitle>
</ncx>"#;
        let out = clean_head_only(xml).unwrap();
        assert!(
            !out.contains("secret-identifier"),
            "uid must be blanked: {out}"
        );
        assert!(!out.contains("Calibre"), "generator must be blanked: {out}");
        assert!(out.contains("docTitle"), "rest of doc must survive");
    }

    #[test]
    fn uuid_has_correct_version_and_variant() {
        let mut rng = rand::rng();
        let urn = generate_urn_uuid_v4(&mut rng);
        // Format is urn:uuid:XXXXXXXX-XXXX-4XXX-YXXX-XXXXXXXXXXXX
        assert!(urn.starts_with("urn:uuid:"));
        let core = &urn["urn:uuid:".len()..];
        let parts: Vec<&str> = core.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert!(parts[2].starts_with('4'), "version 4 expected: {core}");
        let variant = parts[3].chars().next().unwrap();
        assert!(
            matches!(variant, '8' | '9' | 'a' | 'b'),
            "RFC 4122 variant expected: {core}"
        );
    }
}
