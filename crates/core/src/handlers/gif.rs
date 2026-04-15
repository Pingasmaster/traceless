//! GIF metadata cleaner (byte-level).
//!
//! GIF is a chunked format. Metadata lives in two extension block types:
//! - Comment Extension   (0x21 0xFE ...)
//! - Application Extension (0x21 0xFF ...), used for XMP, ICC profiles,
//!   NETSCAPE2.0 loop counters, and author tools.
//!
//! We walk the GIF stream and drop every Comment Extension and every
//! Application Extension whose identifier isn't `NETSCAPE2.0` (looping
//! animations would break without that one). Graphic Control Extensions
//! and Plain Text Extensions — the other two types — are kept because
//! they carry rendering state, not identifying metadata.
//!
//! Reference: <https://www.w3.org/Graphics/GIF/spec-gif89a.txt>

use std::fs;
use std::path::Path;

use crate::error::CoreError;
use crate::metadata::{MetadataGroup, MetadataItem, MetadataSet};

use super::FormatHandler;

pub struct GifHandler;

impl FormatHandler for GifHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
        super::check_input_size(path)?;
        let bytes = fs::read(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;
        if !is_gif(&bytes) {
            return Err(CoreError::ParseError {
                path: path.to_path_buf(),
                detail: "not a GIF file (missing GIF87a/89a signature)".to_string(),
            });
        }
        let mut items = Vec::new();
        for block in walk_extension_blocks(&bytes) {
            match block {
                Extension::Comment(text) => {
                    items.push(MetadataItem {
                        key: "Comment".to_string(),
                        value: String::from_utf8_lossy(&text).into_owned(),
                    });
                }
                Extension::Application { identifier, blocks } => {
                    let id_str = String::from_utf8_lossy(&identifier).to_string();
                    // NETSCAPE2.0 is the animation loop count — not a
                    // fingerprint, keep it from the report.
                    if id_str.starts_with("NETSCAPE2.0") {
                        continue;
                    }
                    items.push(MetadataItem {
                        key: format!("Application: {id_str}"),
                        value: format!(
                            "{} sub-block(s), {} bytes total",
                            blocks.len(),
                            blocks.iter().map(Vec::len).sum::<usize>()
                        ),
                    });
                }
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
        if !is_gif(&bytes) {
            return Err(CoreError::ParseError {
                path: path.to_path_buf(),
                detail: "not a GIF file".to_string(),
            });
        }
        let cleaned = strip_gif_metadata(&bytes).ok_or_else(|| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: "GIF parse error during clean".to_string(),
        })?;
        fs::write(output_path, cleaned).map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("Failed to write cleaned GIF: {e}"),
        })?;
        Ok(())
    }

    fn supported_mime_types(&self) -> &[&str] {
        &["image/gif"]
    }
}

fn is_gif(bytes: &[u8]) -> bool {
    bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")
}

#[derive(Debug)]
enum Extension {
    Comment(Vec<u8>),
    Application {
        identifier: Vec<u8>,
        blocks: Vec<Vec<u8>>,
    },
}

/// Walk the GIF stream and yield every metadata-bearing extension block.
/// Used by the reader to surface leaks to the user.
fn walk_extension_blocks(bytes: &[u8]) -> Vec<Extension> {
    let mut out = Vec::new();
    let Some(mut i) = gif_header_size(bytes) else {
        return out;
    };

    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x3B {
            // Trailer
            break;
        }
        if b == 0x21 {
            // Extension introducer
            if i + 1 >= bytes.len() {
                break;
            }
            let label = bytes[i + 1];
            i += 2;
            match label {
                0xFE => {
                    // Comment extension: sub-blocks of text
                    let (text, next) = collect_sub_blocks(bytes, i);
                    let mut concat = Vec::new();
                    for sb in &text {
                        concat.extend_from_slice(sb);
                    }
                    out.push(Extension::Comment(concat));
                    i = next;
                }
                0xFF => {
                    // Application extension: 1 block of exactly 11
                    // bytes identifier (8 ident + 3 auth), then sub-blocks.
                    if i >= bytes.len() || bytes[i] != 0x0B {
                        // Truncated / malformed; stop walking instead
                        // of trying to advance through a broken stream.
                        i = skip_past_sub_blocks(bytes, i).unwrap_or(bytes.len());
                        continue;
                    }
                    let id_start = i + 1;
                    let id_end = id_start + 11;
                    if id_end > bytes.len() {
                        break;
                    }
                    let identifier = bytes[id_start..id_end].to_vec();
                    let (blocks, next) = collect_sub_blocks(bytes, id_end);
                    out.push(Extension::Application { identifier, blocks });
                    i = next;
                }
                _ => {
                    // Unknown / Graphic Control / Plain Text — skip.
                    i = skip_past_sub_blocks(bytes, i).unwrap_or(bytes.len());
                }
            }
        } else if b == 0x2C {
            // Image descriptor — read 9 header bytes, optional local
            // color table, then image data (LZW min code size byte +
            // sub-blocks).
            if i + 10 > bytes.len() {
                break;
            }
            let packed = bytes[i + 9];
            let has_lct = (packed & 0x80) != 0;
            let lct_size = if has_lct {
                3 * (1 << ((packed & 0x07) + 1))
            } else {
                0
            };
            i += 10 + lct_size;
            if i >= bytes.len() {
                break;
            }
            // LZW min code size
            i += 1;
            // Skip image data sub-blocks. A truncated reader-path GIF
            // is stopped cleanly: we surface whatever extensions we
            // already parsed and return them to the caller.
            i = skip_past_sub_blocks(bytes, i).unwrap_or(bytes.len());
        } else {
            // Unknown byte — advance to avoid infinite loop
            i += 1;
        }
    }
    out
}

