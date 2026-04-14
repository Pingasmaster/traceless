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

        // `<script>`, `<style>`, and `<textarea>` are HTML raw-text /
        // escapable-raw-text elements per HTML5 §12.2.5: their content
        // is consumed verbatim until the matching closing tag, and
        // `<`, `<!--`, etc. inside the body are NOT tags. Without this
        // branch the generic tokenizer re-parses JavaScript / CSS /
        // textarea string bytes as HTML, and the cleaner then drops
        // fake `<meta>` / `<!-- -->` / `<title>` spans out of the
        // body, producing broken output. `<title>` is also escapable
        // raw text per spec but its current `title_depth` path in
        // `clean_html` already produces the mat2-parity "blank the
        // title body" behaviour, so we deliberately leave it alone
        // and don't add a fourth arm here.
        let rawtext_needle: Option<&[u8]> =
            if !self_closing && local_name == "script" {
                Some(b"script")
            } else if !self_closing && local_name == "style" {
                Some(b"style")
            } else if !self_closing && local_name == "textarea" {
                Some(b"textarea")
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

/// Drop every `<meta>` element and blank every `<title>` body. Leaves
/// all other structure intact. Comments are also dropped because mat2
/// blanks them too.
fn clean_html(src: &str) -> String {
    let tokens = tokenize(src);
    let mut out = String::with_capacity(src.len());
    let mut title_depth: usize = 0;

    for tok in tokens {
        match tok {
            Token::Open {
                raw,
                local_name,
                self_closing,
            } => match local_name.as_str() {
                "meta" => {} // drop entirely
                "title" => {
                    // Re-emit the opening tag verbatim but skip any
                    // text until the matching </title>. If the tag is
                    // self-closing we still emit it.
                    out.push_str(raw);
                    if !self_closing {
                        title_depth += 1;
                    }
                }
                _ => out.push_str(raw),
            },
            Token::Close { raw, local_name } => {
                if local_name == "title" && title_depth > 0 {
                    title_depth -= 1;
                }
                if local_name == "meta" {
                    // Drop any stray `</meta>` (invalid HTML but
                    // sometimes present)
                    continue;
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

#[cfg(test)]
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
    fn html_clean_preserves_script_literal_meta_and_comment() {
        // HTML "raw text" elements: a `<meta>` or `<!--...-->` that
        // appears inside a `<script>` body is part of a JavaScript
        // string literal, not a real HTML element. Without the
        // rawtext-aware tokenizer the cleaner drops these and
        // produces broken JS.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        let dst = dir.path().join("clean.html");
        let input = r#"<html><head><script>var s = "<meta name='x' content='y'>";
var c = "<!--nope-->";</script></head><body>ok</body></html>"#;
        fs::write(&src, input).unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(
            out.contains("<meta name='x' content='y'>"),
            "script literal HTML must round-trip verbatim, got: {out}"
        );
        assert!(
            out.contains("<!--nope-->"),
            "script literal comment must round-trip verbatim, got: {out}"
        );
        assert!(out.contains("</script>"));
        assert!(out.contains("ok"));
    }

    #[test]
    fn html_clean_preserves_style_literal_content() {
        // `<style>` is also a raw-text element. A `content:` value with
        // angle brackets must pass through untouched.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        let dst = dir.path().join("clean.html");
        let input = r#"<html><head><style>.x::before { content: "<meta>"; }</style></head><body/></html>"#;
        fs::write(&src, input).unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(
            out.contains(r#"content: "<meta>""#),
            "style literal must round-trip verbatim, got: {out}"
        );
        assert!(out.contains("</style>"));
    }

    #[test]
    fn html_reader_ignores_meta_inside_script_body() {
        // The reader shares the same tokenizer. A `<meta>` embedded as
        // a JavaScript string literal must not be surfaced as real
        // metadata the user sees.
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
    fn html_clean_script_unterminated_at_eof_preserved() {
        // Unterminated `<script>` bodies at EOF are legal HTML - the
        // raw-text body simply runs to the end of the input. The
        // cleaner must keep the whole tail verbatim instead of dropping
        // fake inner tags.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        let dst = dir.path().join("clean.html");
        fs::write(
            &src,
            r#"<html><body><script>var s = "<meta>"; // no close"#,
        )
        .unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(
            out.contains(r#"var s = "<meta>"; // no close"#),
            "unterminated script body must round-trip: {out}"
        );
    }

    #[test]
    fn html_clean_script_close_tag_case_insensitive() {
        // The raw-text scanner must match `</SCRIPT>` in any case.
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
        assert!(out.contains(r#"var s = "<meta>";"#));
        assert!(out.contains("<p>after</p>"));
    }

    #[test]
    fn html_clean_script_unterminated_close_at_eof_no_panic() {
        // Regression: an input whose tail is exactly `</script` (no
        // terminator byte after the name) used to panic inside
        // `find_rawtext_close` with an out-of-bounds read. The partial
        // close must now be treated as part of the script body.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("page.html");
        let dst = dir.path().join("clean.html");
        fs::write(&src, b"<script></script").unwrap();
        let h = HtmlHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(
            out.contains("</script"),
            "partial close at EOF must round-trip verbatim, got: {out}"
        );
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
        assert!(
            out.contains("</style"),
            "partial close at EOF must round-trip verbatim, got: {out}"
        );
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
}
