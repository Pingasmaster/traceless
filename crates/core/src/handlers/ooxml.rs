//! Deep-clean helpers for Office Open XML (DOCX, XLSX, PPTX).
//!
//! This module is a line-for-line port of the MS-Office-specific parts of
//! `mat2/libmat2/office.py` (`MSOfficeParser`). Each public `clean_*`
//! function takes the raw XML of one archive member and returns a new XML
//! string with the metadata / fingerprinting vectors removed. The callers
//! in `handlers::document` dispatch based on the member path.
//!
//! Coverage (see mat2 office.py for the upstream behavior):
//! - `__remove_rsid`        → removes `w:rsid*` elements and attributes
//! - `__remove_nsid`        → removes `w:nsid` elements
//! - `__remove_revisions`   → drops `w:del`, promotes `w:ins` children
//! - `__remove_document_comment_meta` → drops `commentRangeStart/End`, `commentReference`
//! - `__randomize_creationId` → fresh random `p14:creationId` values
//! - `__randomize_sldMasterId` → fresh random slide-master ids
//! - `_sort_xml_attributes` → via `handlers::xml_util::sort_xml_attributes`
//! - `mc:Ignorable` strip   → final byte-level regex pass

use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use quick_xml::writer::Writer;
use rand::{Rng, RngExt};
use std::io::Cursor;

use super::xml_util::{local_name, sort_xml_attributes};

/// Paths that are *always* replaced with a minimal stub, regardless of
/// what's inside. The content of docProps/core.xml and docProps/app.xml
/// is 100% metadata — every field there is a fingerprint.
#[must_use]
pub fn stub_for_path(path: &str) -> Option<&'static str> {
    match path {
        "docProps/core.xml" => Some(CORE_STUB),
        "docProps/app.xml" => Some(APP_STUB),
        "docProps/custom.xml" => Some(CUSTOM_STUB),
        _ => None,
    }
}

const CORE_STUB: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
    "<cp:coreProperties ",
    "xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" ",
    "xmlns:dc=\"http://purl.org/dc/elements/1.1/\" ",
    "xmlns:dcterms=\"http://purl.org/dc/terms/\"/>",
);

const APP_STUB: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
    "<Properties ",
    "xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\"/>",
);

const CUSTOM_STUB: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
    "<Properties ",
    "xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/custom-properties\"/>",
);

/// Full pipeline applied to every `.xml` / `.xml.rels` member that doesn't
/// have a stub: remove rsid, nsid, revisions, comment ranges; randomize
/// creationIds; sort attributes; strip `mc:Ignorable`.
#[must_use]
pub fn clean_xml_member(path: &str, xml: &str) -> String {
    // 1. Element-level cleanups
    let mut out = strip_fingerprints(xml);

    // 2. word/document.xml specifically: drop tracked changes + comment refs
    if path.ends_with("word/document.xml") {
        out = strip_revisions(&out);
        out = strip_comment_refs(&out);
    }

    // 3. presentation.xml specifically: randomize slide master ids
    if path.ends_with("ppt/presentation.xml") {
        out = randomize_sld_master_ids(&out);
    }

    // 4. attribute ordering
    out = sort_xml_attributes(&out);

    // 5. `mc:Ignorable` is byte-level — see mat2 office.py line 515
    strip_mc_ignorable(&out)
}

