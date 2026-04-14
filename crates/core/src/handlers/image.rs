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
                        items.push(MetadataItem {
                            key: name,
                            value,
                        });
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
                        if marker == 0xE1
                            && seg_data.starts_with(b"http://ns.adobe.com/xap/1.0/\0")
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
                        if marker == 0xED
                            && seg_data.starts_with(b"Photoshop 3.0\0")
                        {
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
                        if !saw_icc
                            && marker == 0xE2
                            && seg_data.starts_with(b"ICC_PROFILE\0")
                        {
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

    fn clean_metadata(
        &self,
        path: &Path,
        output_path: &Path,
    ) -> Result<(), CoreError> {
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
            ExifMetadata::file_clear_metadata(output_path).map_err(|e| {
                CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: format!("Failed to clear metadata: {e}"),
                }
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
                        detail:
                            "JPEG post-strip failed; refusing to ship partially-stripped image"
                                .to_string(),
                    })?
                } else if mime == "image/png" {
                    strip_png_text_chunks(&buf).ok_or_else(|| CoreError::CleanError {
                        path: path.to_path_buf(),
                        detail:
                            "PNG post-strip failed; refusing to ship partially-stripped image"
                                .to_string(),
                    })?
                } else if mime == "image/webp" {
                    strip_webp_extra_chunks(&buf).ok_or_else(|| CoreError::CleanError {
                        path: path.to_path_buf(),
                        detail:
                            "WebP post-strip failed; refusing to ship partially-stripped image"
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
