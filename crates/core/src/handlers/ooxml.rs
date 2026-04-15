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
use std::collections::HashSet;
use std::hash::BuildHasher;
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

/// Rewrite `[Content_Types].xml` to drop every `<Override PartName="/X"/>`
/// whose target `X` is not in the `kept_parts` set.
///
/// This pass is critical because `is_office_junk_path` deletes parts like
/// `word/theme/theme1.xml`, `word/numbering.xml`, `word/webSettings.xml`
/// and `customXml/*` without rewriting the package manifest. A DOCX whose
/// `[Content_Types].xml` points at parts that are no longer in the zip is
/// malformed per ECMA-376. Word and LibreOffice are lenient and auto-
/// repair, but strict consumers (python-docx, for example) reject it.
///
/// `Default Extension="..."` entries are content-type wildcards and are
/// preserved unchanged. Only explicit `Override` entries are filtered.
#[must_use]
pub fn rewrite_content_types<S: BuildHasher>(xml: &str, kept_parts: &HashSet<String, S>) -> String {
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));

    loop {
        match reader.read_event() {
            Ok(Event::Empty(ref e)) if local_name(e) == "Override" => {
                if !override_part_name_is_dropped(e, kept_parts)
                    && writer.write_event(Event::Empty(e.clone())).is_err()
                {
                    return xml.to_string();
                }
            }
            Ok(Event::Start(ref e)) if local_name(e) == "Override" => {
                // Paired `<Override ...></Override>` - read to the matching
                // end so nested text (shouldn't happen in practice) and the
                // close tag are swallowed with the element if we're dropping
                // it.
                let end_name = e.to_end().into_owned();
                let drop = override_part_name_is_dropped(e, kept_parts);
                if drop {
                    let _ = reader.read_to_end(end_name.name());
                } else if writer.write_event(Event::Start(e.clone())).is_err() {
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

/// Return `true` if an `<Override>` element should be removed because its
/// `PartName` points at a part that isn't in `kept_parts`. An Override
/// without a `PartName` attribute is left alone (malformed input; we
/// don't silently drop it).
fn override_part_name_is_dropped<S: BuildHasher>(
    e: &BytesStart<'_>,
    kept: &HashSet<String, S>,
) -> bool {
    for attr in e.attributes().filter_map(Result::ok) {
        if attr.key.as_ref() == b"PartName" {
            let raw = String::from_utf8_lossy(&attr.value).into_owned();
            let normalized = raw.trim_start_matches('/').to_string();
            return !kept.contains(&normalized);
        }
    }
    false
}

/// Rewrite a `.rels` file to drop every `<Relationship Target="Y"/>`
/// whose resolved target is not in the `kept_parts` set.
///
/// `rels_path` is the package-relative path of the `.rels` file itself
/// (e.g. `word/_rels/document.xml.rels`), which anchors the resolution
/// of relative `Target` attributes. External relationships
/// (`TargetMode="External"`) point at URLs, not package parts, and are
/// preserved unconditionally.
#[must_use]
pub fn rewrite_rels<S: BuildHasher>(
    xml: &str,
    rels_path: &str,
    kept_parts: &HashSet<String, S>,
) -> String {
    let base = rels_base_for(rels_path);
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));

    loop {
        match reader.read_event() {
            Ok(Event::Empty(ref e)) if local_name(e) == "Relationship" => {
                if !relationship_target_is_dropped(e, &base, kept_parts)
                    && writer.write_event(Event::Empty(e.clone())).is_err()
                {
                    return xml.to_string();
                }
            }
            Ok(Event::Start(ref e)) if local_name(e) == "Relationship" => {
                let end_name = e.to_end().into_owned();
                let drop = relationship_target_is_dropped(e, &base, kept_parts);
                if drop {
                    let _ = reader.read_to_end(end_name.name());
                } else if writer.write_event(Event::Start(e.clone())).is_err() {
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

fn relationship_target_is_dropped<S: BuildHasher>(
    e: &BytesStart<'_>,
    base: &str,
    kept: &HashSet<String, S>,
) -> bool {
    let mut target: Option<String> = None;
    let mut is_external = false;
    for attr in e.attributes().filter_map(Result::ok) {
        match attr.key.as_ref() {
            b"Target" => {
                target = Some(String::from_utf8_lossy(&attr.value).into_owned());
            }
            b"TargetMode" => {
                if attr.value.as_ref() == b"External" {
                    is_external = true;
                }
            }
            _ => {}
        }
    }
    if is_external {
        return false;
    }
    let Some(t) = target else {
        return false; // malformed, leave alone
    };
    let resolved = resolve_rels_target(base, &t);
    !kept.contains(&resolved)
}

/// Compute the package-root-relative directory that a `.rels` file's
/// `Target` attributes are resolved against.
///
/// `_rels/.rels` -> `""` (package root)
/// `word/_rels/document.xml.rels` -> `"word/"`
/// `ppt/slides/_rels/slide1.xml.rels` -> `"ppt/slides/"`
fn rels_base_for(rels_path: &str) -> String {
    // Strip the trailing `_rels/<file>.rels` segment. In every legal
    // OOXML path the `.rels` file lives in a `_rels/` directory whose
    // parent is the logical owner.
    if let Some(idx) = rels_path.rfind("_rels/") {
        rels_path[..idx].to_string()
    } else {
        String::new()
    }
}

/// Resolve a `Relationship/@Target` value against the `.rels` file's base
/// directory, returning the package-root-relative path with `.` / `..`
/// segments collapsed and no leading slash.
fn resolve_rels_target(base: &str, target: &str) -> String {
    let joined = if let Some(abs) = target.strip_prefix('/') {
        abs.to_string()
    } else {
        format!("{base}{target}")
    };
    normalize_path(&joined)
}

/// Collapse `.` and `..` segments in a forward-slash-separated path.
/// A `..` at the root is treated as a no-op (package paths never escape
/// the archive root).
fn normalize_path(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out.join("/")
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

    fn kept_set(entries: &[&str]) -> HashSet<String> {
        entries.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn rewrite_content_types_drops_dangling_overrides() {
        let kept = kept_set(&[
            "word/document.xml",
            "word/styles.xml",
            "docProps/core.xml",
            "docProps/app.xml",
            "[Content_Types].xml",
            "_rels/.rels",
            "word/_rels/document.xml.rels",
        ]);
        let xml = concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">"#,
            r#"<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>"#,
            r#"<Default Extension="xml" ContentType="application/xml"/>"#,
            r#"<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>"#,
            r#"<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>"#,
            r#"<Override PartName="/word/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>"#,
            r#"<Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>"#,
            r#"<Override PartName="/word/webSettings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.webSettings+xml"/>"#,
            r#"<Override PartName="/customXml/item1.xml" ContentType="application/xml"/>"#,
            r#"<Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>"#,
            r#"<Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>"#,
            r#"</Types>"#,
        );
        let out = rewrite_content_types(xml, &kept);

        // Kept parts survive.
        assert!(out.contains(r#"PartName="/word/document.xml""#), "{out}");
        assert!(out.contains(r#"PartName="/word/styles.xml""#), "{out}");
        assert!(out.contains(r#"PartName="/docProps/core.xml""#), "{out}");
        assert!(out.contains(r#"PartName="/docProps/app.xml""#), "{out}");

        // Default wildcards unchanged.
        assert!(out.contains(r#"Default Extension="rels""#), "{out}");
        assert!(out.contains(r#"Default Extension="xml""#), "{out}");

        // Dropped parts are gone.
        assert!(!out.contains("theme1.xml"), "theme1 override leaked: {out}");
        assert!(
            !out.contains("numbering.xml"),
            "numbering override leaked: {out}"
        );
        assert!(
            !out.contains("webSettings.xml"),
            "webSettings override leaked: {out}"
        );
        assert!(
            !out.contains("customXml/item1.xml"),
            "customXml override leaked: {out}"
        );
    }

    #[test]
    fn rewrite_rels_drops_dangling_relationships() {
        let kept = kept_set(&[
            "word/document.xml",
            "word/styles.xml",
            "word/_rels/document.xml.rels",
        ]);
        let xml = concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
            r#"<Relationship Id="rId1" Type=".../styles" Target="styles.xml"/>"#,
            r#"<Relationship Id="rId2" Type=".../theme" Target="theme/theme1.xml"/>"#,
            r#"<Relationship Id="rId3" Type=".../numbering" Target="numbering.xml"/>"#,
            r#"<Relationship Id="rId4" Type=".../webSettings" Target="webSettings.xml"/>"#,
            r#"<Relationship Id="rId5" Type=".../hyperlink" Target="http://example.com" TargetMode="External"/>"#,
            r#"</Relationships>"#,
        );
        let out = rewrite_rels(xml, "word/_rels/document.xml.rels", &kept);

        assert!(out.contains(r#"Target="styles.xml""#), "{out}");
        assert!(
            out.contains(r#"Target="http://example.com""#),
            "external URL relationship dropped: {out}"
        );
        assert!(
            !out.contains(r#"Target="theme/theme1.xml""#),
            "theme rel leaked: {out}"
        );
        assert!(
            !out.contains(r#"Target="numbering.xml""#),
            "numbering rel leaked: {out}"
        );
        assert!(
            !out.contains(r#"Target="webSettings.xml""#),
            "webSettings rel leaked: {out}"
        );
    }

    #[test]
    fn rewrite_rels_resolves_absolute_target() {
        // A `Target` with a leading slash resolves against the package
        // root, not against the .rels file's base directory.
        let kept = kept_set(&["word/document.xml"]);
        let xml = concat!(
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
            r#"<Relationship Id="rId1" Type="x" Target="/word/document.xml"/>"#,
            r#"<Relationship Id="rId2" Type="x" Target="/word/theme/theme1.xml"/>"#,
            r#"</Relationships>"#,
        );
        let out = rewrite_rels(xml, "word/_rels/document.xml.rels", &kept);
        assert!(out.contains(r#"Target="/word/document.xml""#), "{out}");
        assert!(!out.contains("theme1.xml"), "{out}");
    }

    #[test]
    fn resolve_rels_target_handles_relative_absolute_and_dot_dot() {
        // Package root .rels
        assert_eq!(
            resolve_rels_target("", "word/document.xml"),
            "word/document.xml"
        );
        // word/_rels/document.xml.rels with relative target
        assert_eq!(
            resolve_rels_target("word/", "theme/theme1.xml"),
            "word/theme/theme1.xml"
        );
        // Absolute target (leading slash)
        assert_eq!(
            resolve_rels_target("word/", "/docProps/core.xml"),
            "docProps/core.xml"
        );
        // Parent-dir traversal: `word/../word/styles.xml` -> `word/styles.xml`
        assert_eq!(
            resolve_rels_target("word/", "../word/styles.xml"),
            "word/styles.xml"
        );
        // Current-dir `.` is a no-op
        assert_eq!(
            resolve_rels_target("word/", "./styles.xml"),
            "word/styles.xml"
        );
    }

    #[test]
    fn rels_base_for_extracts_parent_directory() {
        assert_eq!(rels_base_for("_rels/.rels"), "");
        assert_eq!(rels_base_for("word/_rels/document.xml.rels"), "word/");
        assert_eq!(
            rels_base_for("ppt/slides/_rels/slide1.xml.rels"),
            "ppt/slides/"
        );
    }

    #[test]
    fn override_drop_predicate_strips_leading_slash() {
        let kept = kept_set(&["word/document.xml"]);
        let keep_elem = BytesStart::from_content(
            r#"Override PartName="/word/document.xml" ContentType="x""#,
            "Override".len(),
        );
        assert!(!override_part_name_is_dropped(&keep_elem, &kept));

        let drop_elem = BytesStart::from_content(
            r#"Override PartName="/word/theme/theme1.xml" ContentType="x""#,
            "Override".len(),
        );
        assert!(override_part_name_is_dropped(&drop_elem, &kept));

        let missing_attr = BytesStart::from_content("Override", "Override".len());
        assert!(
            !override_part_name_is_dropped(&missing_attr, &kept),
            "malformed Override without PartName should be left alone"
        );
    }
}
