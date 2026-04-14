//! HTML / XHTML metadata cleaner.
//!
//! HTML has two distinct metadata vectors:
//! 1. `<meta>` elements in `<head>`. mat2's `HTMLParser` drops these
//!    entirely (the "blocklist" approach — `tags_blocklist = {meta}`).
//! 2. The contents of `<title>`. mat2 keeps the element (some readers
//!    require it) but blanks the text inside it (the "required blocklist"
//!    — `tags_required_blocklist = {title}`).
//!
//! We implement the same behavior at the byte level. HTML is not valid
//! XML in the general case so we don't use quick-xml; instead we hand-
//! roll a tag-level state machine that only needs to recognize `<meta>`
//! and `<title>` with correct handling of comments, CDATA-ish blocks,
//! script/style raw text, and malformed input.
//!
//! Scope:
//! - HTML5, XHTML, and hand-written mixed markup all work.
//! - We do *not* apply entity escaping — we pass text through verbatim
//!   so hand-written entities round-trip intact.

use std::borrow::Cow;
use std::fs;
use std::path::Path;

use crate::error::CoreError;
use crate::metadata::{MetadataGroup, MetadataItem, MetadataSet};

use super::FormatHandler;

pub struct HtmlHandler;

impl FormatHandler for HtmlHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
        let src = fs::read_to_string(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        let items = extract_html_metadata(&src);

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
        let src = fs::read_to_string(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        let cleaned = clean_html(&src);
        fs::write(output_path, cleaned).map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("Failed to write cleaned HTML: {e}"),
        })?;
        Ok(())
    }

    fn supported_mime_types(&self) -> &[&str] {
        &["text/html", "application/xhtml+xml"]
    }
}

// ---------- Tag-level walker ----------