/// Given `start` pointing at the first sub-block length byte, return
/// `(collected blocks, index just past the 0x00 terminator)`.
fn collect_sub_blocks(bytes: &[u8], start: usize) -> (Vec<Vec<u8>>, usize) {
    let mut i = start;
    let mut out = Vec::new();
    while i < bytes.len() {
        let n = bytes[i] as usize;
        if n == 0 {
            return (out, i + 1);
        }
        if i + 1 + n > bytes.len() {
            return (out, bytes.len());
        }
        out.push(bytes[i + 1..i + 1 + n].to_vec());
        i += 1 + n;
    }
    (out, i)
}

/// Skip the sub-block run starting at `i`, returning the index just
/// past the 0x00 terminator. Returns `None` if the run walks off the
/// end of `bytes` without hitting a terminator - this is a malformed
/// GIF and the cleaner path must refuse to emit a truncated stream
/// rather than slice into the buffer with an out-of-range index.
fn skip_past_sub_blocks(bytes: &[u8], mut i: usize) -> Option<usize> {
    while i < bytes.len() {
        let n = bytes[i] as usize;
        if n == 0 {
            return Some(i + 1);
        }
        // `i + 1 + n` must fit inside `bytes` for the next sub-block
        // header to be reachable. An `n == 255` with `i` near the end
        // of a short buffer would otherwise walk past `bytes.len()` and
        // the slice accesses in `strip_gif_metadata` would panic. The
        // `checked_add` also closes a theoretical wrap on pathologically
        // large `i` values that cannot occur with a real GIF but keeps
        // the strict arithmetic lint quiet.
        let next = i.checked_add(1)?.checked_add(n)?;
        if next > bytes.len() {
            return None;
        }
        i = next;
    }
    None
}

/// Return the size of the GIF logical screen descriptor + global color
/// table (if present), i.e. the first byte offset after the GIF header.
///
/// The header claims a GCT size of `3 * 2^(n+1)` bytes where `n` is the
/// low 3 bits of the packed byte, which tops out at 768 bytes on top of
/// the 13-byte screen descriptor. A truncated input whose declared GCT
/// runs off the end of the buffer must be rejected here, otherwise the
/// `&bytes[..header_end]` slice in `strip_gif_metadata` panics out of
/// range.
fn gif_header_size(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 13 {
        return None;
    }
    if !is_gif(bytes) {
        return None;
    }
    // 6 signature + 7 logical screen descriptor = 13
    let packed = bytes[10];
    let has_gct = (packed & 0x80) != 0;
    let gct_size = if has_gct {
        3 * (1 << ((packed & 0x07) + 1))
    } else {
        0
    };
    let end = 13 + gct_size;
    if end > bytes.len() {
        return None;
    }
    Some(end)
}

