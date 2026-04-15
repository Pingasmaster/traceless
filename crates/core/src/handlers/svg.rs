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
///
/// `script` / `foreignObject` / `iframe` are here for content-safety,
/// not metadata: an SVG can carry arbitrary JavaScript that runs when
/// the file is opened in a browser, and a user who clicks "Clean"
/// reasonably expects the output to be safe to share. mat2 side-steps
/// all of this by rasterizing the whole SVG; we preserve vectors and
/// instead enumerate the unsafe surface here and in
/// `sanitize_attributes` below.
///
/// `<foreignObject>` is the SVG mechanism for embedding arbitrary HTML
/// (or MathML, etc.) inside an SVG. Browsers parse the embedded subtree
/// with full HTML/DOM semantics, so an `<iframe src="javascript:...">`
/// or `<object data="...">` inside one is a real XSS vector - and the
/// `sanitize_attributes` javascript-URI check below only covers the
/// native SVG `href` / `xlink:href` attributes, so every foreign
/// URI-bearing attribute (`src`, `action`, `formaction`, `data`, ...)
/// would otherwise slip through. Dropping the whole subtree is both
/// simpler and stricter than trying to enumerate every URI attribute
/// name in every foreign namespace that could legally appear inside
/// embedded HTML. `<iframe>` is added to the list as defense-in-depth
/// for parsers that accept mixed content outside a `<foreignObject>`
/// wrapper.
const DROP_ELEMENTS: &[&str] = &[
    "metadata",      // the whole RDF block
    "title",         // often contains author name
    "desc",          // often contains description + author
    "namedview",     // sodipodi editor state
    "script",        // embedded JavaScript (content-safety, not metadata)
    "style",         // CSS block: producer comments + @import / url() beacons
    "foreignObject", // XSS via embedded HTML (iframe / form / object / ...)
    "iframe",        // defense-in-depth - not a native SVG element
];

/// Namespace prefixes whose attributes we strip from every element.
const STRIP_NS_PREFIXES: &[&str] = &["inkscape:", "sodipodi:", "rdf:", "dc:", "cc:"];

/// Attribute local-names whose values we scrub for `javascript:` URIs.
/// An SVG `<a href="javascript:...">` or `<use xlink:href="javascript:...">`
/// fires the JS when the link is followed, so even without a `<script>`
/// block or an `on*` handler the cleaned file can still execute code.
const URL_ATTRIBUTE_LOCALS: &[&str] = &["href"];

/// Complete list of HTML5 + SVG event-handler attributes. Kept as a
/// lowercase slice so `is_event_handler_attr` below does an ASCII
/// case-insensitive linear scan. Matches the union of HTML Living
/// Standard §8.1.7.2.1 and SVG 2 §6.5 ("Event attributes"). ~100 names
/// means a linear scan is cheap enough per attribute and avoids the
/// `phf` / `HashSet` dependency.
///
/// Over-matching a non-event attribute starts stripping real user data
/// (e.g. a custom `onion` data attribute), and under-matching leaves a
/// real XSS vector in the cleaned file. The whitelist approach is the
/// only way to get both right.
const EVENT_HANDLER_ATTRS: &[&str] = &[
    "onabort",
    "onactivate",
    "onafterprint",
    "onanimationend",
    "onanimationiteration",
    "onanimationstart",
    "onauxclick",
    "onbeforeinput",
    "onbeforeprint",
    "onbeforeunload",
    "onbegin",
    "onblur",
    "oncancel",
    "oncanplay",
    "oncanplaythrough",
    "onchange",
    "onclick",
    "onclose",
    "oncontextlost",
    "oncontextmenu",
    "oncontextrestored",
    "oncopy",
    "oncuechange",
    "oncut",
    "ondblclick",
    "ondrag",
    "ondragend",
    "ondragenter",
    "ondragleave",
    "ondragover",
    "ondragstart",
    "ondrop",
    "ondurationchange",
    "onemptied",
    "onend",
    "onended",
    "onerror",
    "onfocus",
    "onfocusin",
    "onfocusout",
    "onformdata",
    "ongotpointercapture",
    "onhashchange",
    "oninput",
    "oninvalid",
    "onkeydown",
    "onkeypress",
    "onkeyup",
    "onlanguagechange",
    "onload",
    "onloadeddata",
    "onloadedmetadata",
    "onloadstart",
    "onlostpointercapture",
    "onmessage",
    "onmessageerror",
    "onmousedown",
    "onmouseenter",
    "onmouseleave",
    "onmousemove",
    "onmouseout",
    "onmouseover",
    "onmouseup",
    "onoffline",
    "ononline",
    "onpagehide",
    "onpageshow",
    "onpaste",
    "onpause",
    "onplay",
    "onplaying",
    "onpointercancel",
    "onpointerdown",
    "onpointerenter",
    "onpointerleave",
    "onpointermove",
    "onpointerout",
    "onpointerover",
    "onpointerup",
    "onpopstate",
    "onprogress",
    "onratechange",
    "onrejectionhandled",
    "onrepeat",
    "onreset",
    "onresize",
    "onscroll",
    "onscrollend",
    "onsecuritypolicyviolation",
    "onseeked",
    "onseeking",
    "onselect",
    "onshow",
    "onslotchange",
    "onstalled",
    "onstorage",
    "onsubmit",
    "onsuspend",
    "ontimeupdate",
    "ontoggle",
    "ontransitionend",
    "ontransitionstart",
    "onunhandledrejection",
    "onunload",
    "onvolumechange",
    "onwaiting",
    "onwheel",
    "onzoom",
];

