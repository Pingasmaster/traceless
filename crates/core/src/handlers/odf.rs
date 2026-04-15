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

use crate::error::CoreError;

use super::xml_util::{local_name, sort_xml_attributes};

/// Clean a single ODF XML member. Dispatches on the file name.
///
/// # Errors
///
/// Surfaces any `quick_xml` parse/write failure instead of silently
/// falling back to the original bytes. A malformed `content.xml`
/// otherwise ships unstripped through the cleaner, defeating the
/// tracked-changes removal.
pub fn clean_xml_member(path: &str, xml: &str) -> Result<String, CoreError> {
    let mut out = xml.to_string();
    if path.ends_with("content.xml") {
        out = strip_tracked_changes(&out)?;
    }
    sort_xml_attributes(&out)
}

/// Drop every `<text:tracked-changes>` element (and its children) from
/// the given XML. ODF stores the full history of inserts/deletes there,
/// which is exactly the kind of leak mat2 removes.
///
/// Returns `Err` on any XML parse/write failure so a crafted-malformed
/// `content.xml` cannot slip past the cleaner: the old behaviour was to
/// return the original bytes, which meant a broken `<text:tracked-changes>`
/// subtree (e.g. unbalanced tags) survived unmodified.
fn strip_tracked_changes(xml: &str) -> Result<String, CoreError> {
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
                writer
                    .write_event(Event::Start(e.clone()))
                    .map_err(|err| CoreError::CleanError {
                        path: std::path::PathBuf::new(),
                        detail: format!("ODF content write error: {err}"),
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
                        path: std::path::PathBuf::new(),
                        detail: format!("ODF content write error: {err}"),
                    })?;
            }
            Ok(Event::Empty(ref e)) => {
                if skip_depth > 0 {
                    continue;
                }
                if local_name(e) == "tracked-changes" {
                    continue;
                }
                writer
                    .write_event(Event::Empty(e.clone()))
                    .map_err(|err| CoreError::CleanError {
                        path: std::path::PathBuf::new(),
                        detail: format!("ODF content write error: {err}"),
                    })?;
            }
            Ok(Event::Text(ref t)) => {
                if skip_depth == 0 {
                    writer
                        .write_event(Event::Text(t.clone()))
                        .map_err(|err| CoreError::CleanError {
                            path: std::path::PathBuf::new(),
                            detail: format!("ODF content write error: {err}"),
                        })?;
                }
            }
            Ok(Event::Eof) => break,
            Ok(other) => {
                if skip_depth == 0 {
                    writer
                        .write_event(other)
                        .map_err(|err| CoreError::CleanError {
                            path: std::path::PathBuf::new(),
                            detail: format!("ODF content write error: {err}"),
                        })?;
                }
            }
            Err(err) => {
                return Err(CoreError::CleanError {
                    path: std::path::PathBuf::new(),
                    detail: format!("ODF content XML parse error: {err}"),
                });
            }
        }
    }

    String::from_utf8(writer.into_inner().into_inner()).map_err(|err| CoreError::CleanError {
        path: std::path::PathBuf::new(),
        detail: format!("ODF cleaned output was not UTF-8: {err}"),
    })
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
        let out = strip_tracked_changes(xml).unwrap();
        assert!(
            !out.contains("tracked-changes"),
            "wrapper must be gone: {out}"
        );
        assert!(!out.contains("deleted"), "deleted text must be gone: {out}");
        assert!(out.contains("kept"), "other body text must survive: {out}");
    }

    #[test]
    fn strip_tracked_changes_surfaces_parse_errors_instead_of_silent_passthrough() {
        // Unbalanced `<text:tracked-changes>` - the inner contents
        // include a stray closing tag so quick_xml errors out. Before
        // the F2 fix this function returned the *original* bytes,
        // meaning the tracked-changes block sailed through the
        // cleaner. The new contract is to surface the parse error as
        // `CoreError::CleanError` so the top-level clean aborts.
        let xml = r#"<office:document-content xmlns:office="o" xmlns:text="t">
            <office:body>
                <office:text>
                    <text:tracked-changes></broken>
                </office:text>
            </office:body>
        </office:document-content>"#;
        assert!(strip_tracked_changes(xml).is_err());
    }
}
