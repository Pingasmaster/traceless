//! Minimal XMP and IPTC parsers.
//!
//! Used by every handler that needs to surface individual metadata
//! fields out of an opaque metadata packet:
//! - `pdf.rs` parses the XMP stream in `/Metadata`.
//! - `image.rs` parses the XMP APP1 segment and the IPTC 8BIM resource
//!   inside an APP13 segment of a JPEG.
//!
//! These are deliberately pragmatic rather than spec-complete parsers:
//! they extract the fields mat2's UI reports and enough other common
//! ones to cover the typical leak surface. If a real RDF/XMP parser
//! is ever needed, switching to `xmp-toolkit` or a similar crate is a
//! drop-in replacement.

use crate::metadata::MetadataItem;

/// XMP namespace prefixes whose fields we surface. Anything outside
/// this set is silently ignored — we don't want to flood the UI with
/// `x:`/`rdf:` structural markers.
const XMP_PREFIXES: &[&str] = &[
    "dc",
    "xmp",
    "xmpMM",
    "xmpRights",
    "pdf",
    "pdfx",
    "photoshop",
    "Iptc4xmpCore",
    "aux",
    "cc",
    "exif",
    "tiff",
    "crs",
    "stEvt",
    "stRef",
    "lr",
    "plus",
];

/// Extract `(key, value)` pairs from an XMP packet. The input is
/// typically the raw stream bytes of a PDF `/Metadata` object, or the
/// body of a JPEG APP1 segment that starts with
/// `http://ns.adobe.com/xap/1.0/\0`.
///
/// Entries whose "value" is itself XML (RDF containers like `rdf:Bag`)
/// are skipped — the caller will see their child `rdf:li` elements
/// instead.
#[must_use]
pub fn parse_xmp_fields(bytes: &[u8]) -> Vec<MetadataItem> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    let pairs = find_xmp_pairs(text);
    for (qname, value) in pairs {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.contains('<') {
            // This "value" contains nested elements — skip, the caller
            // will see the inner rdf:li entries separately.
            continue;
        }
        out.push(MetadataItem {
            key: format!("XMP {qname}"),
            value: trimmed.to_string(),
        });
    }
    out
}

/// Walk an XMP document and return `(qualified_name, raw_inner_text)`
/// pairs for every element whose prefix is in `XMP_PREFIXES`.
///
/// Implemented without a real XML parser — see the module doc comment
/// for why this is OK.
fn find_xmp_pairs(text: &str) -> Vec<(String, String)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // Comment: `<!-- ... -->`
        if bytes[i..].starts_with(b"<!--") {
            if let Some(end) = find_bytes(&bytes[i..], b"-->") {
                i += end + 3;
            } else {
                break;
            }
            continue;
        }
        // Processing instruction: `<? ... ?>`
        if bytes[i..].starts_with(b"<?") {
            if let Some(end) = find_bytes(&bytes[i..], b"?>") {
                i += end + 2;
            } else {
                break;
            }
            continue;
        }
        // Close tag — not an entry point
        if bytes[i..].starts_with(b"</") {
            if let Some(end) = bytes[i..].iter().position(|&b| b == b'>') {
                i += end + 1;
            } else {
                break;
            }
            continue;
        }

        // Opening tag — extract qualified name
        let name_start = i + 1;
        let mut j = name_start;
        while j < bytes.len()
            && !matches!(bytes[j], b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/')
        {
            j += 1;
        }
        if j >= bytes.len() {
            break;
        }

        let Ok(qname) = std::str::from_utf8(&bytes[name_start..j]) else {
            i = j + 1;
            continue;
        };
        let prefix = qname.split(':').next().unwrap_or("");
        let is_interesting = XMP_PREFIXES.contains(&prefix);

        // Walk to the end of the opening tag `>` (or `/>`).
        let mut k = j;
        while k < bytes.len() && bytes[k] != b'>' {
            k += 1;
        }
        if k >= bytes.len() {
            break;
        }
        let self_closing = k > 0 && bytes[k - 1] == b'/';
        let content_start = k + 1;

        if self_closing || !is_interesting {
            i = content_start;
            continue;
        }

        // Find matching `</qname>`
        let close_tag = format!("</{qname}>");
        let close_tag_bytes = close_tag.as_bytes();
        let Some(rel) = find_bytes(&bytes[content_start..], close_tag_bytes) else {
            break;
        };
        let content_end = content_start + rel;
        let value = String::from_utf8_lossy(&bytes[content_start..content_end]).into_owned();
        out.push((qname.to_string(), value));
        i = content_end + close_tag_bytes.len();
    }
    out
}