/// Strip:
/// - elements whose local name matches `rsid*` or equals `nsid`
/// - attributes on *any* element whose local name matches `rsid*`
/// - randomize `p14:creationId` values
fn strip_fingerprints(xml: &str) -> String {
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut rng = rand::rng();
    let mut skip_depth: usize = 0;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                if skip_depth > 0 {
                    skip_depth += 1;
                    continue;
                }
                let name = local_name(e);
                if is_fingerprint_element(&name) {
                    skip_depth = 1;
                    continue;
                }
                let rewritten = rewrite_attributes(e, &mut rng);
                if writer.write_event(Event::Start(rewritten)).is_err() {
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
                let name = local_name(e);
                if is_fingerprint_element(&name) {
                    continue;
                }
                let rewritten = rewrite_attributes(e, &mut rng);
                if writer.write_event(Event::Empty(rewritten)).is_err() {
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

fn is_fingerprint_element(local: &str) -> bool {
    let lower = local.to_ascii_lowercase();
    // rsid* elements (rsid, rsidR, rsidRPr, rsidP, rsids, rsidRoot, …)
    if lower.starts_with("rsid") {
        return true;
    }
    // nsid element
    if lower == "nsid" {
        return true;
    }
    false
}

fn rewrite_attributes(start: &BytesStart<'_>, rng: &mut impl Rng) -> BytesStart<'static> {
    let name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let mut out = BytesStart::new(name);
    let is_creation_id = local_name(start) == "creationId";

    for attr in start.attributes().filter_map(Result::ok) {
        let key_bytes = attr.key.as_ref().to_vec();
        let key_str = String::from_utf8_lossy(&key_bytes).to_ascii_lowercase();

        // Attribute rsid removal (e.g. w:rsidR="00F12345")
        let local_attr = key_str
            .rsplit_once(':')
            .map_or(key_str.as_str(), |(_, l)| l);
        if local_attr.starts_with("rsid") {
            continue;
        }

        // creationId randomization: mat2 writes a fresh random u32 on every
        // element that has a creationId attribute, which is the full
        // fingerprinting vector for newer PPT files.
        if is_creation_id && local_attr == "val" {
            let new_val = format!("{}", rng.random::<u32>());
            out.push_attribute((key_bytes.as_slice(), new_val.as_bytes()));
            continue;
        }

        let value_bytes = attr.value.into_owned();
        out.push_attribute(Attribute {
            key: quick_xml::name::QName(&key_bytes),
            value: std::borrow::Cow::Owned(value_bytes),
        });
    }
    out
}

/// `word/document.xml` stores tracked changes as `<w:del>` and `<w:ins>`
/// elements. We drop `w:del` entirely (including its children — the
/// deleted text). For `w:ins` we promote its children so the *new* text is
/// preserved but the authorship of the insertion is lost.
fn strip_revisions(xml: &str) -> String {
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut del_depth: usize = 0;
    let mut ins_depth: usize = 0;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let ln = local_name(e);
                if del_depth > 0 {
                    del_depth += 1;
                    continue;
                }
                if ln == "del" {
                    del_depth = 1;
                    continue;
                }
                if ln == "ins" {
                    ins_depth += 1;
                    continue; // swallow the wrapper, keep the children
                }
                if writer.write_event(Event::Start(e.clone())).is_err() {
                    return xml.to_string();
                }
            }
            Ok(Event::End(ref e)) => {
                let ln = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let ln = ln.rsplit_once(':').map_or(ln.as_str(), |(_, l)| l);
                if del_depth > 0 {
                    del_depth -= 1;
                    continue;
                }
                if ln == "ins" && ins_depth > 0 {
                    ins_depth -= 1;
                    continue;
                }
                if writer.write_event(Event::End(e.clone())).is_err() {
                    return xml.to_string();
                }
            }
            Ok(Event::Empty(ref e)) => {
                let ln = local_name(e);
                if del_depth > 0 || ln == "del" {
                    continue;
                }
                if ln == "ins" {
                    // self-closing ins has nothing to promote
                    continue;
                }
                if writer.write_event(Event::Empty(e.clone())).is_err() {
                    return xml.to_string();
                }
            }
            Ok(Event::Text(ref t)) => {
                if del_depth == 0 && writer.write_event(Event::Text(t.clone())).is_err() {
                    return xml.to_string();
                }
            }
            Ok(Event::Eof) => break,
            Ok(other) => {
                if del_depth == 0 && writer.write_event(other).is_err() {
                    return xml.to_string();
                }
            }
            Err(_) => return xml.to_string(),
        }
    }

    String::from_utf8(writer.into_inner().into_inner()).unwrap_or_else(|_| xml.to_string())
}

/// Drop `w:commentRangeStart`, `w:commentRangeEnd`, `w:commentReference`.
/// The comment bodies themselves live in `word/comments*.xml` which is
/// already filtered out at the archive level by `zip_util::is_office_junk_path`.
fn strip_comment_refs(xml: &str) -> String {
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));

    let drop = |name: &str| {
        matches!(
            name,
            "commentRangeStart" | "commentRangeEnd" | "commentReference"
        )
    };

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) if drop(&local_name(e)) => {
                // paired <w:commentRangeStart>…</w:commentRangeStart> —
                // swallow both ends
                let end_name = e.to_end().into_owned();
                let _ = reader.read_to_end(end_name.name());
            }
            Ok(Event::Empty(ref e)) if drop(&local_name(e)) => {}
            Ok(Event::Eof) => break,
            Ok(other) => {
                if writer.write_event(other).is_err() {
                    return xml.to_string();
                }
            }
            Err(_) => return xml.to_string(),
        }
    }

    String::from_utf8(writer.into_inner().into_inner()).unwrap_or_else(|_| xml.to_string())
}

