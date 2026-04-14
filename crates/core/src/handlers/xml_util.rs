//! Shared XML helpers used by the OOXML, ODF and EPUB deep-cleaners.
//!
//! The primary purpose is attribute-order normalization: different office
//! producers emit attributes in different orders (MS Word, LibreOffice,
//! OnlyOffice, Pages…), and that order is fingerprintable. By lexicographically
//! sorting attributes on every element we emit, two structurally identical
//! documents produced by different tools collapse to the same byte stream.

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use quick_xml::writer::Writer;
use std::io::Cursor;

/// Rewrite a `BytesStart`/`BytesEmpty` with its attributes sorted
/// lexicographically by raw key bytes. Invalid attributes are dropped,
/// which matches mat2's behavior (they are already malformed).
fn sort_attributes(start: &BytesStart<'_>) -> BytesStart<'static> {
    let mut pairs: Vec<(Vec<u8>, Vec<u8>)> = start
        .attributes()
        .filter_map(std::result::Result::ok)
        .map(|a| (a.key.as_ref().to_vec(), a.value.into_owned()))
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));

    let name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let mut out = BytesStart::new(name);
    for (k, v) in &pairs {
        // push_attribute copies the bytes into the element's internal buf,
        // so references into `pairs` are fine for the duration of the call.
        out.push_attribute((k.as_slice(), v.as_slice()));
    }
    out
}

/// Sort the attributes of every `Start` and `Empty` element in `xml`.
/// Everything else is copied through verbatim.
///
/// Returns the rewritten XML on success. On any parse error, returns the
/// input unchanged — we never want this helper to drop content.
pub fn sort_xml_attributes(xml: &str) -> String {
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let sorted = sort_attributes(e);
                if writer.write_event(Event::Start(sorted)).is_err() {
                    return xml.to_string();
                }
            }
            Ok(Event::Empty(ref e)) => {
                let sorted = sort_attributes(e);
                if writer.write_event(Event::Empty(sorted)).is_err() {
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

/// Return the local name (without namespace prefix) of an element event,
/// as an owned String. Used by the deep-cleaner match arms.
pub fn local_name(start: &BytesStart<'_>) -> String {
    let qname = start.name();
    let bytes = qname.as_ref();
    match bytes.iter().position(|&c| c == b':') {
        Some(idx) => String::from_utf8_lossy(&bytes[idx + 1..]).into_owned(),
        None => String::from_utf8_lossy(bytes).into_owned(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn sort_orders_attributes_lexicographically() {
        let xml = r#"<root z="3" a="1" m="2"/>"#;
        let out = sort_xml_attributes(xml);
        assert!(out.contains(r#"a="1""#));
        assert!(out.contains(r#"m="2""#));
        assert!(out.contains(r#"z="3""#));
        // a must come before m must come before z
        let pa = out.find(r#"a="1""#).unwrap();
        let pm = out.find(r#"m="2""#).unwrap();
        let pz = out.find(r#"z="3""#).unwrap();
        assert!(pa < pm && pm < pz, "attribute order not normalized: {out}");
    }

    #[test]
    fn sort_preserves_text_and_nesting() {
        let xml = r#"<a b="2" a="1"><inner x="y"/>hello</a>"#;
        let out = sort_xml_attributes(xml);
        assert!(out.contains("hello"));
        assert!(out.contains("inner"));
    }

    #[test]
    fn local_name_strips_prefix() {
        let start = BytesStart::new("w:rsidR");
        assert_eq!(local_name(&start), "rsidR");
        let start = BytesStart::new("plain");
        assert_eq!(local_name(&start), "plain");
    }
}