// ============================================================
// IPTC (IIM records inside a Photoshop 8BIM resource)
// ============================================================

/// Parse a JPEG APP13 segment body starting at `Photoshop 3.0\0` and
/// return IPTC IIM records as `(field-name, value)` pairs.
///
/// IPTC IIM is a sequence of records, each prefixed with a 5-byte
/// header: `0x1C` `<record>` `<dataset>` + 2-byte big-endian length,
/// followed by `length` bytes of value. We only decode the record-2
/// (application) datasets that are commonly used for fingerprinting —
/// the others are camera-internal and not user-visible.
#[must_use]
pub fn parse_iptc_8bim(bytes: &[u8]) -> Vec<MetadataItem> {
    let mut out = Vec::new();

    // Locate the IPTC 8BIM resource ID 0x0404 inside the Photoshop
    // resource block.
    // Format: "8BIM" + u16 resource id + pascal string (padded to
    // even length) + u32 size + size bytes of IPTC data.
    let mut i = 0usize;
    while i + 12 <= bytes.len() {
        if &bytes[i..i + 4] != b"8BIM" {
            i += 1;
            continue;
        }
        let resource_id = u16::from_be_bytes([bytes[i + 4], bytes[i + 5]]);
        // Pascal string
        let name_len = bytes[i + 6] as usize;
        let name_field_len = 1 + name_len;
        let padded = name_field_len + (name_field_len & 1);
        let size_offset = i + 6 + padded;
        if size_offset + 4 > bytes.len() {
            break;
        }
        let data_len = u32::from_be_bytes([
            bytes[size_offset],
            bytes[size_offset + 1],
            bytes[size_offset + 2],
            bytes[size_offset + 3],
        ]) as usize;
        let data_offset = size_offset + 4;
        if data_offset + data_len > bytes.len() {
            break;
        }
        if resource_id == 0x0404 {
            let iptc_data = &bytes[data_offset..data_offset + data_len];
            out.extend(parse_iim_stream(iptc_data));
        }
        // Advance past this resource (padded to even length).
        let next = data_offset + data_len + (data_len & 1);
        if next <= i {
            break;
        }
        i = next;
    }

    out
}

/// Walk a sequence of IIM records and return user-visible fields.
fn parse_iim_stream(bytes: &[u8]) -> Vec<MetadataItem> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 5 <= bytes.len() {
        if bytes[i] != 0x1C {
            // Not a record marker; IIM streams are supposed to be
            // densely packed, but some producers pad. Skip byte.
            i += 1;
            continue;
        }
        let record = bytes[i + 1];
        let dataset = bytes[i + 2];
        let len = u16::from_be_bytes([bytes[i + 3], bytes[i + 4]]) as usize;
        let value_start = i + 5;
        if value_start + len > bytes.len() {
            break;
        }
        let value_bytes = &bytes[value_start..value_start + len];

        if record == 2
            && let Some(name) = iim_record2_name(dataset)
        {
            // IPTC values are usually UTF-8 or ISO-8859-1; lossy
            // conversion is fine for display.
            let value = String::from_utf8_lossy(value_bytes).into_owned();
            out.push(MetadataItem {
                key: format!("IPTC {name}"),
                value,
            });
        }
        i = value_start + len;
    }
    out
}

