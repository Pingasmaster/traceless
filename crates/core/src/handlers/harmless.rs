//! Handler for "harmless" file formats: those that either cannot carry
//! metadata at all (text/plain, minimal BMP) or whose metadata is
//! trivially strippable with a single pass (PPM comments).
//!
//! Mirrors `libmat2/harmless.py` (text/plain + BMP) and
//! `libmat2/images.py::PPMParser` (PPM).

#[cfg(feature = "native")]
use std::fs;
#[cfg(feature = "native")]
use std::path::Path;

use crate::error::CoreError;
#[cfg(feature = "native")]
use crate::metadata::{MetadataGroup, MetadataItem, MetadataSet};

#[cfg(feature = "native")]
use super::FormatHandler;

pub struct HarmlessHandler;

/// True if the lowercase extension names a NetPBM image whose `#` comment
/// lines we strip. The remaining harmless formats (text/plain, BMP) carry
/// no strippable metadata and pass through verbatim.
fn is_ppm_ext(ext: &str) -> bool {
    matches!(ext, "ppm" | "pgm" | "pbm" | "pnm")
}

/// Clean a "harmless" file in memory. For NetPBM (PPM/PGM/PBM/PNM) this
/// drops every `#` comment line from the header; for text/plain and BMP
/// it is a verbatim copy. Shared by the native `clean_metadata` wrapper
/// and the wasm `inmem` path. `ext` is the lowercase extension (no dot).
///
/// # Errors
///
/// Returns [`CoreError::FileTooLarge`] if the buffer exceeds the input
/// cap. The PPM walk itself is infallible.
pub(crate) fn clean_bytes(input: &[u8], ext: &str) -> Result<Vec<u8>, CoreError> {
    super::check_input_len(input.len())?;
    if is_ppm_ext(ext) {
        Ok(strip_ppm_comments(input))
    } else {
        // text/plain + BMP: nothing strippable, byte-for-byte copy.
        Ok(input.to_vec())
    }
}

#[cfg(feature = "native")]
impl FormatHandler for HarmlessHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
        if !path.exists() {
            return Err(CoreError::NotFound {
                path: path.to_path_buf(),
            });
        }
        super::check_input_size(path)?;

        let mime = mime_guess::from_path(path).first_or_octet_stream();
        let mime_str = mime.as_ref();

        if is_ppm(mime_str, path) {
            return read_ppm_metadata(path);
        }

        // text/plain and BMP have no retrievable metadata in the sense
        // that applies to this tool. Return an empty set.
        Ok(MetadataSet::default())
    }

    fn clean_metadata(&self, path: &Path, output_path: &Path) -> Result<(), CoreError> {
        super::check_input_size(path)?;
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        let mime_str = mime.as_ref();

        // Preserve the native PPM-detection rule (MIME prefix OR
        // extension): when it fires, route through `clean_bytes` with a
        // synthetic `ppm` extension so the in-memory walker runs; the
        // resolved extension is otherwise irrelevant for the passthrough
        // branch.
        let ext = if is_ppm(mime_str, path) {
            "ppm".to_string()
        } else {
            path.extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .unwrap_or_default()
        };

        let data = fs::read(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;
        let cleaned = clean_bytes(&data, &ext).map_err(|e| super::repath(e, path))?;
        fs::write(output_path, cleaned).map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("Failed to write cleaned harmless file: {e}"),
        })?;
        Ok(())
    }

    fn supported_mime_types(&self) -> &[&str] {
        &[
            "text/plain",
            "image/bmp",
            "image/x-ms-bmp",
            "image/x-portable-pixmap",
            "image/x-portable-graymap",
            "image/x-portable-bitmap",
            "image/x-portable-anymap",
        ]
    }
}