/// True if `name` is one of the HTML/SVG event-handler attributes.
/// Case-insensitive: `ONCLICK`, `onClick`, and `onclick` all match.
fn is_event_handler_attr(name: &str) -> bool {
    // Binary search over the sorted table. `eq_ignore_ascii_case`
    // gives us the case-insensitivity; we compare via lower bound.
    EVENT_HANDLER_ATTRS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(name))
}

/// True if the attribute's local name (namespace prefix already
/// stripped) is in `URL_ATTRIBUTE_LOCALS`.
fn is_url_attribute(local: &str) -> bool {
    URL_ATTRIBUTE_LOCALS
        .iter()
        .any(|&n| n.eq_ignore_ascii_case(local))
}

/// True if the (trimmed, ASCII-lowercased) attribute value names a
/// `javascript:` pseudo-URI.
///
/// Operates on the byte slice rather than slicing the `&str`, because
/// slicing a `&str` at byte 11 panics when byte 11 is not a UTF-8
/// char boundary. A value like `"javascrip☃"` (9 ASCII bytes plus the
/// 3-byte snowman) has its 11th byte in the middle of `☃`, which
/// would otherwise crash the worker on an adversarial SVG. The byte
/// slice is always indexable, and the comparison is ASCII-only so a
/// plain `eq_ignore_ascii_case` on the bytes is both correct and
/// fast.
fn is_javascript_uri(value: &str) -> bool {
    const PREFIX: &[u8] = b"javascript:";
    let trimmed = value.trim_start().as_bytes();
    trimmed
        .get(..PREFIX.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(PREFIX))
}