/// The IIM record-2 datasets that are user-visible leaks. mat2 uses
/// exiftool to parse these; we hardcode the common ones. Full list at
/// <https://www.iptc.org/std/photometadata/documentation/userguide/>.
const fn iim_record2_name(dataset: u8) -> Option<&'static str> {
    Some(match dataset {
        5 => "Object Name",
        10 => "Urgency",
        15 => "Category",
        20 => "Supplemental Categories",
        22 => "Fixture Identifier",
        25 => "Keywords",
        40 => "Special Instructions",
        55 => "Date Created",
        60 => "Time Created",
        62 => "Digital Creation Date",
        63 => "Digital Creation Time",
        65 => "Originating Program",
        70 => "Program Version",
        80 => "By-line",
        85 => "By-line Title",
        90 => "City",
        92 => "Sub-location",
        95 => "Province / State",
        100 => "Country Code",
        101 => "Country",
        103 => "Original Transmission Reference",
        105 => "Headline",
        110 => "Credit",
        115 => "Source",
        116 => "Copyright Notice",
        118 => "Contact",
        120 => "Caption",
        122 => "Writer / Editor",
        130 => "Image Type",
        131 => "Image Orientation",
        135 => "Language Identifier",
        _ => return None,
    })
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xmp_extracts_dc_creator_and_xmp_creator_tool() {
        let xmp = br#"<?xpacket begin=""?><x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description>
<dc:creator xmlns:dc="http://purl.org/dc/elements/1.1/">Jane Doe</dc:creator>
<xmp:CreatorTool xmlns:xmp="http://ns.adobe.com/xap/1.0/">SecretCam 2.0</xmp:CreatorTool>
<photoshop:City xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/">Paris</photoshop:City>
</rdf:Description>
</rdf:RDF></x:xmpmeta><?xpacket end=""?>"#;
        let items = parse_xmp_fields(xmp);
        let dump = format!("{items:?}");
        assert!(dump.contains("dc:creator"), "{dump}");
        assert!(dump.contains("Jane Doe"), "{dump}");
        assert!(dump.contains("xmp:CreatorTool"), "{dump}");
        assert!(dump.contains("SecretCam"), "{dump}");
        assert!(dump.contains("photoshop:City"), "{dump}");
        assert!(dump.contains("Paris"), "{dump}");
    }

    #[test]
    fn xmp_ignores_unknown_namespaces() {
        let xmp = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description>
<foo:bar xmlns:foo="urn:foo">ignored</foo:bar>
<dc:title xmlns:dc="http://purl.org/dc/elements/1.1/">shown</dc:title>
</rdf:Description>
</rdf:RDF></x:xmpmeta>"#;
        let items = parse_xmp_fields(xmp);
        let dump = format!("{items:?}");
        assert!(!dump.contains("foo:bar"));
        assert!(dump.contains("dc:title"));
        assert!(dump.contains("shown"));
    }

    #[test]
    fn iptc_parses_byline_and_caption() {
        // Build a minimal Photoshop 8BIM resource with a single IPTC
        // data block containing record 2:80 (By-line) = "Alice" and
        // record 2:120 (Caption) = "vacation photo".
        let mut iim = Vec::new();
        // By-line
        iim.extend_from_slice(&[0x1C, 2, 80, 0x00, 0x05]);
        iim.extend_from_slice(b"Alice");
        // Caption
        iim.extend_from_slice(&[0x1C, 2, 120, 0x00, 0x0E]);
        iim.extend_from_slice(b"vacation photo");

        let mut app13 = Vec::new();
        app13.extend_from_slice(b"8BIM");
        app13.extend_from_slice(&0x0404u16.to_be_bytes()); // resource id
        app13.push(0x00); // pascal string length
        app13.push(0x00); // pad to even
        app13.extend_from_slice(&(iim.len() as u32).to_be_bytes());
        app13.extend_from_slice(&iim);

        let items = parse_iptc_8bim(&app13);
        let dump = format!("{items:?}");
        assert!(dump.contains("By-line"), "{dump}");
        assert!(dump.contains("Alice"), "{dump}");
        assert!(dump.contains("Caption"), "{dump}");
        assert!(dump.contains("vacation photo"), "{dump}");
    }
}
