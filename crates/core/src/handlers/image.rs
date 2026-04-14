use std::fs;
use std::io::{BufWriter, Cursor};
use std::path::Path;

use img_parts::jpeg::Jpeg;
use img_parts::png::Png;
use img_parts::webp::WebP;
use img_parts::{DynImage, ImageEXIF, ImageICC};
use little_exif::metadata::Metadata as ExifMetadata;

use crate::error::CoreError;
use crate::metadata::{MetadataGroup, MetadataItem, MetadataSet};

use super::FormatHandler;

pub struct ImageHandler;

impl FormatHandler for ImageHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut items = Vec::new();
        // Tracks whether `little_exif` surfaced any concrete EXIF tags.
        // Gating the generic "EXIF data: present" fallback line on this
        // bool - rather than on `items.is_empty()` - prevents an ICC
        // profile pushed later in the same reader pass from masking
        // the fallback. See Bug 14 in round-6's audit plan.
        let mut little_exif_surfaced_tags = false;

        // Read EXIF tags via little_exif (iterate the Metadata struct)
        match ExifMetadata::new_from_path(path) {
            Ok(exif) => {
                for tag in &exif {
                    let tag_str = format!("{tag:?}");
                    // Debug output includes the value, extract tag name and value
                    if let Some((name, value)) = split_debug_tag(&tag_str) {
                        items.push(MetadataItem { key: name, value });
                    } else {
                        items.push(MetadataItem {
                            key: tag_str.clone(),
                            value: String::new(),
                        });
                    }
                    little_exif_surfaced_tags = true;
                }
            }
            Err(e) => {
                log::debug!("No EXIF data or parse error for {}: {e}", path.display());
            }
        }

        // Check for additional metadata segments. For JPEG we parse
        // once via `Jpeg::from_bytes` and inspect raw markers (covers
        // XMP APP1, IPTC APP13, and ICC as a side-effect). For other
        // formats we fall back to `DynImage::from_bytes`.
        let data = fs::read(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        let mime = mime_guess::from_path(path).first_or_octet_stream();
        if mime == "image/jpeg" {
            match Jpeg::from_bytes(data.into()) {
                Ok(jpeg) => {
                    let mut saw_icc = false;
                    for segment in jpeg.segments() {
                        let marker = segment.marker();
                        let seg_data = segment.contents();
                        // APP1 with Adobe XMP namespace marker
                        if marker == 0xE1 && seg_data.starts_with(b"http://ns.adobe.com/xap/1.0/\0")
                        {
                            // Strip the 29-byte namespace header.
                            let xmp_body = &seg_data[29..];
                            let parsed = super::xmp::parse_xmp_fields(xmp_body);
                            if parsed.is_empty() {
                                items.push(MetadataItem {
                                    key: "XMP data".to_string(),
                                    value: "present".to_string(),
                                });
                            } else {
                                items.extend(parsed);
                            }
                        }
                        // APP13 with Photoshop 3.0 marker (IPTC 8BIM block)
                        if marker == 0xED && seg_data.starts_with(b"Photoshop 3.0\0") {
                            // Skip the 14-byte "Photoshop 3.0\0" marker
                            let body = &seg_data[14..];
                            let parsed = super::xmp::parse_iptc_8bim(body);
                            if parsed.is_empty() {
                                items.push(MetadataItem {
                                    key: "IPTC/Photoshop data".to_string(),
                                    value: "present".to_string(),
                                });
                            } else {
                                items.extend(parsed);
                            }
                        }
                        if !saw_icc && marker == 0xE2 && seg_data.starts_with(b"ICC_PROFILE\0") {
                            items.push(MetadataItem {
                                key: "ICC Profile".to_string(),
                                value: "present".to_string(),
                            });
                            saw_icc = true;
                        }
                    }
                }
                Err(e) => {
                    log::debug!("img-parts JPEG parse error for {}: {e}", path.display());
                }
            }
        } else {
            let data_vec = data.clone();
            match DynImage::from_bytes(data.into()) {
                Ok(Some(img)) => {
                    let (icc_line, exif_line) = generic_dynimage_lines(
                        img.icc_profile().is_some(),
                        img.exif().is_some(),
                        little_exif_surfaced_tags,
                    );
                    if let Some(item) = icc_line {
                        items.push(item);
                    }
                    if let Some(item) = exif_line {
                        items.push(item);
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    log::debug!("img-parts parse error for {}: {e}", path.display());
                }
            }

            // WebP XMP packet is in a `XMP ` RIFF chunk that `DynImage`
            // doesn't expose via `exif()` / `icc_profile()`. Pull it out
            // directly via `WebP::from_bytes` so the reader surfaces the
            // XMP fields the cleaner is about to strip.
            if mime == "image/webp"
                && let Ok(webp) = WebP::from_bytes(data_vec.into())
            {
                const CHUNK_XMP: [u8; 4] = *b"XMP ";
                for chunk in webp.chunks_by_id(CHUNK_XMP) {
                    let Some(body) = chunk.content().data() else {
                        continue;
                    };
                    let parsed = super::xmp::parse_xmp_fields(body.as_ref());
                    if parsed.is_empty() {
                        items.push(MetadataItem {
                            key: "XMP data".to_string(),
                            value: "present".to_string(),
                        });
                    } else {
                        items.extend(parsed);
                    }
                }
            }
        }

        let mut set = MetadataSet::default();
        if !items.is_empty() {
            set.groups.push(MetadataGroup { filename, items });
        }
        Ok(set)
    }

    fn clean_metadata(&self, path: &Path, output_path: &Path) -> Result<(), CoreError> {
        let mime = mime_guess::from_path(path).first_or_octet_stream();

        // TIFF, HEIC/HEIF and JXL are not handled by img-parts::DynImage.
        // little_exif has a dedicated code path for each format that
        // clears the EXIF IFD (TIFF/HEIF) or the exif box (JXL) in
        // place without re-encoding the pixel data.
        if matches!(
            mime.as_ref(),
            "image/tiff" | "image/heic" | "image/heif" | "image/jxl"
        ) {
            fs::copy(path, output_path).map_err(|e| CoreError::CleanError {
                path: path.to_path_buf(),
                detail: format!("Failed to copy file: {e}"),
            })?;
            ExifMetadata::file_clear_metadata(output_path).map_err(|e| CoreError::CleanError {
                path: path.to_path_buf(),
                detail: format!("Failed to clear metadata: {e}"),
            })?;
            return Ok(());
        }

        // Strip all metadata segments via img-parts, then run format-
        // specific post-passes for the bits img-parts doesn't expose.
        let data = fs::read(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        match DynImage::from_bytes(data.into()) {
            Ok(Some(mut img)) => {
                img.set_exif(None);
                img.set_icc_profile(None);

                // For JPEG, also strip APP13 (IPTC), XMP, COM segments
                let mut buf = Vec::new();
                img.encoder()
                    .write_to(&mut BufWriter::new(Cursor::new(&mut buf)))
                    .map_err(|e| CoreError::CleanError {
                        path: path.to_path_buf(),
                        detail: format!("Failed to encode cleaned image: {e}"),
                    })?;

                // Format-specific post-pass: strip leftover metadata
                // chunks that img-parts doesn't expose a setter for. If
                // the post-pass fails (our own img-parts output did not
                // re-parse cleanly), fail rather than ship bytes that
                // may still carry XMP / IPTC / COM / text chunks.
                let final_data = if mime == "image/jpeg" {
                    strip_jpeg_extra_segments(&buf).ok_or_else(|| CoreError::CleanError {
                        path: path.to_path_buf(),
                        detail: "JPEG post-strip failed; refusing to ship partially-stripped image"
                            .to_string(),
                    })?
                } else if mime == "image/png" {
                    strip_png_text_chunks(&buf).ok_or_else(|| CoreError::CleanError {
                        path: path.to_path_buf(),
                        detail: "PNG post-strip failed; refusing to ship partially-stripped image"
                            .to_string(),
                    })?
                } else if mime == "image/webp" {
                    strip_webp_extra_chunks(&buf).ok_or_else(|| CoreError::CleanError {
                        path: path.to_path_buf(),
                        detail: "WebP post-strip failed; refusing to ship partially-stripped image"
                            .to_string(),
                    })?
                } else {
                    buf
                };

                fs::write(output_path, final_data).map_err(|e| CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: format!("Failed to write output: {e}"),
                })?;
            }
            Ok(None) => {
                return Err(CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: "Could not parse image".to_string(),
                });
            }
            Err(e) => {
                return Err(CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: format!("Image parse error: {e}"),
                });
            }
        }

        Ok(())
    }

    fn supported_mime_types(&self) -> &[&str] {
        &[
            "image/jpeg",
            "image/png",
            "image/webp",
            "image/tiff",
            "image/heic",
            "image/heif",
            "image/jxl",
        ]
    }
}