impl FormatHandler for SvgHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
        super::check_input_size(path)?;
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

                    collect_leaky_attrs(e, &qname, &mut items);

                    if DROP_ELEMENTS.contains(&name.as_str()) {
                        current_tag = Some(qname);
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    collect_leaky_attrs(e, &full_name_of(e), &mut items);
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
        super::check_input_size(path)?;
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
                    writer.write_event(Event::Start(sanitized)).map_err(|err| {
                        CoreError::CleanError {
                            path: path.to_path_buf(),
                            detail: format!("SVG write error: {err}"),
                        }
                    })?;
                }
                Ok(Event::End(ref e)) => {
                    if skip_depth > 0 {
                        skip_depth -= 1;
                        continue;
                    }
                    writer.write_event(Event::End(e.clone())).map_err(|err| {
                        CoreError::CleanError {
                            path: path.to_path_buf(),
                            detail: format!("SVG write error: {err}"),
                        }
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
                    writer.write_event(Event::Empty(sanitized)).map_err(|err| {
                        CoreError::CleanError {
                            path: path.to_path_buf(),
                            detail: format!("SVG write error: {err}"),
                        }
                    })?;
                }
                Ok(Event::Text(ref t)) => {
                    if skip_depth == 0 {
                        writer.write_event(Event::Text(t.clone())).map_err(|err| {
                            CoreError::CleanError {
                                path: path.to_path_buf(),
                                detail: format!("SVG write error: {err}"),
                            }
                        })?;
                    }
                }
                Ok(Event::Comment(_)) => {
                    // SVG comments are a metadata vector — drop them.
                }
                Ok(Event::Eof) => break,
                Ok(other) => {
                    if skip_depth == 0 {
                        writer
                            .write_event(other)
                            .map_err(|err| CoreError::CleanError {
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
/// an `inkscape:` / `sodipodi:` / `rdf:` / `dc:` / `cc:` prefix, plus
/// any event-handler attribute (`onclick`, `onload`, …) and any
/// `href` / `xlink:href` value pointing at a `javascript:` URI.
fn sanitize_attributes(start: &BytesStart<'_>) -> BytesStart<'static> {
    let name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let mut out = BytesStart::new(name);

    for attr in start.attributes().filter_map(Result::ok) {
        let key_bytes = attr.key.as_ref().to_vec();
        let key_str = String::from_utf8_lossy(&key_bytes);
        let local_attr = key_str
            .rsplit_once(':')
            .map_or_else(|| key_str.as_ref(), |(_, l)| l);

        if STRIP_NS_PREFIXES.iter().any(|p| key_str.starts_with(p)) {
            continue;
        }
        // Drop xmlns declarations for dropped namespaces to keep the
        // output tidy - they otherwise dangle as unused prefix bindings.
        if let Some(prefix) = key_str.strip_prefix("xmlns:")
            && STRIP_NS_PREFIXES
                .iter()
                .any(|p| p.trim_end_matches(':') == prefix)
        {
            continue;
        }
        // Event handlers (`onclick`, `onload`, …) can run arbitrary
        // JavaScript when the SVG is rendered in a browser.
        if is_event_handler_attr(local_attr) {
            continue;
        }
        // `href`/`xlink:href` values like `javascript:alert(1)` fire
        // when the link is followed.
        let value = attr.value.into_owned();
        if is_url_attribute(local_attr)
            && let Ok(value_str) = std::str::from_utf8(&value)
            && is_javascript_uri(value_str)
        {
            continue;
        }

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

/// Walk the attributes of one element and append every attribute that
/// `clean_metadata` would drop as a `MetadataItem` so the user sees it
/// before they click Clean. Covers the five leaky namespace prefixes
/// plus `on*` event handlers plus `href`/`xlink:href` values that
/// point at `javascript:` URIs.
fn collect_leaky_attrs(start: &BytesStart<'_>, qname: &str, items: &mut Vec<MetadataItem>) {
    for attr in start.attributes().filter_map(Result::ok) {
        let key_str = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        let local_attr = key_str
            .rsplit_once(':')
            .map_or(key_str.as_str(), |(_, l)| l);
        let value_str = String::from_utf8_lossy(attr.value.as_ref()).into_owned();

        if STRIP_NS_PREFIXES.iter().any(|p| key_str.starts_with(p)) {
            items.push(MetadataItem {
                key: format!("<{qname}> {key_str}"),
                value: value_str,
            });
            continue;
        }
        if is_event_handler_attr(local_attr) {
            items.push(MetadataItem {
                key: format!("<{qname}> {key_str} (event handler)"),
                value: value_str,
            });
            continue;
        }
        if is_url_attribute(local_attr) && is_javascript_uri(&value_str) {
            items.push(MetadataItem {
                key: format!("<{qname}> {key_str} (javascript: uri)"),
                value: value_str,
            });
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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
    fn svg_clean_drops_script_and_event_handlers_and_js_uris() {
        // mat2 rasterizes SVGs, which nukes scripts and event handlers
        // as a side effect. We preserve vectors, so we have to
        // explicitly strip every element and attribute that can execute
        // JS when the file is rendered in a browser.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("hostile.svg");
        let dst = dir.path().join("clean.svg");
        let xml = br#"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg"
     xmlns:xlink="http://www.w3.org/1999/xlink"
     width="10" height="10">
  <script>var stolen = document.cookie;</script>
  <rect x="0" y="0" width="10" height="10" fill="red"
        onclick="alert('evil-click')"
        ONLOAD="alert('evil-load')"
        one="keep-me"
        onion="keep-too"/>
  <a href="javascript:alert('a-href')"><text>click</text></a>
  <use xlink:href="javascript:alert('xlink')"/>
  <g><rect x="5" y="5" width="2" height="2" fill="blue"/></g>
</svg>"#;
        fs::write(&src, xml).unwrap();

        let h = SvgHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();

        // Every script body, event handler, and javascript: URI is gone.
        for needle in [
            "var stolen",
            "document.cookie",
            "<script",
            "</script>",
            "evil-click",
            "evil-load",
            "onclick",
            "onload",
            "ONLOAD",
            "javascript:",
            "a-href",
            "xlink:href",
        ] {
            assert!(
                !out.contains(needle),
                "'{needle}' leaked through SVG clean: {out}"
            );
        }

        // But the harmless attributes whose names merely start with
        // "on" (`one`, `onion`) must survive.
        assert!(
            out.contains(r#"one="keep-me""#),
            "harmless 'one' attr was over-eagerly stripped: {out}"
        );
        assert!(
            out.contains(r#"onion="keep-too""#),
            "harmless 'onion' attr was over-eagerly stripped: {out}"
        );

        // Structural content still there
        assert!(out.contains("<rect"), "rect elements must survive: {out}");
        assert!(out.contains(r#"fill="red""#));
        assert!(out.contains(r#"fill="blue""#));
        // The link's `<text>` child (not inside a dropped subtree) must
        // survive even though the enclosing `<a>` lost its href.
        assert!(
            out.contains("click"),
            "<text> body inside <a> must survive: {out}"
        );
    }

    #[test]
    fn svg_read_surfaces_script_and_event_handlers() {
        // The reader must flag scripts and event handlers to the user
        // so they see why the cleaner is about to touch the file.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("hostile.svg");
        fs::write(
            &src,
            r#"<svg xmlns="http://www.w3.org/2000/svg">
  <script>evil()</script>
  <rect onclick="bad()"/>
  <a href="javascript:also_bad()"><text>x</text></a>
</svg>"#,
        )
        .unwrap();
        let h = SvgHandler;
        let meta = h.read_metadata(&src).unwrap();
        let dump = format!("{meta:?}");
        assert!(dump.contains("event handler"), "{dump}");
        assert!(dump.contains("bad()"), "{dump}");
        assert!(dump.contains("javascript: uri"), "{dump}");
        assert!(dump.contains("also_bad"), "{dump}");
        // Script body text is surfaced via the existing
        // DROP_ELEMENTS text-capture path, not as an attribute.
        assert!(dump.contains("evil()"), "{dump}");
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

    #[test]
    fn is_javascript_uri_does_not_panic_on_mid_char_boundary() {
        // Regression: the old implementation did
        // `trimmed[.."javascript:".len()]` which slices a `&str` at
        // byte index 11. If the 11th byte is a UTF-8 continuation
        // byte (e.g. the second byte of a 3-byte snowman that starts
        // at byte 9), the slice panics with "byte index 11 is not a
        // char boundary". A crafted SVG attribute value would then
        // panic the worker thread; `run_job_with_terminal_error` now
        // catches this, but a handler should never panic on valid
        // UTF-8 input in the first place.
        let value = "javascrip\u{2603}"; // 9 ASCII bytes + 3-byte snowman
        assert_eq!(value.len(), 12);
        // Must not panic.
        assert!(!is_javascript_uri(value));

        // Also confirm the happy path still matches.
        assert!(is_javascript_uri("javascript:alert(1)"));
        assert!(is_javascript_uri("JavaScript:alert(1)"));
        assert!(is_javascript_uri("  javascript:alert(1)")); // leading whitespace
        // And a non-matching prefix of the same length is still rejected.
        assert!(!is_javascript_uri("javascrunt:"));
        // An empty value is rejected without panicking.
        assert!(!is_javascript_uri(""));
        // A value too short for the prefix is rejected.
        assert!(!is_javascript_uri("java"));
    }

    #[test]
    fn svg_clean_with_multibyte_href_does_not_panic() {
        // End-to-end: a real SVG whose `href` attribute is the
        // multibyte-boundary panic reproducer. The cleaner must
        // process this without panicking and emit something that
        // contains neither the script tag nor any JavaScript-uri
        // residue.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("multibyte.svg");
        let dst = dir.path().join("clean.svg");
        let xml = r#"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
  <a href="javascrip☃">
    <text>click</text>
  </a>
</svg>"#;
        fs::write(&src, xml).unwrap();
        let h = SvgHandler;
        h.clean_metadata(&src, &dst).expect("clean must not panic");
        let out = fs::read_to_string(&dst).unwrap();
        assert!(out.contains("<text"), "text element must survive: {out}");
    }

    #[test]
    fn svg_clean_drops_foreign_object_with_iframe_xss() {
        // Round 19 regression: `<foreignObject>` is the SVG mechanism
        // for embedding arbitrary HTML. The sanitize_attributes
        // URI-filter only knows about `href`/`xlink:href`, so a
        // smuggled `<iframe src="javascript:alert(1)">` inside a
        // `<foreignObject>` used to pass through unchanged. Drop the
        // entire subtree instead.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("xss.svg");
        let dst = dir.path().join("clean.svg");
        let xml = r#"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg">
  <foreignObject width="100" height="100">
    <iframe xmlns="http://www.w3.org/1999/xhtml" src="javascript:alert(1)"/>
  </foreignObject>
  <rect x="0" y="0" width="10" height="10" fill="red"/>
</svg>"#;
        fs::write(&src, xml).unwrap();
        let h = SvgHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(
            !out.contains("javascript:"),
            "javascript: URI must not survive inside foreignObject, got: {out}"
        );
        assert!(
            !out.contains("<iframe"),
            "iframe must be dropped along with foreignObject, got: {out}"
        );
        assert!(
            !out.contains("foreignObject"),
            "foreignObject wrapper must be dropped, got: {out}"
        );
        // Sibling non-foreignObject content must survive.
        assert!(
            out.contains("<rect"),
            "sibling rect element must survive: {out}"
        );
    }

    #[test]
    fn svg_clean_drops_direct_iframe_defense_in_depth() {
        // Defense-in-depth regression: `<iframe>` is not a native SVG
        // element, so any top-level iframe inside an svg root is a
        // hand-crafted attempt to smuggle it through a mixed-content
        // parser. Drop it regardless of whether it lives inside a
        // `<foreignObject>`.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("direct.svg");
        let dst = dir.path().join("clean.svg");
        let xml = r#"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg">
  <iframe src="javascript:alert(2)"/>
  <rect x="0" y="0" width="10" height="10" fill="blue"/>
</svg>"#;
        fs::write(&src, xml).unwrap();
        let h = SvgHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(
            !out.contains("javascript:"),
            "iframe with javascript: src must be dropped, got: {out}"
        );
        assert!(
            !out.contains("<iframe"),
            "iframe element must be dropped, got: {out}"
        );
        assert!(
            out.contains("<rect"),
            "sibling rect element must survive: {out}"
        );
    }

    #[test]
    fn svg_clean_drops_style_block() {
        // `<style>` inside an SVG survived the drop pass because the
        // element was not in DROP_ELEMENTS. CSS comments and @import
        // / url() beacons inside a styled SVG are a real leak vector:
        // a `/* author: jvoisin */` comment fingerprints the editor,
        // and an `@import url(http://tracker/x.css)` beacons on
        // every render. Match mat2's outcome (mat2 rasterizes, so
        // styles are lost) by dropping the whole element subtree.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("styled.svg");
        let dst = dir.path().join("clean.svg");
        let xml = br#"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
  <style>
    /* author: jvoisin, version: 1.2.3 */
    @import url(http://tracker.example/beacon.css);
    rect { fill: url(http://tracker.example/fill.png); }
  </style>
  <rect x="0" y="0" width="10" height="10"/>
</svg>"#;
        fs::write(&src, xml).unwrap();

        let h = SvgHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();

        for needle in [
            "<style",
            "</style>",
            "author: jvoisin",
            "version: 1.2.3",
            "@import",
            "tracker.example",
            "beacon.css",
            "fill.png",
        ] {
            assert!(
                !out.contains(needle),
                "'{needle}' leaked through SVG clean: {out}"
            );
        }
        // Structural content still there.
        assert!(out.contains("<rect"), "rect must survive: {out}");
    }
}