/// Rewrite every `<p:sldMasterId id="N" .../>` with a fresh random u32.
fn randomize_sld_master_ids(xml: &str) -> String {
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut rng = rand::rng();

    loop {
        match reader.read_event() {
            Ok(Event::Empty(ref e)) if local_name(e) == "sldMasterId" => {
                let rewritten = rewrite_id_attribute(e, "id", &mut rng);
                if writer.write_event(Event::Empty(rewritten)).is_err() {
                    return xml.to_string();
                }
            }
            Ok(Event::Start(ref e)) if local_name(e) == "sldMasterId" => {
                let rewritten = rewrite_id_attribute(e, "id", &mut rng);
                if writer.write_event(Event::Start(rewritten)).is_err() {
                    return xml.to_string();
                }
            }
            Ok(Event::Eof) => break,
            Ok(other) => {
                if writer.write_event(other).is_err() {
                    return xml.to_string();
                }
            }
            Err(_) => return xml.to_string(),
        }
    }

    String::from_utf8(writer.into_inner().into_inner()).unwrap_or_else(|_| xml.to_string())
}

fn rewrite_id_attribute(
    start: &BytesStart<'_>,
    target_local: &str,
    rng: &mut impl Rng,
) -> BytesStart<'static> {
    let name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let mut out = BytesStart::new(name);
    for attr in start.attributes().filter_map(Result::ok) {
        let key_bytes = attr.key.as_ref().to_vec();
        let key_str = String::from_utf8_lossy(&key_bytes);
        let local = key_str
            .rsplit_once(':')
            .map_or_else(|| key_str.as_ref(), |(_, l)| l);
        if local == target_local {
            let new_val = format!("{}", rng.random::<u32>());
            out.push_attribute((key_bytes.as_slice(), new_val.as_bytes()));
        } else {
            let value = attr.value.into_owned();
            out.push_attribute(Attribute {
                key: quick_xml::name::QName(&key_bytes),
                value: std::borrow::Cow::Owned(value),
            });
        }
    }
    out
}