/// Produce a cleaned GIF: same as input minus Comment extensions and
/// non-NETSCAPE2.0 Application extensions. Returns `None` on any parse
/// error so the caller can fall back / error.
fn strip_gif_metadata(bytes: &[u8]) -> Option<Vec<u8>> {
    let header_end = gif_header_size(bytes)?;
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[..header_end]);

    let mut i = header_end;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x3B {
            out.push(b);
            i += 1;
            continue;
        }
        if b == 0x21 {
            if i + 1 >= bytes.len() {
                return None;
            }
            let label = bytes[i + 1];
            let block_start = i;
            let after_intro = i + 2;
            match label {
                0xFE => {
                    // Comment — skip entirely. A truncated sub-block
                    // stream means the input is malformed; propagate
                    // `None` so the caller surfaces a parse error
                    // instead of silently clipping the output.
                    i = skip_past_sub_blocks(bytes, after_intro)?;
                }
                0xFF => {
                    // Application — keep only NETSCAPE2.0
                    if after_intro >= bytes.len() || bytes[after_intro] != 0x0B {
                        i = skip_past_sub_blocks(bytes, after_intro)?;
                        continue;
                    }
                    let id_start = after_intro + 1;
                    let id_end = id_start + 11;
                    if id_end > bytes.len() {
                        return None;
                    }
                    let id = &bytes[id_start..id_end];
                    if id.starts_with(b"NETSCAPE2.0") {
                        let end = skip_past_sub_blocks(bytes, id_end)?;
                        out.extend_from_slice(&bytes[block_start..end]);
                        i = end;
                    } else {
                        i = skip_past_sub_blocks(bytes, id_end)?;
                    }
                }
                _ => {
                    // Graphic Control / Plain Text / unknown — keep.
                    let end = skip_past_sub_blocks(bytes, after_intro)?;
                    out.extend_from_slice(&bytes[block_start..end]);
                    i = end;
                }
            }
            continue;
        }
        if b == 0x2C {
            // Image descriptor — copy header + LCT + image data
            if i + 10 > bytes.len() {
                return None;
            }
            let packed = bytes[i + 9];
            let has_lct = (packed & 0x80) != 0;
            let lct_size = if has_lct {
                3 * (1 << ((packed & 0x07) + 1))
            } else {
                0
            };
            let header_copy_end = i + 10 + lct_size;
            if header_copy_end + 1 > bytes.len() {
                return None;
            }
            let data_end = skip_past_sub_blocks(bytes, header_copy_end + 1)?;
            out.extend_from_slice(&bytes[i..data_end]);
            i = data_end;
            continue;
        }
        // Unknown — pass through a single byte
        out.push(b);
        i += 1;
    }
    Some(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Build a minimum-viable 1x1 GIF89a with a Comment Extension
    /// and an Application Extension (non-NETSCAPE). Returns the bytes.
    fn make_dirty_gif() -> Vec<u8> {
        let mut gif = Vec::new();
        // Header
        gif.extend_from_slice(b"GIF89a");
        // Logical Screen Descriptor: 1x1, no global color table
        gif.extend_from_slice(&[
            0x01, 0x00, // width 1
            0x01, 0x00, // height 1
            0x00, // packed (no GCT)
            0x00, // bg color index
            0x00, // pixel aspect ratio
        ]);

        // Comment Extension: "secret comment"
        gif.extend_from_slice(&[0x21, 0xFE]);
        let comment = b"secret-comment";
        gif.push(comment.len() as u8);
        gif.extend_from_slice(comment);
        gif.push(0x00); // block terminator

        // Application Extension: identifier "XMP DataXMP" (XMP packet marker)
        gif.extend_from_slice(&[0x21, 0xFF, 0x0B]);
        gif.extend_from_slice(b"XMP DataXMP");
        let xmp_payload = b"secret-xmp-packet";
        gif.push(xmp_payload.len() as u8);
        gif.extend_from_slice(xmp_payload);
        gif.push(0x00);

        // Application Extension: NETSCAPE2.0 loop count (must survive)
        gif.extend_from_slice(&[0x21, 0xFF, 0x0B]);
        gif.extend_from_slice(b"NETSCAPE2.0");
        gif.extend_from_slice(&[0x03, 0x01, 0x00, 0x00, 0x00]);

        // Image Descriptor: 1x1 image at (0,0)
        gif.extend_from_slice(&[
            0x2C, // image separator
            0x00, 0x00, // left
            0x00, 0x00, // top
            0x01, 0x00, // width
            0x01, 0x00, // height
            0x00, // packed (no LCT)
        ]);
        // LZW min code size
        gif.push(0x02);
        // Image data sub-block: 2 bytes of LZW-ish data + terminator
        gif.push(0x02);
        gif.push(0x44);
        gif.push(0x01);
        gif.push(0x00);

        // Trailer
        gif.push(0x3B);

        gif
    }

    #[test]
    fn gif_read_surfaces_comment_and_app_ext() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("dirty.gif");
        fs::write(&src, make_dirty_gif()).unwrap();

        let h = GifHandler;
        let meta = h.read_metadata(&src).unwrap();
        let dump = format!("{meta:?}");
        assert!(
            dump.contains("secret-comment"),
            "comment not surfaced: {dump}"
        );
        assert!(dump.contains("XMP"), "XMP app ext not surfaced: {dump}");
        // NETSCAPE2.0 must NOT show up
        assert!(!dump.contains("NETSCAPE"));
    }

    #[test]
    fn gif_clean_strips_comment_and_xmp() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("dirty.gif");
        let dst = dir.path().join("clean.gif");
        fs::write(&src, make_dirty_gif()).unwrap();

        let h = GifHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read(&dst).unwrap();

        // Parse-level assertion: re-read must return empty
        let meta = h.read_metadata(&dst).unwrap();
        assert!(meta.is_empty(), "metadata survived clean: {meta:?}");

        // Byte-level assertion: literal payloads gone
        assert!(
            !out.windows(14).any(|w| w == b"secret-comment"),
            "comment bytes survived"
        );
        assert!(
            !out.windows(17).any(|w| w == b"secret-xmp-packet"),
            "XMP bytes survived"
        );
        // NETSCAPE loop block preserved
        assert!(
            out.windows(11).any(|w| w == b"NETSCAPE2.0"),
            "NETSCAPE2.0 must survive"
        );
        // GIF trailer still there
        assert_eq!(out.last(), Some(&0x3B));
    }

    #[test]
    fn gif_rejects_non_gif() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("not.gif");
        fs::write(&src, b"nope").unwrap();
        let h = GifHandler;
        assert!(h.read_metadata(&src).is_err());
    }

    /// Build a GIF whose last image-data sub-block claims length 5 but
    /// only contains 3 bytes before EOF, and omits the 0x00 terminator
    /// of the sub-block stream. This is exactly the shape
    /// `skip_past_sub_blocks` used to walk off the end of before round
    /// 15: the function would return an index past `bytes.len()`, and
    /// the subsequent `&bytes[i..data_end]` slice in
    /// `strip_gif_metadata` would panic with "range end index N out of
    /// range for slice of length M".
    fn make_truncated_subblock_gif() -> Vec<u8> {
        let mut g = Vec::new();
        g.extend_from_slice(b"GIF89a");
        // 1x1, no GCT
        g.extend_from_slice(&[0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00]);
        // Image Descriptor: 1x1 at (0,0), no LCT
        g.extend_from_slice(&[0x2C, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
        // LZW min code size
        g.push(0x02);
        // First sub-block: length 5, 5 bytes of data
        g.push(0x05);
        g.extend_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05]);
        // Second sub-block: claims length 5 but only 3 bytes follow,
        // and no 0x00 terminator after. This is what triggers the bug.
        g.push(0x05);
        g.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        g
    }

    #[test]
    fn gif_header_size_rejects_truncated_global_color_table() {
        // 13-byte GIF claiming a 256-entry GCT (3 * 2^(7+1) = 768 bytes
        // of color table) that isn't actually present. Before the fix,
        // `gif_header_size` would return `Some(13 + 768) = Some(781)`
        // and the `&bytes[..header_end]` slice in `strip_gif_metadata`
        // would panic out-of-range on a 13-byte buffer. The header
        // helper now checks that the declared GCT fits inside `bytes`
        // and returns `None` when it doesn't.
        let mut bytes = Vec::from(&b"GIF89a"[..]);
        // LSD: width=1, height=1, packed=0x87 (has_gct=1, gct_size=7),
        // bg color=0, aspect=0
        bytes.extend_from_slice(&[0x01, 0x00, 0x01, 0x00, 0x87, 0x00, 0x00]);
        assert_eq!(bytes.len(), 13);
        assert!(
            super::gif_header_size(&bytes).is_none(),
            "gif_header_size must reject a GIF whose declared GCT runs past EOF"
        );
        assert!(
            super::strip_gif_metadata(&bytes).is_none(),
            "strip_gif_metadata must not panic on a truncated GCT header"
        );
    }

    #[test]
    fn gif_clean_rejects_truncated_sub_block_stream_without_panic() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("truncated.gif");
        fs::write(&src, make_truncated_subblock_gif()).unwrap();
        let dst = dir.path().join("clean.gif");
        let h = GifHandler;
        let res = h.clean_metadata(&src, &dst);
        // Must be a terminal `CleanError`, not a panic. The worker
        // panic-safety wrapper in file_store catches unwinds and turns
        // them into `FileError`s, but we want the cleaner itself to
        // surface a specific parse-error instead of relying on that
        // safety net.
        match res {
            Err(CoreError::CleanError { detail, .. }) => {
                assert!(
                    detail.contains("GIF parse error"),
                    "expected GIF parse error, got: {detail}"
                );
            }
            other => panic!("expected CoreError::CleanError for truncated GIF, got: {other:?}"),
        }
    }

    #[test]
    fn gif_read_truncated_sub_block_stream_does_not_panic() {
        // The reader path keeps graceful-stop behaviour: a truncated
        // stream must not panic and must not crash; it should return
        // whatever extensions were parsed before the truncation
        // (possibly none).
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("truncated.gif");
        fs::write(&src, make_truncated_subblock_gif()).unwrap();
        let h = GifHandler;
        let _ = h
            .read_metadata(&src)
            .expect("reader must not panic on truncated GIF");
    }
}