#[derive(Clone)]
enum Token<'a> {
    /// Literal text (not inside a tag)
    Text(&'a str),
    /// An HTML comment `<!-- ... -->`. The content is dropped (we blank
    /// all comments on clean) so we only need a sentinel variant.
    Comment,
    /// A DOCTYPE or processing instruction-style declaration (`<!DOCTYPE ...>` or `<?xml ...?>`)
    Declaration(&'a str),
    /// An opening or self-closing tag `<tag attr="val">`
    /// Stores the full source of the tag and a cached lowercase local name.
    Open {
        raw: &'a str,
        local_name: String,
        self_closing: bool,
    },
    /// A closing tag `</tag>`
    Close { raw: &'a str, local_name: String },
}

/// Walk `src` producing a sequence of tokens. Unterminated tags at EOF
/// are emitted as `Text` so nothing is silently dropped.
fn tokenize(src: &str) -> Vec<Token<'_>> {
    let bytes = src.as_bytes();
    let mut tokens: Vec<Token<'_>> = Vec::new();
    let mut i = 0usize;
    let len = bytes.len();

    while i < len {
        let b = bytes[i];
        if b != b'<' {
            // Accumulate text until the next `<`
            let start = i;
            while i < len && bytes[i] != b'<' {
                i += 1;
            }
            tokens.push(Token::Text(&src[start..i]));
            continue;
        }

        // b == b'<'
        if i + 3 < len && &bytes[i..i + 4] == b"<!--" {
            // Comment: scan until `-->`. We don't need to preserve the
            // content — the cleaner always drops comments.
            i += 4;
            while i + 2 < len && &bytes[i..i + 3] != b"-->" {
                i += 1;
            }
            let end = if i + 3 <= len { i + 3 } else { len };
            tokens.push(Token::Comment);
            i = end;
            continue;
        }

        if i + 1 < len && (bytes[i + 1] == b'!' || bytes[i + 1] == b'?') {
            // Declaration / DOCTYPE / `<?xml ?>`
            let start = i;
            while i < len && bytes[i] != b'>' {
                i += 1;
            }
            if i < len {
                i += 1;
            }
            tokens.push(Token::Declaration(&src[start..i]));
            continue;
        }

        if i + 1 < len && bytes[i + 1] == b'/' {
            // Closing tag
            let start = i;
            let name_start = i + 2;
            let mut j = name_start;
            while j < len && bytes[j] != b'>' {
                j += 1;
            }
            if j >= len {
                // Unterminated closing tag → treat as text
                tokens.push(Token::Text(&src[start..]));
                i = len;
                continue;
            }
            let local_name = local_from_close(&src[name_start..j]);
            i = j + 1;
            tokens.push(Token::Close {
                raw: &src[start..i],
                local_name,
            });
            continue;
        }

        // Opening or self-closing tag
        let start = i;
        i += 1;
        let name_start = i;
        while i < len
            && bytes[i] != b' '
            && bytes[i] != b'\t'
            && bytes[i] != b'\r'
            && bytes[i] != b'\n'
            && bytes[i] != b'>'
            && bytes[i] != b'/'
        {
            i += 1;
        }
        let raw_name = &src[name_start..i];
        let local_name = raw_name.to_ascii_lowercase();

        // Scan attributes to end of tag. Values in quotes must allow
        // `>` characters inside.
        let mut in_quote: Option<u8> = None;
        while i < len {
            let c = bytes[i];
            if let Some(q) = in_quote {
                if c == q {
                    in_quote = None;
                }
                i += 1;
                continue;
            }
            if c == b'"' || c == b'\'' {
                in_quote = Some(c);
                i += 1;
                continue;
            }
            if c == b'>' {
                break;
            }
            i += 1;
        }
        if i >= len {
            tokens.push(Token::Text(&src[start..]));
            i = len;
            continue;
        }
        // Self-closing if the last non-space char before `>` is `/`
        let tag_body = &bytes[start..i];
        let self_closing = tag_body
            .iter()
            .rev()
            .find(|&&c| !c.is_ascii_whitespace() && c != b'>')
            .is_some_and(|&c| c == b'/');

        i += 1; // consume `>`

        // `<script>`, `<style>`, `<textarea>`, and `<title>` are HTML
        // raw-text / escapable-raw-text elements per HTML5 §12.2.5:
        // their content is consumed verbatim until the matching closing
        // tag, and `<`, `<!--`, etc. inside the body are NOT tags.
        // Without the rawtext branch the generic tokenizer re-parses
        // JavaScript / CSS / textarea / title string bytes as HTML,
        // and a literal `<` inside the body is misinterpreted as the
        // start of a new opening tag - which for `<title>Alice < Bob</title>`
        // consumes the real `</title>` as part of a bogus empty-name
        // tag, leaks the `< Bob` suffix through the cleaner, and hides
        // the title from the reader entirely.
        let rawtext_needle: Option<&[u8]> =
            if !self_closing && local_name == "script" {
                Some(b"script")
            } else if !self_closing && local_name == "style" {
                Some(b"style")
            } else if !self_closing && local_name == "textarea" {
                Some(b"textarea")
            } else if !self_closing && local_name == "title" {
                Some(b"title")
            } else {
                None
            };
        tokens.push(Token::Open {
            raw: &src[start..i],
            local_name,
            self_closing,
        });
        if let Some(needle) = rawtext_needle {
            let body_start = i;
            let body_end = find_rawtext_close(bytes, i, needle);
            if body_end > body_start {
                tokens.push(Token::Text(&src[body_start..body_end]));
            }
            i = body_end;
        }
    }

    tokens
}

/// Scan forward from `from` looking for an ASCII-case-insensitive
/// `</name` sequence followed by a tag terminator (`>`, whitespace, `/`,
/// or EOF). Returns the byte offset at which the raw-text body ends
/// (i.e. the `<` of the closing tag), or `bytes.len()` if no closing
/// tag is found - an unterminated `<script>` at EOF is legal HTML and
/// the whole tail is the script body.
fn find_rawtext_close(bytes: &[u8], from: usize, name: &[u8]) -> usize {
    let mut i = from;
    let len = bytes.len();
    while i < len {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // Need room for `</` + name + one terminator byte.
        let header_end = i + 2 + name.len();
        if header_end >= len {
            return len;
        }
        if bytes[i + 1] != b'/' {
            i += 1;
            continue;
        }
        let name_slice = &bytes[i + 2..i + 2 + name.len()];
        if !name_slice.eq_ignore_ascii_case(name) {
            i += 1;
            continue;
        }
        // Must be followed by `>`, whitespace, or `/` to be a real close.
        let next = bytes[header_end];
        if next == b'>' || next == b'/' || next.is_ascii_whitespace() {
            return i;
        }
        i += 1;
    }
    len
}

fn local_from_close(inner: &str) -> String {
    inner
        .trim()
        .trim_end_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

// ---------- Reader ----------

/// Extract author/description/keywords/etc from `<meta>` tags plus
/// `<title>` text content. Returns the items as a flat vec.
fn extract_html_metadata(src: &str) -> Vec<MetadataItem> {
    let tokens = tokenize(src);
    let mut items: Vec<MetadataItem> = Vec::new();
    let mut in_title = false;
    let mut title_text = String::new();

    for tok in &tokens {
        match tok {
            Token::Open { local_name, raw, .. } => {
                match local_name.as_str() {
                    "meta" => {
                        // Parse `name="…" content="…"` or `http-equiv`/`property`
                        let attrs = parse_attrs(raw);
                        let label = attrs
                            .iter()
                            .find_map(|(k, v)| {
                                matches!(k.as_str(), "name" | "http-equiv" | "property" | "itemprop")
                                    .then(|| v.clone())
                            })
                            .unwrap_or_else(|| "(meta)".to_string());
                        let value = attrs
                            .iter()
                            .find_map(|(k, v)| (k == "content").then(|| v.clone()))
                            .unwrap_or_default();
                        items.push(MetadataItem {
                            key: label,
                            value,
                        });
                    }
                    "title" => {
                        in_title = true;
                        title_text.clear();
                    }
                    _ => {}
                }
            }
            Token::Close { local_name, .. } => {
                if local_name == "title" && in_title {
                    in_title = false;
                    let t = title_text.trim().to_string();
                    if !t.is_empty() {
                        items.push(MetadataItem {
                            key: "title".to_string(),
                            value: t,
                        });
                    }
                    title_text.clear();
                }
            }
            Token::Text(t) => {
                if in_title {
                    title_text.push_str(t);
                }
            }
            _ => {}
        }
    }

    items
}

fn parse_attrs(tag_raw: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = tag_raw.as_bytes();
    if bytes.len() < 2 {
        return out;
    }
    // Skip `<tagname`
    let mut i = 1usize;
    while i < bytes.len()
        && !bytes[i].is_ascii_whitespace()
        && bytes[i] != b'>'
        && bytes[i] != b'/'
    {
        i += 1;
    }
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'>' || bytes[i] == b'/' {
            break;
        }
        let key_start = i;
        while i < bytes.len()
            && bytes[i] != b'='
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'>'
            && bytes[i] != b'/'
        {
            i += 1;
        }
        let key = tag_raw[key_start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let value;
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                i += 1;
                let v_start = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                value = tag_raw[v_start..i].to_string();
                if i < bytes.len() {
                    i += 1;
                }
            } else {
                let v_start = i;
                while i < bytes.len()
                    && !bytes[i].is_ascii_whitespace()
                    && bytes[i] != b'>'
                    && bytes[i] != b'/'
                {
                    i += 1;
                }
                value = tag_raw[v_start..i].to_string();
            }
            out.push((key, value));
        } else {
            out.push((key, String::new()));
        }
    }
    out
}