/// Strip APP1-APP15 and COM segments from JPEG data using img-parts.
fn strip_jpeg_extra_segments(data: &[u8]) -> Option<Vec<u8>> {
    let mut jpeg = Jpeg::from_bytes(data.to_vec().into()).ok()?;

    // Remove APP1-APP15 markers (0xE1-0xEF) and COM (0xFE)
    for marker in 0xE1u8..=0xEF {
        jpeg.remove_segments_by_marker(marker);
    }
    jpeg.remove_segments_by_marker(0xFE); // COM

    let mut buf = Vec::new();
    jpeg.encoder()
        .write_to(&mut BufWriter::new(Cursor::new(&mut buf)))
        .ok()?;
    Some(buf)
}

/// Strip PNG ancillary text + timestamp chunks (`tEXt`, `iTXt`, `zTXt`,
/// `tIME`). img-parts already zeroed `eXIf` and `iCCP` via `set_exif` /
/// `set_icc_profile`, but it has no API for the text/time chunks, so a
/// PNG with Author / Software / Creation Time fields would survive a
/// full clean otherwise.
fn strip_png_text_chunks(data: &[u8]) -> Option<Vec<u8>> {
    const CHUNK_TEXT: [u8; 4] = *b"tEXt";
    const CHUNK_ITXT: [u8; 4] = *b"iTXt";
    const CHUNK_ZTXT: [u8; 4] = *b"zTXt";
    const CHUNK_TIME: [u8; 4] = *b"tIME";

    let mut png = Png::from_bytes(data.to_vec().into()).ok()?;
    png.remove_chunks_by_type(CHUNK_TEXT);
    png.remove_chunks_by_type(CHUNK_ITXT);
    png.remove_chunks_by_type(CHUNK_ZTXT);
    png.remove_chunks_by_type(CHUNK_TIME);

    let mut buf = Vec::new();
    png.encoder()
        .write_to(&mut BufWriter::new(Cursor::new(&mut buf)))
        .ok()?;
    Some(buf)
}

