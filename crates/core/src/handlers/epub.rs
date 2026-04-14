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

use super::xml_util::local_name;

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
#[must_use]
pub fn clean_opf(xml: &str) -> String {
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
                    if writer.write_event(Event::Start(e.clone())).is_err() {
                        return xml.to_string();
                    }
                    write_minimal_metadata(&mut writer, &uuid);
                    if writer
                        .write_event(Event::End(BytesEnd::new(
                            String::from_utf8_lossy(e.name().as_ref()).into_owned(),
                        )))
                        .is_err()
                    {
                        return xml.to_string();
                    }
                    // Skip until the corresponding </metadata>
                    skip_depth = 1;
                    replaced_metadata = true;
                    continue;
                }
                if writer.write_event(Event::Start(e.clone())).is_err() {
                    return xml.to_string();
                }
            }
            Ok(Event::End(ref e)) => {
                if skip_depth > 0 {
                    skip_depth -= 1;
                    continue;
                }
                if writer.write_event(Event::End(e.clone())).is_err() {
                    return xml.to_string();
                }
            }
            Ok(Event::Empty(ref e)) => {
                if skip_depth > 0 {
                    continue;
                }
                if writer.write_event(Event::Empty(e.clone())).is_err() {
                    return xml.to_string();
                }
            }
            Ok(Event::Text(ref t)) => {
                if skip_depth == 0 && writer.write_event(Event::Text(t.clone())).is_err() {
                    return xml.to_string();
                }
            }
            Ok(Event::Eof) => break,
            Ok(other) => {
                if skip_depth == 0 && writer.write_event(other).is_err() {
                    return xml.to_string();
                }
            }
            Err(_) => return xml.to_string(),
        }
    }

    // If the OPF had no <metadata> block at all, something is wrong with
    // the source file; return the original content so we don't break the
    // reader.
    if !replaced_metadata {
        return xml.to_string();
    }
    String::from_utf8(writer.into_inner().into_inner()).unwrap_or_else(|_| xml.to_string())
}

/// Blank the `<head>` section of an NCX or OPS XML file. We preserve
/// the rest of the document (which contains the actual text / navigation
/// structure).
#[must_use]
pub fn clean_head_only(xml: &str) -> String {
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
                    if writer.write_event(Event::Start(e.clone())).is_err() {
                        return xml.to_string();
                    }
                    if writer
                        .write_event(Event::End(BytesEnd::new(
                            String::from_utf8_lossy(e.name().as_ref()).into_owned(),
                        )))
                        .is_err()
                    {
                        return xml.to_string();
                    }
                    skip_depth = 1;
                    continue;
                }
                if writer.write_event(Event::Start(e.clone())).is_err() {
                    return xml.to_string();
                }
            }
            Ok(Event::End(ref e)) => {
                if skip_depth > 0 {
                    skip_depth -= 1;
                    continue;
                }
                if writer.write_event(Event::End(e.clone())).is_err() {
                    return xml.to_string();
                }
            }
            Ok(Event::Empty(ref e)) => {
                if skip_depth > 0 {
                    continue;
                }
                if writer.write_event(Event::Empty(e.clone())).is_err() {
                    return xml.to_string();
                }
            }
            Ok(Event::Text(ref t)) => {
                if skip_depth == 0 && writer.write_event(Event::Text(t.clone())).is_err() {
                    return xml.to_string();
                }
            }
            Ok(Event::Eof) => break,
            Ok(other) => {
                if skip_depth == 0 && writer.write_event(other).is_err() {
                    return xml.to_string();
                }
            }
            Err(_) => return xml.to_string(),
        }
    }

    String::from_utf8(writer.into_inner().into_inner()).unwrap_or_else(|_| xml.to_string())
}

fn write_minimal_metadata(writer: &mut Writer<Cursor<Vec<u8>>>, urn: &str) {
    // <dc:identifier id="id">urn:uuid:…</dc:identifier>
    let mut ident = BytesStart::new("dc:identifier");
    ident.push_attribute(("id", "id"));
    let _ = writer.write_event(Event::Start(ident.clone()));
    let _ = writer.write_event(Event::Text(BytesText::new(urn)));
    let _ = writer.write_event(Event::End(BytesEnd::new("dc:identifier")));

    // Empty dc:language
    let _ = writer.write_event(Event::Start(BytesStart::new("dc:language")));
    let _ = writer.write_event(Event::End(BytesEnd::new("dc:language")));

    // Empty dc:title
    let _ = writer.write_event(Event::Start(BytesStart::new("dc:title")));
    let _ = writer.write_event(Event::End(BytesEnd::new("dc:title")));
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
        let out = clean_opf(xml);
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
        let out = clean_head_only(xml);
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