// ---------- Cleaner ----------

/// Tags we drop entirely from cleaned HTML output. Matches mat2's
/// `HTMLParser.tags_blocklist` in `libmat2/web.py` plus the content
/// frames `<object>`, `<embed>`, `<iframe>` which can leak via their
/// attributes or child documents.
///
/// - `meta`, `link`, `base`: void head metadata (`<link rel="author">`,
///   `<link rel="canonical">`, `<base href>` all fingerprint).
/// - `script`, `style`, `noscript`: raw-text containers carrying code,
///   fonts, inline URLs.
/// - `iframe`, `object`, `embed`: external resource frames.
fn is_drop_tag(name: &str) -> bool {
    matches!(
        name,
        "meta"
            | "link"
            | "base"
            | "script"
            | "style"
            | "noscript"
            | "iframe"
            | "object"
            | "embed"
    )
}

/// HTML5 void elements per WHATWG §12.1.2. A void element has no
/// content and no closing tag, so the cleaner must not try to push
/// it onto the drop-stack.
fn is_void_tag(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// Drop every metadata-bearing element and blank every `<title>` body.
/// Comments are also dropped because mat2 blanks them too.
///
/// Element drops are stack-aware: `<iframe>`, `<object>`, `<script>`,
/// `<style>`, `<noscript>` and friends can be arbitrarily nested, and
/// every token between the open and the matching close (including
/// nested same-name opens) is elided. Void drop tags (`<meta>`,
/// `<link>`, `<base>`, `<embed>`) are simply skipped once.
///
/// Every surviving open tag has its inline `on*` event handlers
/// stripped so they don't leak fingerprints or cross-site tracking.
#[must_use]
pub(crate) fn clean_html(src: &str) -> String {
    let tokens = tokenize(src);
    let mut out = String::with_capacity(src.len());
    let mut title_depth: usize = 0;
    // Stack of currently-open drop-element names. While non-empty
    // every token is silently consumed until the matching close pops
    // the stack back to empty.
    let mut drop_stack: Vec<String> = Vec::new();

    for tok in tokens {
        if let Some(top) = drop_stack.last().cloned() {
            match &tok {
                Token::Open {
                    local_name,
                    self_closing,
                    ..
                } => {
                    if *local_name == top && !*self_closing {
                        drop_stack.push(local_name.clone());
                    }
                }
                Token::Close { local_name, .. } => {
                    if *local_name == top {
                        drop_stack.pop();
                    }
                }
                _ => {}
            }
            continue;
        }

        match tok {
            Token::Open {
                raw,
                local_name,
                self_closing,
            } => {
                if is_drop_tag(&local_name) {
                    if !self_closing && !is_void_tag(&local_name) {
                        drop_stack.push(local_name);
                    }
                    continue;
                }
                match local_name.as_str() {
                    "title" => {
                        out.push_str(&strip_on_handlers(raw));
                        if !self_closing {
                            title_depth += 1;
                        }
                    }
                    _ => out.push_str(&strip_on_handlers(raw)),
                }
            }
            Token::Close { raw, local_name } => {
                if is_drop_tag(&local_name) {
                    // Stray closer for a drop tag (mismatched open or
                    // invalid HTML). Silently elide.
                    continue;
                }
                if local_name == "title" && title_depth > 0 {
                    title_depth -= 1;
                }
                out.push_str(raw);
            }
            Token::Text(t) => {
                if title_depth == 0 {
                    out.push_str(t);
                }
            }
            Token::Comment => {}
            Token::Declaration(d) => out.push_str(d),
        }
    }

    out
}

/// Strip every `on*` event-handler attribute from the raw bytes of
/// an opening tag, preserving every other attribute verbatim. Returns
/// borrowed bytes when no `on*` attribute is present so untouched
/// tags cost nothing beyond the fast-path scan.
fn strip_on_handlers(raw: &str) -> Cow<'_, str> {
    if !contains_on_attribute(raw) {
        return Cow::Borrowed(raw);
    }
    let bytes = raw.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);

    // Copy `<tagname` verbatim.
    let mut i = 0usize;
    while i < len
        && !bytes[i].is_ascii_whitespace()
        && bytes[i] != b'>'
        && bytes[i] != b'/'
    {
        i += 1;
    }
    out.push_str(&raw[..i]);

    while i < len {
        let ws_start = i;
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let ws = &raw[ws_start..i];
        if i >= len || bytes[i] == b'>' || bytes[i] == b'/' {
            out.push_str(ws);
            out.push_str(&raw[i..]);
            return Cow::Owned(out);
        }
        let key_start = i;
        while i < len
            && bytes[i] != b'='
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'>'
            && bytes[i] != b'/'
        {
            i += 1;
        }
        let key_end = i;
        let key_lower = raw[key_start..key_end].to_ascii_lowercase();

        // Skip optional whitespace before `=`.
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let attr_has_value = i < len && bytes[i] == b'=';
        if attr_has_value {
            i += 1;
            while i < len && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < len && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                i += 1;
                while i < len && bytes[i] != quote {
                    i += 1;
                }
                if i < len {
                    i += 1;
                }
            } else {
                while i < len
                    && !bytes[i].is_ascii_whitespace()
                    && bytes[i] != b'>'
                    && bytes[i] != b'/'
                {
                    i += 1;
                }
            }
        }
        let attr_end = i;

        let is_event_handler = key_lower.len() > 2
            && &key_lower.as_bytes()[..2] == b"on"
            && key_lower.as_bytes()[2].is_ascii_alphabetic();
        if !is_event_handler {
            out.push_str(ws);
            out.push_str(&raw[key_start..attr_end]);
        }
        // Dropped `on*`: do not copy the leading whitespace so we
        // don't leave a double-space artifact mid-tag.
    }
    Cow::Owned(out)
}

