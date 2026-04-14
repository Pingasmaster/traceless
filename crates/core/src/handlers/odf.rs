//! Deep-clean helpers for OpenDocument Format (ODT, ODS, ODP, ODG, ODF).
//!
//! This mirrors `libmat2/office.py::LibreOfficeParser`:
//! - `meta.xml` is dropped entirely at the archive level (see
//!   `zip_util::is_office_junk_path`).
//! - `Thumbnails/`, `Configurations2/`, `layout-cache` are omitted the
//!   same way.
//! - `content.xml` is stripped of `<text:tracked-changes>`.
//! - All retained XML files get `sort_xml_attributes` applied to kill
//!   producer-order fingerprinting.

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use quick_xml::writer::Writer;
use std::io::Cursor;

use super::xml_util::{local_name, sort_xml_attributes};

/// Clean a single ODF XML member. Dispatches on the file name.
#[must_use]
pub fn clean_xml_member(path: &str, xml: &str) -> String {
    let mut out = xml.to_string();
    if path.ends_with("content.xml") {
        out = strip_tracked_changes(&out);
    }
    sort_xml_attributes(&out)
}

/// Drop every `<text:tracked-changes>` element (and its children) from
/// the given XML. ODF stores the full history of inserts/deletes there,
/// which is exactly the kind of leak mat2 removes.
fn strip_tracked_changes(xml: &str) -> String {
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
                if local_name(e) == "tracked-changes" {
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
                if local_name(e) == "tracked-changes" {
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn tracked_changes_are_dropped() {
        let xml = r#"<office:document-content xmlns:office="o" xmlns:text="t">
            <office:body>
                <office:text>
                    <text:tracked-changes>
                        <text:changed-region><text:p>deleted</text:p></text:changed-region>
                    </text:tracked-changes>
                    <text:p>kept</text:p>
                </office:text>
            </office:body>
        </office:document-content>"#;
        let out = strip_tracked_changes(xml);
        assert!(
            !out.contains("tracked-changes"),
            "wrapper must be gone: {out}"
        );
        assert!(!out.contains("deleted"), "deleted text must be gone: {out}");
        assert!(out.contains("kept"), "other body text must survive: {out}");
    }
}