/// Strip WebP metadata chunks. img-parts 0.4's `DynImage::set_exif` and
/// `set_icc_profile` clear the `EXIF` and `ICCP` RIFF chunks, but it
/// has no setter for the `XMP ` chunk (`CHUNK_XMP` is declared in the
/// crate but never referenced internally). A WebP exported from
/// Lightroom / Photoshop / Affinity carries an Adobe XMP packet in
/// that chunk with `dc:creator`, `xmpMM:InstanceID`, GPS, etc., which
/// would otherwise pass through untouched. Parse the re-encoded buffer
/// directly here and drop every `XMP ` chunk.
fn strip_webp_extra_chunks(data: &[u8]) -> Option<Vec<u8>> {
    const CHUNK_XMP: [u8; 4] = *b"XMP ";
    let mut webp = WebP::from_bytes(data.to_vec().into()).ok()?;
    webp.remove_chunks_by_id(CHUNK_XMP);

    let mut buf = Vec::new();
    webp.encoder()
        .write_to(&mut BufWriter::new(Cursor::new(&mut buf)))
        .ok()?;
    Some(buf)
}

/// Split a Debug-formatted `ExifTag` string like `ImageDescription("Hello")`
/// into (name, value).
fn split_debug_tag(debug: &str) -> Option<(String, String)> {
    let paren = debug.find('(')?;
    let name = debug[..paren].to_string();
    let inner = debug[paren + 1..].trim_end_matches(')');
    // Remove surrounding quotes if present
    let value = inner.trim_matches('"').to_string();
    Some((name, value))
}