/// Fast-path probe: returns true if the raw tag source contains any
/// attribute whose name starts with `on` (case-insensitive) followed
/// by an alpha character. When this returns false we can hand the
/// caller back a borrowed slice with no allocation.
fn contains_on_attribute(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let len = bytes.len();
    if len < 4 {
        return false;
    }
    let mut i = 0usize;
    while i + 3 < len {
        if bytes[i].is_ascii_whitespace() {
            let c0 = bytes[i + 1];
            let c1 = bytes[i + 2];
            let c2 = bytes[i + 3];
            if (c0 == b'o' || c0 == b'O')
                && (c1 == b'n' || c1 == b'N')
                && c2.is_ascii_alphabetic()
            {
                return true;
            }
        }
        i += 1;
    }
    false
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn html_read_surfaces_meta_and_title() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        fs::write(
            &src,
            r#"<!DOCTYPE html>
<html><head>
<meta name="author" content="jvoisin">
<meta name="description" content="a leak">
<title>Secret Document</title>
</head><body><p>hi</p></body></html>"#,
        )
        .unwrap();
        let h = HtmlHandler;
        let meta = h.read_metadata(&src).unwrap();
        let dump = format!("{meta:?}");
        assert!(dump.contains("jvoisin"));
        assert!(dump.contains("a leak"));
        assert!(dump.contains("Secret Document"));
    }

    #[test]
    fn html_clean_drops_meta_and_blanks_title() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        let dst = dir.path().join("clean.html");
        fs::write(
            &src,
            r#"<!DOCTYPE html>
<html><head>
<meta name="author" content="jvoisin">
<title>Secret Document</title>
</head><body><p>visible</p></body></html>"#,
        )
        .unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(!out.contains("jvoisin"));
        assert!(!out.contains("Secret Document"));
        assert!(out.contains("<title>"));
        assert!(out.contains("</title>"));
        assert!(out.contains("visible"));
        // Doctype preserved
        assert!(out.contains("<!DOCTYPE html>"));
    }

    #[test]
    fn html_clean_drops_comments() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        let dst = dir.path().join("clean.html");
        fs::write(
            &src,
            r"<html><body><!-- secret comment -->visible<!--another--></body></html>",
        )
        .unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(!out.contains("secret comment"));
        assert!(!out.contains("another"));
        assert!(out.contains("visible"));
    }

    #[test]
    fn html_clean_handles_self_closing_meta() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.xhtml");
        let dst = dir.path().join("clean.xhtml");
        fs::write(
            &src,
            r#"<?xml version="1.0"?><html><head><meta name="author" content="x"/></head></html>"#,
        )
        .unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(!out.contains("author"));
        assert!(out.contains("<head>"));
        assert!(out.contains("</head>"));
    }

    #[test]
    fn html_clean_preserves_nested_content() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        let dst = dir.path().join("clean.html");
        fs::write(
            &src,
            r#"<html><body><div class="a"><span>inner</span></div></body></html>"#,
        )
        .unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert_eq!(out, r#"<html><body><div class="a"><span>inner</span></div></body></html>"#);
    }

    #[test]
    fn html_clean_drops_script_and_style_elements() {
        // mat2 parity: `<script>` and `<style>` are in the drop
        // blocklist. The rawtext tokenizer still consumes their body
        // as a single Text token, and the new drop-stack rule then
        // elides that body along with the open/close pair so no
        // script bytes survive into the cleaned output.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        let dst = dir.path().join("clean.html");
        let input = r#"<html><head><script>var secret = "leak-from-script";</script><style>.x { content: "leak-from-style"; }</style></head><body><p>visible</p></body></html>"#;
        fs::write(&src, input).unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(!out.contains("<script"), "<script> tag survived: {out}");
        assert!(!out.contains("</script"), "</script> tag survived: {out}");
        assert!(!out.contains("leak-from-script"), "script body survived: {out}");
        assert!(!out.contains("<style"), "<style> tag survived: {out}");
        assert!(!out.contains("</style"), "</style> tag survived: {out}");
        assert!(!out.contains("leak-from-style"), "style body survived: {out}");
        assert!(out.contains("<p>visible</p>"), "body content must survive: {out}");
    }

    #[test]
    fn html_clean_drops_link_base_and_frames() {
        // mat2 blocklist: <link>, <base>, <iframe>, <object>, <embed>,
        // <noscript>. All must vanish, including any nested content
        // inside the containers.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        let dst = dir.path().join("clean.html");
        let input = r#"<html><head>
<link rel="author" href="https://example.invalid/me">
<link rel="canonical" href="https://example.invalid/canonical">
<base href="https://example.invalid/">
</head><body>
<iframe src="https://tracker.invalid/"><p>inside iframe</p></iframe>
<object data="leak.swf"><param name="secret" value="leak"/></object>
<embed src="leak.mov"/>
<noscript>noscript content</noscript>
<p>survivor</p>
</body></html>"#;
        fs::write(&src, input).unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(!out.contains("<link"), "<link> survived: {out}");
        assert!(!out.contains("<base"), "<base> survived: {out}");
        assert!(!out.contains("<iframe"), "<iframe> survived: {out}");
        assert!(!out.contains("</iframe"), "</iframe> survived: {out}");
        assert!(!out.contains("inside iframe"), "iframe body survived: {out}");
        assert!(!out.contains("<object"), "<object> survived: {out}");
        assert!(!out.contains("<param"), "object param survived: {out}");
        assert!(!out.contains("secret"), "object secret survived: {out}");
        assert!(!out.contains("<embed"), "<embed> survived: {out}");
        assert!(!out.contains("<noscript"), "<noscript> survived: {out}");
        assert!(!out.contains("noscript content"), "noscript body survived: {out}");
        assert!(out.contains("<p>survivor</p>"), "body content must survive: {out}");
    }

    #[test]
    fn html_clean_strips_on_event_handlers() {
        // mat2 parity: inline on* event handlers are fingerprinting
        // and user-tracking vectors. Strip every on* attribute from
        // every surviving element while keeping non-on attributes
        // (class, id, href, data-*) intact.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        let dst = dir.path().join("clean.html");
        let input = r#"<html><body><div onclick="track()" class="a" data-x="keep"><a href="/x" ONLOAD="track()" onerror='track()'>link</a></div></body></html>"#;
        fs::write(&src, input).unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(!out.contains("onclick"), "onclick survived: {out}");
        assert!(!out.contains("ONLOAD"), "ONLOAD survived: {out}");
        assert!(!out.contains("onerror"), "onerror survived: {out}");
        assert!(!out.contains("track()"), "handler body survived: {out}");
        assert!(out.contains(r#"class="a""#), "class must survive: {out}");
        assert!(out.contains(r#"data-x="keep""#), "data-* must survive: {out}");
        assert!(out.contains(r#"href="/x""#), "href must survive: {out}");
        assert!(out.contains(">link</a>"), "body text must survive: {out}");
    }

    #[test]
    fn html_reader_ignores_meta_inside_script_body() {
        // The reader shares the same tokenizer. A `<meta>` embedded as
        // a JavaScript string literal must not be surfaced as real
        // metadata the user sees. (The cleaner now drops the whole
        // script element, but the reader still has to ignore the
        // literal so the "before cleaning" UI doesn't alarm users
        // about code inside a script body.)
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        fs::write(
            &src,
            r#"<html><head><script>var s = "<meta name='author' content='not-a-real-meta'>";</script></head></html>"#,
        )
        .unwrap();
        let h = HtmlHandler;
        let meta = h.read_metadata(&src).unwrap();
        let dump = format!("{meta:?}");
        assert!(
            !dump.contains("not-a-real-meta"),
            "reader surfaced a `<meta>` that lived inside a script body: {dump}"
        );
    }

    #[test]
    fn html_clean_script_unterminated_at_eof_dropped() {
        // An unterminated `<script>` at EOF: the rawtext tokenizer
        // treats the tail to EOF as the script body, and the cleaner
        // drops the entire element including that body. This used to
        // be a "preserve verbatim" test before script landed on the
        // blocklist.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        let dst = dir.path().join("clean.html");
        fs::write(
            &src,
            r#"<html><body><p>keep</p><script>var s = "<meta>"; // no close"#,
        )
        .unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(out.contains("<p>keep</p>"), "body prefix must survive: {out}");
        assert!(!out.contains("<script"), "<script> survived: {out}");
        assert!(!out.contains("var s"), "script body survived: {out}");
    }

    #[test]
    fn html_clean_script_close_tag_case_insensitive_dropped() {
        // The raw-text scanner matches `</SCRIPT>` in any case, and the
        // drop pass then elides the whole element.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        let dst = dir.path().join("clean.html");
        fs::write(
            &src,
            r#"<html><body><SCRIPT>var s = "<meta>";</SCRIPT><p>after</p></body></html>"#,
        )
        .unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(!out.contains("<SCRIPT"), "<SCRIPT> survived: {out}");
        assert!(!out.contains("var s"), "script body survived: {out}");
        assert!(out.contains("<p>after</p>"), "post-script text must survive: {out}");
    }

    #[test]
    fn html_clean_script_unterminated_close_at_eof_no_panic() {
        // Regression: an input whose tail is exactly `</script` (no
        // terminator byte after the name) used to panic inside
        // `find_rawtext_close` with an out-of-bounds read. The whole
        // element is now dropped, so the output must not panic and
        // must not contain any script bytes.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        let dst = dir.path().join("clean.html");
        fs::write(&src, b"<script></script").unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(!out.contains("script"), "script bytes survived: {out}");
    }

    #[test]
    fn html_clean_style_unterminated_close_at_eof_no_panic() {
        // Same regression for the `<style>` raw-text element.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        let dst = dir.path().join("clean.html");
        fs::write(&src, b"<style></style").unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(!out.contains("style"), "style bytes survived: {out}");
    }

    #[test]
    fn html_clean_preserves_textarea_literal_meta_and_comment() {
        // Round-7 Bug 15: `<textarea>` is "escapable raw text" per
        // HTML5 §12.2.5, so embedded `<meta>` / `<!--...-->` must
        // round-trip verbatim. Before the fix the tokenizer parsed
        // them as real HTML tags and `clean_html` dropped the meta
        // and the comment, corrupting the user's textarea content.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        let dst = dir.path().join("clean.html");
        let input = r#"<html><body><form><textarea>Report draft:
<meta name="author" content="example">
<!-- TODO: review -->
End of draft.</textarea></form></body></html>"#;
        fs::write(&src, input).unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(
            out.contains(r#"<meta name="author" content="example">"#),
            "textarea literal meta must round-trip verbatim, got: {out}"
        );
        assert!(
            out.contains("<!-- TODO: review -->"),
            "textarea literal comment must round-trip verbatim, got: {out}"
        );
        assert!(out.contains("Report draft:"));
        assert!(out.contains("End of draft."));
        assert!(out.contains("</textarea>"));
    }

    #[test]
    fn html_clean_drops_real_meta_outside_textarea() {
        // Negative control for the fix above: a real `<meta>` in
        // `<head>` (outside any textarea) must still be dropped.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        let dst = dir.path().join("clean.html");
        let input = r#"<html><head><meta name="author" content="real-author"></head><body><textarea><meta name="fake" content="kept"></textarea></body></html>"#;
        fs::write(&src, input).unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(
            !out.contains("real-author"),
            "real head <meta> must be stripped, got: {out}"
        );
        assert!(
            out.contains(r#"<meta name="fake" content="kept">"#),
            "textarea literal meta must survive: {out}"
        );
    }

    #[test]
    fn html_clean_textarea_case_insensitive_close() {
        // The rawtext scanner must match `</TEXTAREA>` in any case.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        let dst = dir.path().join("clean.html");
        fs::write(
            &src,
            r"<html><body><TEXTAREA>pre <meta> post</TEXTAREA><p>after</p></body></html>",
        )
        .unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(out.contains("pre <meta> post"));
        assert!(out.contains("<p>after</p>"));
    }

    #[test]
    fn html_clean_blanks_title_containing_left_angle() {
        // Round 17 regression: `<title>` is HTML5 escapable raw text.
        // A literal `<` inside the title body used to make the generic
        // tokenizer consume the real `</title>` as part of a bogus
        // empty-name opening tag, which then rode through the
        // cleaner's `_ => out.push_str(raw)` arm verbatim - leaking
        // the portion of the body after the `<`. The tokenizer now
        // treats `title` as rawtext, so the entire body is emitted as
        // a single `Text` event and `clean_html`'s `title_depth` path
        // blanks it cleanly.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        let dst = dir.path().join("clean.html");
        fs::write(&src, "<title>Alice < Bob</title>").unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(
            !out.contains("Bob"),
            "title body with `<` inside must be fully blanked, got: {out}"
        );
        assert!(
            !out.contains("Alice"),
            "leading title body must also be blanked, got: {out}"
        );
        assert!(out.contains("<title>"));
        assert!(out.contains("</title>"));
    }

    #[test]
    fn html_read_surfaces_title_containing_left_angle() {
        // Reader side of the same regression: before the fix, a
        // mis-parsed title left `extract_html_metadata` in `in_title`
        // state with no matching `Close(title)` event, so the
        // accumulated title text was never pushed into the metadata
        // set and the user saw "no metadata" for a file that actually
        // leaked a title.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        fs::write(&src, "<title>Alice < Bob</title>").unwrap();
        let h = HtmlHandler;
        let meta = h.read_metadata(&src).unwrap();
        let dump = format!("{meta:?}");
        assert!(
            dump.contains("Alice < Bob") || dump.contains("Alice &lt; Bob"),
            "reader must surface the full title body, got: {dump}"
        );
    }
}