#[cfg(feature = "native")]
fn is_ppm(mime: &str, path: &Path) -> bool {
    if mime.starts_with("image/x-portable-") {
        return true;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    matches!(ext.as_deref(), Some("ppm" | "pgm" | "pbm" | "pnm"))
}

/// PPM/PGM/PBM metadata reader. The NetPBM formats have a text header
/// followed by raw pixel data. The only metadata vector is the `#`
/// comment line, which mat2 surfaces via `libmat2/images.py::PPMParser`.
#[cfg(feature = "native")]
fn read_ppm_metadata(path: &Path) -> Result<MetadataSet, CoreError> {
    // Read only the header (up to a few KB). The pixel data after
    // the header may be binary (non-UTF-8), so take the minimum needed.
    let bytes = fs::read(path).map_err(|e| CoreError::ReadError {
        path: path.to_path_buf(),
        source: e,
    })?;

    // Locate the end of the header: P?, width, height, maxval, then
    // binary pixels. For safety we just scan the first 8 KiB.
    let scan = &bytes[..bytes.len().min(8192)];
    let scan_str = String::from_utf8_lossy(scan);

    let mut items = Vec::new();
    for (idx, raw_line) in scan_str.lines().enumerate() {
        let line = raw_line.trim_start();
        if line.starts_with('#') {
            items.push(MetadataItem {
                key: format!("comment line {}", idx + 1),
                value: line.trim_end().to_string(),
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

/// PPM cleaner: drop every `#` comment line from the header. Pixel data
/// (possibly binary) is passed through untouched. Walks bytes directly
/// to avoid corrupting binary payloads. Pure bytes-in/bytes-out so both
/// the native and wasm paths share it.
fn strip_ppm_comments(raw: &[u8]) -> Vec<u8> {
    // Scan the header token-by-token, dropping any line that starts
    // with `#`. The header ends after the 4th whitespace-separated
    // token for PPM (P6/P3), the 3rd for PGM (P5/P2), the 3rd for PBM
    // (P4/P1) - but rather than track which format, we stop scanning
    // as soon as we've seen the expected number of non-comment tokens.
    let magic = raw.get(..2).unwrap_or(&[]);
    // magic + width + height (PBM) or magic + width + height + maxval
    // (PGM/PPM). If the magic is unrecognized we default to 4 tokens to
    // be safe - this still stops before pixel data.
    let token_count_needed: usize = if matches!(magic, b"P1" | b"P4") { 3 } else { 4 };

    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0usize;
    let mut tokens_seen = 0usize;
    let mut last_was_whitespace = true; // synthesize a leading whitespace for counter logic

    while i < raw.len() && tokens_seen < token_count_needed {
        let b = raw[i];
        if b == b'#' {
            // Skip to end of line without writing the comment.
            while i < raw.len() && raw[i] != b'\n' {
                i += 1;
            }
            // Preserve the newline so the decoder still sees tokens
            // separated by whitespace.
            if i < raw.len() {
                out.push(b'\n');
                i += 1;
            }
            last_was_whitespace = true;
            continue;
        }
        if b.is_ascii_whitespace() {
            if !last_was_whitespace {
                tokens_seen += 1;
            }
            last_was_whitespace = true;
        } else if last_was_whitespace {
            last_was_whitespace = false;
        }
        out.push(b);
        i += 1;
    }

    // Everything after the header is pixel data - copy verbatim.
    out.extend_from_slice(&raw[i..]);
    out
}

#[cfg(all(test, feature = "native"))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn text_file_clean_is_copy() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("note.txt");
        let dst = dir.path().join("note.cleaned.txt");
        fs::write(&src, b"line one\nline two\n").unwrap();

        let h = HarmlessHandler;
        h.clean_metadata(&src, &dst).unwrap();
        assert_eq!(fs::read(&src).unwrap(), fs::read(&dst).unwrap());
    }

    #[test]
    fn text_file_read_is_empty() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("note.txt");
        fs::write(&src, b"hello").unwrap();
        let h = HarmlessHandler;
        let meta = h.read_metadata(&src).unwrap();
        assert!(meta.is_empty());
    }

    #[test]
    fn ppm_comments_are_read() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("image.ppm");
        // P3 ASCII PPM, 1x1, white
        fs::write(
            &src,
            b"P3\n# A metadata comment\n1 1\n# another one\n255\n255 255 255\n",
        )
        .unwrap();
        let h = HarmlessHandler;
        let meta = h.read_metadata(&src).unwrap();
        assert_eq!(meta.total_count(), 2);
    }

    #[test]
    fn ppm_comments_are_stripped_on_clean() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("image.ppm");
        let dst = dir.path().join("image.cleaned.ppm");
        fs::write(
            &src,
            b"P3\n# A metadata comment\n1 1\n255\n# trailing comment\n255 255 255\n",
        )
        .unwrap();

        let h = HarmlessHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read(&dst).unwrap();
        let out_str = String::from_utf8_lossy(&out);
        assert!(
            !out_str.contains("A metadata comment"),
            "header comment leaked: {out_str}"
        );
        // pixel data still present
        assert!(out_str.contains("255 255 255"));
    }
}