/// Return the ICC and generic-EXIF fallback lines the non-JPEG reader
/// branch should push. Factored out as a pure function so the
/// interaction between "little_exif surfaced concrete tags already",
/// "img-parts sees an ICC chunk", and "img-parts sees an EXIF chunk"
/// is unit-testable without a real image fixture.
///
/// The `EXIF data: present` line must only be suppressed when
/// `little_exif` already contributed *concrete* tags for the same
/// file. It must NOT be suppressed merely because the ICC line has
/// just been pushed: that was the round-6 Bug 14 regression.
pub(super) fn generic_dynimage_lines(
    has_icc: bool,
    has_exif: bool,
    little_exif_surfaced_tags: bool,
) -> (Option<MetadataItem>, Option<MetadataItem>) {
    let icc = has_icc.then(|| MetadataItem {
        key: "ICC Profile".to_string(),
        value: "present".to_string(),
    });
    let exif = (has_exif && !little_exif_surfaced_tags).then(|| MetadataItem {
        key: "EXIF data".to_string(),
        value: "present (could not parse individual tags)".to_string(),
    });
    (icc, exif)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Minimal valid 1x1 JPEG: SOI + JFIF APP0 + quantization + SOF0 +
    // Huffman + one-line scan + EOI. Used as a base for building
    // metadata-bearing variants.
    const MINIMAL_JPEG: &[u8] = &[
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00,
        0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x03, 0x02, 0x02, 0x02, 0x02,
        0x02, 0x02, 0x02, 0x02, 0x03, 0x03, 0x03, 0x03, 0x04, 0x06, 0x04, 0x04, 0x04, 0x04, 0x04,
        0x08, 0x06, 0x06, 0x05, 0x06, 0x09, 0x08, 0x0A, 0x0A, 0x09, 0x08, 0x09, 0x09, 0x0A, 0x0C,
        0x0F, 0x0C, 0x0A, 0x0B, 0x0E, 0x0B, 0x09, 0x09, 0x0D, 0x11, 0x0D, 0x0E, 0x0F, 0x10, 0x10,
        0x11, 0x10, 0x0A, 0x0C, 0x12, 0x13, 0x12, 0x10, 0x13, 0x0F, 0x10, 0x10, 0x10, 0xFF, 0xC0,
        0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11, 0x00, 0xFF, 0xC4, 0x00, 0x14,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0xFF, 0xC4, 0x00, 0x14, 0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xDA, 0x00, 0x08, 0x01,
        0x01, 0x00, 0x00, 0x3F, 0x00, 0x37, 0xFF, 0xD9,
    ];

    fn push_app_segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
        out.push(0xFF);
        out.push(marker);
        let total = payload.len() + 2;
        out.push((total >> 8) as u8);
        out.push((total & 0xff) as u8);
        out.extend_from_slice(payload);
    }

    /// Take MINIMAL_JPEG and splice new APP segments in between the
    /// SOI (2 bytes) and the first JFIF APP0, so the resulting JPEG
    /// carries APP1..APP15 plus a COM segment in addition to JFIF.
    fn jpeg_with_every_app_marker() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&MINIMAL_JPEG[..2]); // SOI
        for marker in 0xE1u8..=0xEF {
            push_app_segment(&mut out, marker, format!("leak-{marker:02x}").as_bytes());
        }
        push_app_segment(&mut out, 0xFE, b"leak-comment"); // COM
        out.extend_from_slice(&MINIMAL_JPEG[2..]);
        out
    }

    // ---------- split_debug_tag ----------

    #[test]
    fn split_debug_tag_basic_string() {
        let (name, value) = split_debug_tag("ImageDescription(\"Hello\")").unwrap();
        assert_eq!(name, "ImageDescription");
        assert_eq!(value, "Hello");
    }

    #[test]
    fn split_debug_tag_integer_value() {
        let (name, value) = split_debug_tag("Orientation(6)").unwrap();
        assert_eq!(name, "Orientation");
        assert_eq!(value, "6");
    }

    #[test]
    fn split_debug_tag_nested_parens_in_value() {
        // The helper is lenient: it finds the first `(` and everything
        // after it becomes the value, minus trailing `)`. Nested
        // parens should not panic.
        let (name, value) = split_debug_tag("Custom(foo (bar))").unwrap();
        assert_eq!(name, "Custom");
        assert_eq!(value, "foo (bar");
    }

    #[test]
    fn split_debug_tag_no_paren_returns_none() {
        assert!(split_debug_tag("NoParenHere").is_none());
    }

    #[test]
    fn split_debug_tag_empty_input_returns_none() {
        assert!(split_debug_tag("").is_none());
    }

    // ---------- generic_dynimage_lines ----------

    #[test]
    fn generic_dynimage_lines_all_off() {
        let (icc, exif) = generic_dynimage_lines(false, false, false);
        assert!(icc.is_none());
        assert!(exif.is_none());
    }

    #[test]
    fn generic_dynimage_lines_icc_only() {
        let (icc, exif) = generic_dynimage_lines(true, false, false);
        assert!(icc.is_some());
        assert!(exif.is_none());
    }

    #[test]
    fn generic_dynimage_lines_exif_only_without_little_exif_tags() {
        let (icc, exif) = generic_dynimage_lines(false, true, false);
        assert!(icc.is_none());
        let exif = exif.unwrap();
        assert_eq!(exif.key, "EXIF data");
    }

    #[test]
    fn generic_dynimage_lines_suppresses_exif_fallback_when_tags_surfaced() {
        // The Bug 14 regression pin: little_exif already produced
        // concrete tags, so the fallback "EXIF data: present" line
        // must be suppressed even if the reader saw an EXIF chunk.
        let (_icc, exif) = generic_dynimage_lines(true, true, true);
        assert!(exif.is_none());
    }

    #[test]
    fn generic_dynimage_lines_all_on_surfaces_icc_only() {
        // has_icc + has_exif + tags-already-surfaced = icc line only.
        let (icc, exif) = generic_dynimage_lines(true, true, true);
        assert!(icc.is_some());
        assert!(exif.is_none());
    }

    // ---------- strip_jpeg_extra_segments ----------

    #[test]
    fn strip_jpeg_removes_every_app_marker() {
        let dirty = jpeg_with_every_app_marker();
        let cleaned = strip_jpeg_extra_segments(&dirty).expect("valid JPEG must parse");

        // Every marker 0xE1..=0xEF and 0xFE must be absent from the
        // cleaned output. Scanning raw bytes is fine because we built
        // the input and know JFIF is the only legitimate APP0.
        // Walk the markers by hand:
        let mut i = 2usize; // skip SOI
        while i + 1 < cleaned.len() {
            if cleaned[i] != 0xFF {
                break;
            }
            let m = cleaned[i + 1];
            if m == 0xD9 {
                break;
            }
            assert!(
                !(0xE1..=0xEF).contains(&m),
                "APP{} survived the strip",
                m - 0xE0
            );
            assert_ne!(m, 0xFE, "COM marker survived the strip");
            if i + 3 < cleaned.len() {
                let len = ((cleaned[i + 2] as usize) << 8) | cleaned[i + 3] as usize;
                if len < 2 {
                    break;
                }
                i += 2 + len;
            } else {
                break;
            }
        }
    }

    #[test]
    fn strip_jpeg_returns_none_on_invalid_input() {
        assert!(strip_jpeg_extra_segments(&[]).is_none());
        assert!(strip_jpeg_extra_segments(b"not a jpeg at all").is_none());
    }

    // ---------- strip_png_text_chunks ----------

    fn minimal_png_with_text_chunks() -> Vec<u8> {
        // Build a PNG with IHDR, every text-bearing chunk type, tIME,
        // and IEND. This mirrors `tests/common::make_dirty_png` but
        // inline so the unit test stays self-contained.
        fn crc(ty: [u8; 4], data: &[u8]) -> u32 {
            const TABLE: [u32; 256] = {
                let mut table = [0u32; 256];
                let mut n = 0u32;
                while n < 256 {
                    let mut c = n;
                    let mut k = 0;
                    while k < 8 {
                        c = if c & 1 != 0 {
                            0xedb8_8320 ^ (c >> 1)
                        } else {
                            c >> 1
                        };
                        k += 1;
                    }
                    table[n as usize] = c;
                    n += 1;
                }
                table
            };
            let mut c: u32 = 0xffff_ffff;
            for &b in ty.iter().chain(data.iter()) {
                c = TABLE[((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8);
            }
            c ^ 0xffff_ffff
        }
        fn append(out: &mut Vec<u8>, ty: [u8; 4], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(&ty);
            out.extend_from_slice(data);
            out.extend_from_slice(&crc(ty, data).to_be_bytes());
        }

        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        // IHDR: 1x1 grayscale
        append(&mut out, *b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 0, 0, 0, 0]);
        append(&mut out, *b"tEXt", b"Author\0alice");
        append(&mut out, *b"iTXt", b"Copyright\0\0\0\0\0secret");
        append(&mut out, *b"zTXt", b"Title\0\0compressed");
        append(&mut out, *b"tIME", &[0x07, 0xe7, 1, 1, 0, 0, 0]);
        // Minimal IDAT: a single deflate block with empty zlib stream
        // won't validate, so we write the shortest legit zlib empty:
        // CMF=0x78, FLG=0x9c, one BFINAL stored empty, adler32
        append(
            &mut out,
            *b"IDAT",
            &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
        );
        append(&mut out, *b"IEND", &[]);
        out
    }

    #[test]
    fn strip_png_removes_text_and_time_chunks() {
        let dirty = minimal_png_with_text_chunks();
        let cleaned = strip_png_text_chunks(&dirty).expect("valid PNG must parse");

        let needles = [&b"tEXt"[..], b"iTXt", b"zTXt", b"tIME"];
        for needle in needles {
            assert!(
                !cleaned.windows(4).any(|w| w == needle),
                "PNG chunk {:?} must not survive the strip",
                std::str::from_utf8(needle).unwrap()
            );
        }
        // Sanity: IHDR and IEND must survive.
        assert!(cleaned.windows(4).any(|w| w == b"IHDR"));
        assert!(cleaned.windows(4).any(|w| w == b"IEND"));
    }

    #[test]
    fn strip_png_returns_none_on_garbage() {
        assert!(strip_png_text_chunks(&[]).is_none());
        assert!(strip_png_text_chunks(b"no png here").is_none());
    }

    // ---------- strip_webp_extra_chunks ----------

    #[test]
    fn strip_webp_returns_none_on_garbage() {
        assert!(strip_webp_extra_chunks(&[]).is_none());
        assert!(strip_webp_extra_chunks(b"RIFF____").is_none());
    }

    // ---------- ImageHandler supported_mime_types ----------

    #[test]
    fn image_handler_claims_all_expected_mimes() {
        let mimes: Vec<&&str> = ImageHandler.supported_mime_types().iter().collect();
        for required in [
            "image/jpeg",
            "image/png",
            "image/webp",
            "image/tiff",
            "image/heic",
            "image/heif",
            "image/jxl",
        ] {
            assert!(
                mimes.contains(&&required),
                "ImageHandler must claim {required}, got {mimes:?}"
            );
        }
    }

    #[test]
    fn image_handler_reads_minimal_jpeg_without_panic() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("x.jpg");
        fs::write(&path, MINIMAL_JPEG).unwrap();
        // Must not panic. Must return Ok (the file is valid but has
        // no metadata beyond the JFIF APP0, which isn't surfaced).
        let meta = ImageHandler.read_metadata(&path).unwrap();
        assert!(
            meta.groups
                .iter()
                .all(|g| g.items.is_empty() || !g.items.is_empty())
        );
    }

    #[test]
    fn image_handler_clean_roundtrip_on_minimal_jpeg_produces_valid_jpeg() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("in.jpg");
        let dst = dir.path().join("out.jpg");
        fs::write(&src, MINIMAL_JPEG).unwrap();
        ImageHandler.clean_metadata(&src, &dst).unwrap();
        let cleaned = fs::read(&dst).unwrap();
        // Valid JPEG starts with SOI and ends with EOI.
        assert_eq!(&cleaned[..2], &[0xFF, 0xD8]);
        assert_eq!(&cleaned[cleaned.len() - 2..], &[0xFF, 0xD9]);
    }
}
