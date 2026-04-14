//! CSS "metadata" cleaner.
//!
//! CSS itself has no metadata field, but stylesheets frequently carry
//! comments in their header with author name, licence, version,
//! contact info, and other fingerprinting data. mat2's `CSSParser`
//! strips every `/* ... */` comment; we do the same.
//!
//! Single-line `//` comments are **not** valid CSS (they exist in
//! SCSS/LESS preprocessors only), so we ignore them here.

use std::fs;
use std::path::Path;

use crate::error::CoreError;
use crate::metadata::{MetadataGroup, MetadataItem, MetadataSet};

use super::FormatHandler;

pub struct CssHandler;

impl FormatHandler for CssHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
        let content = fs::read_to_string(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        let comments = extract_comments(&content);
        let mut items = Vec::new();
        for (idx, c) in comments.iter().enumerate() {
            // Try to parse "key: value" style lines inside the comment.
            let mut parsed_any = false;
            for line in c.lines() {
                let trimmed = line.trim_matches(|ch: char| ch.is_whitespace() || ch == '*');
                if trimmed.is_empty() {
                    continue;
                }
                if let Some((k, v)) = trimmed.split_once(':') {
                    let key = k.trim().to_string();
                    let value = v.trim().to_string();
                    if !key.is_empty() && !value.is_empty() {
                        items.push(MetadataItem { key, value });
                        parsed_any = true;
                    }
                }
            }
            if !parsed_any {
                // Whole comment is free-form; surface it under a
                // synthetic key so the user sees the leak.
                items.push(MetadataItem {
                    key: format!("comment #{}", idx + 1),
                    value: c.trim().to_string(),
                });
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
        let content = fs::read_to_string(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        let cleaned = strip_comments(&content);
        fs::write(output_path, cleaned).map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("Failed to write cleaned CSS: {e}"),
        })?;
        Ok(())
    }

    fn supported_mime_types(&self) -> &[&str] {
        &["text/css"]
    }
}

/// Walk the CSS source and return the text of every `/* … */` comment.
/// The walker is *string-aware* so comment markers inside CSS string
/// literals are not misinterpreted.
fn extract_comments(css: &str) -> Vec<String> {
    let bytes = css.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        let b = bytes[i];
        // Enter a string literal; skip to the matching closing quote
        if b == b'"' || b == b'\'' {
            let quote = b;
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if i < bytes.len() {
                i += 1;
            }
            continue;
        }
        if b == b'/' && bytes[i + 1] == b'*' {
            // Found a comment. Scan to `*/`.
            let start = i + 2;
            i = start;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < bytes.len() {
                let body = &bytes[start..i];
                out.push(String::from_utf8_lossy(body).into_owned());
                i += 2;
            } else {
                break;
            }
            continue;
        }
        i += 1;
    }
    out
}

/// Remove every `/* … */` comment from the input, preserving string
/// literals verbatim.
fn strip_comments(css: &str) -> String {
    let bytes = css.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' || b == b'\'' {
            let quote = b;
            out.push(b);
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    out.push(bytes[i]);
                    out.push(bytes[i + 1]);
                    i += 2;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            if i < bytes.len() {
                out.push(bytes[i]);
                i += 1;
            }
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            // Skip to matching `*/`
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < bytes.len() {
                i += 2;
            } else {
                // Unterminated comment — drop the rest to avoid
                // re-emitting partial data.
                break;
            }
            continue;
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| css.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn css_read_parses_key_value_comments() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("style.css");
        fs::write(
            &src,
            "/* author: jvoisin\n * version: 1.0\n */\nbody { color: red; }\n",
        )
        .unwrap();
        let h = CssHandler;
        let meta = h.read_metadata(&src).unwrap();
        assert_eq!(meta.total_count(), 2);
        let dump = format!("{meta:?}");
        assert!(dump.contains("author"));
        assert!(dump.contains("jvoisin"));
    }

    #[test]
    fn css_clean_strips_every_comment() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("style.css");
        let dst = dir.path().join("clean.css");
        fs::write(
            &src,
            "/* secret */ body { color: red; /* inline */ } /* trailing */\n",
        )
        .unwrap();
        let h = CssHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(!out.contains("secret"));
        assert!(!out.contains("inline"));
        assert!(!out.contains("trailing"));
        assert!(out.contains("color: red"));
    }

    #[test]
    fn css_preserves_string_literals_with_comment_markers() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("style.css");
        let dst = dir.path().join("clean.css");
        fs::write(
            &src,
            r#"body::before { content: "/* not a comment */"; } /* real */"#,
        )
        .unwrap();
        let h = CssHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(
            out.contains("/* not a comment */"),
            "string literal was corrupted: {out}"
        );
        assert!(!out.contains("real"));
    }
}
