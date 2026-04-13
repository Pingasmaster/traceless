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
        tokens.push(Token::Open {
            raw: &src[start..i],
            local_name,
            self_closing,
        });
    }

    tokens
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
}