/// mat2's final byte-level regex that strips `mc:Ignorable="…"`. Doing
/// this with quick-xml would require tracking element state; a regex-
/// style replace is what mat2 does and is sufficient because we already
/// produced the output ourselves via `sort_xml_attributes`.
///
/// Must loop until no more matches - mat2 uses `re.sub` which replaces
/// every occurrence, and valid OOXML can carry multiple `mc:Ignorable`
/// attributes (e.g. nested `mc:AlternateContent` elements). A single-
/// shot strip would leave every occurrence past the first as a producer
/// fingerprint.
fn strip_mc_ignorable(xml: &str) -> String {
    let needle = b"mc:Ignorable=\"";
    let mut buf = xml.as_bytes().to_vec();
    loop {
        let Some(start) = find_bytes(&buf, needle) else {
            break;
        };
        let after_open = start + needle.len();
        let Some(close_rel) = buf[after_open..].iter().position(|&b| b == b'"') else {
            break;
        };
        let close_abs = after_open + close_rel + 1;
        // Also swallow a preceding space if there is one, to keep the
        // output well-formed (`<tag  x="y">` → `<tag x="y">`).
        let prefix_end = if start > 0 && buf[start - 1] == b' ' {
            start - 1
        } else {
            start
        };
        buf.drain(prefix_end..close_abs);
    }
    String::from_utf8(buf).unwrap_or_else(|_| xml.to_string())
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn strip_rsid_removes_elements_and_attributes() {
        let xml = r#"<w:p xmlns:w="w" w:rsidR="00F12345" w:rsidRDefault="00AABBCC">
            <w:rsids><w:rsidRoot w:val="00112233"/></w:rsids>
            <w:t>hello</w:t>
        </w:p>"#;
        let out = strip_fingerprints(xml);
        assert!(
            !out.contains("rsidR"),
            "rsid attributes must be removed: {out}"
        );
        assert!(
            !out.contains("rsids"),
            "rsid elements must be removed: {out}"
        );
        assert!(
            out.contains("hello"),
            "non-rsid content must survive: {out}"
        );
    }

    #[test]
    fn strip_revisions_drops_del_keeps_ins_children() {
        let xml = r#"<doc xmlns:w="w">
            <w:p>
                <w:del w:id="1" w:author="alice"><w:t>deleted</w:t></w:del>
                <w:ins w:id="2" w:author="bob"><w:t>added</w:t></w:ins>
            </w:p>
        </doc>"#;
        let out = strip_revisions(xml);
        assert!(!out.contains("deleted"), "deleted text must be gone: {out}");
        assert!(out.contains("added"), "inserted text must survive: {out}");
        assert!(!out.contains("w:del"), "del wrapper must be gone");
    }

    #[test]
    fn strip_mc_ignorable_removes_attribute() {
        let xml = r#"<doc xmlns:mc="x" mc:Ignorable="w14 w15"><p/></doc>"#;
        let out = strip_mc_ignorable(xml);
        assert!(
            !out.contains("mc:Ignorable"),
            "mc:Ignorable must be stripped: {out}"
        );
    }

    #[test]
    fn strip_mc_ignorable_removes_every_occurrence() {
        // mat2 uses `re.sub` which replaces every match, and valid OOXML
        // can carry more than one mc:Ignorable (nested mc:AlternateContent
        // wrappers). A single-shot strip would leave the second one as a
        // producer fingerprint; this test pins the fix.
        let xml = concat!(
            r#"<root xmlns:mc="x" mc:Ignorable="w14">"#,
            r#"<mc:AlternateContent mc:Ignorable="w15"/>"#,
            r#"<mc:AlternateContent mc:Ignorable="w16 w17"/>"#,
            r#"</root>"#,
        );
        let out = strip_mc_ignorable(xml);
        assert!(
            !out.contains("mc:Ignorable"),
            "every mc:Ignorable must be stripped, got: {out}"
        );
        assert!(
            out.contains("mc:AlternateContent"),
            "wrappers must survive: {out}"
        );
    }

    #[test]
    fn strip_comment_refs_drops_range_markers() {
        let xml = r#"<doc xmlns:w="w">
            <w:commentRangeStart w:id="1"/>
            <w:t>body</w:t>
            <w:commentRangeEnd w:id="1"/>
            <w:commentReference w:id="1"/>
        </doc>"#;
        let out = strip_comment_refs(xml);
        assert!(!out.contains("commentRange"));
        assert!(!out.contains("commentReference"));
        assert!(out.contains("body"));
    }
}
