#[cfg(feature = "native")]
use std::fs;
use std::io::{BufWriter, Cursor};
#[cfg(feature = "native")]
use std::path::Path;

#[cfg(feature = "native")]
use img_parts::Bytes;
use img_parts::jpeg::Jpeg;
use img_parts::png::Png;
use img_parts::webp::WebP;
use img_parts::{DynImage, ImageEXIF, ImageICC};
use little_exif::filetype::FileExtension;
use little_exif::metadata::Metadata as ExifMetadata;

use crate::error::CoreError;
#[cfg(feature = "native")]
use crate::metadata::{MetadataGroup, MetadataItem, MetadataSet};

#[cfg(feature = "native")]
use super::FormatHandler;

pub struct ImageHandler;

/// In-memory MIME dispatch shared by the native `clean_metadata` wrapper
/// and the wasm `inmem` path: map a lowercase extension to the MIME the
/// img-parts / little_exif logic branches on. Returns `None` for an
/// extension this handler does not own.
fn ext_to_mime(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "tif" | "tiff" => "image/tiff",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "jxl" => "image/jxl",
        _ => return None,
    })
}

/// Strip every metadata segment from an image, in memory. This is the
/// single source of truth for the image cleaner: the native
/// `clean_metadata` reads the file, calls this, and writes the result;
/// the wasm `inmem` dispatch calls it directly. `ext` is the lowercase
/// filename extension (no dot); it drives MIME selection.
///
/// # Errors
///
/// Returns [`CoreError::ParseError`] if the input cannot be parsed as a
/// supported image, [`CoreError::CleanError`] if a post-strip pass fails, or
/// [`CoreError::UnsupportedFormat`] for an extension this handler does
/// not own.
pub(crate) fn clean_bytes(input: &[u8], ext: &str) -> Result<Vec<u8>, CoreError> {
    super::check_input_len(input.len())?;
    let Some(mime) = ext_to_mime(ext) else {
        return Err(CoreError::UnsupportedFormat {
            mime_type: format!("image handler: unknown extension '{ext}'"),
        });
    };

    // TIFF is not handled by img-parts::DynImage. little_exif clears the
    // EXIF IFD in place over a byte buffer without re-encoding the pixel
    // data. (TIFF carries its metadata in the EXIF IFD only, so the
    // Exif-only clear is sufficient here.)
    if mime == "image/tiff" {
        let mut buf = input.to_vec();
        ExifMetadata::clear_metadata(&mut buf, FileExtension::TIFF).map_err(|e| {
            CoreError::CleanError {
                path: std::path::PathBuf::new(),
                detail: format!("Failed to clear metadata: {e}"),
            }
        })?;
        return Ok(buf);
    }

    // JPEG-XL: little_exif's `clear_metadata(.., JXL)` only drops the
    // `Exif` (and `brob`-wrapped-Exif) box, leaving the `xml ` (XMP) and
    // `jumb`/`jumbf` (JUMBF / C2PA) boxes that carry the most identifying
    // metadata (dc:creator, GPS, xmpMM:InstanceID). Walk the ISO-BMFF box
    // list ourselves and drop every metadata box. A bare codestream
    // (0xFF 0x0A) carries no metadata and is returned unchanged.
    if mime == "image/jxl" {
        return strip_jxl_boxes(input);
    }

    // HEIC/HEIF: little_exif's HEIF clear only blanks the Exif *item*; it
    // never touches the separate XMP item (`infe` of content-type
    // application/rdf+xml) nor the `colr` box that holds the embedded ICC
    // profile. Both fingerprint the capture/edit device. We first run the
    // little_exif Exif clear, then sweep the ISO-BMFF tree ourselves to
    // neutralize the XMP item payload and the ICC profile in every `colr`
    // box. Zeroing in place (rather than deleting bytes) keeps every
    // `iloc` extent offset valid so the file still decodes.
    if matches!(mime, "image/heic" | "image/heif") {
        // Pre-validate the top-level ISO-BMFF chain BEFORE handing the bytes
        // to `little_exif`. Its HEIF reader trusts the 32-bit box-size field
        // and allocates it outright, so an 8-byte body whose header claims
        // 0xd743e620 makes it request 3.36 GiB (found by fuzzing, 2026-07-29;
        // reproduced against 0.6.23). In the wasm component that request
        // exceeds the 3 GiB linear-memory ceiling and traps the instance:
        // a remote DoS from a 9-byte request. Our own walker (`parse_box`)
        // already rejects out-of-bounds extents, so reuse it as the gate.
        if !heif_top_level_boxes_fit(input) {
            return Err(CoreError::CleanError {
                path: std::path::PathBuf::new(),
                detail: "HEIF: malformed box structure (a box size exceeds the file); refusing to parse".to_string(),
            });
        }
        let mut buf = input.to_vec();
        ExifMetadata::clear_metadata(&mut buf, FileExtension::HEIF).map_err(|e| {
            CoreError::CleanError {
                path: std::path::PathBuf::new(),
                detail: format!("Failed to clear metadata: {e}"),
            }
        })?;
        let swept = strip_heif_extra_metadata(&buf).ok_or_else(|| CoreError::CleanError {
            path: std::path::PathBuf::new(),
            detail: "HEIF post-strip failed; refusing to ship partially-stripped image".to_string(),
        })?;
        return Ok(swept);
    }

    match DynImage::from_bytes(input.to_vec().into()) {
        Ok(Some(mut img)) => {
            img.set_exif(None);
            img.set_icc_profile(None);

            // For JPEG, also strip APP13 (IPTC), XMP, COM segments.
            let mut buf = Vec::new();
            img.encoder()
                .write_to(&mut BufWriter::new(Cursor::new(&mut buf)))
                .map_err(|e| CoreError::CleanError {
                    path: std::path::PathBuf::new(),
                    detail: format!("Failed to encode cleaned image: {e}"),
                })?;

            // Format-specific post-pass: strip leftover metadata chunks
            // img-parts doesn't expose a setter for. If the post-pass
            // fails (our own img-parts output did not re-parse cleanly),
            // fail rather than ship bytes that may still carry XMP / IPTC
            // / COM / text chunks.
            let final_data = if mime == "image/jpeg" {
                strip_jpeg_extra_segments(&buf).ok_or_else(|| CoreError::CleanError {
                    path: std::path::PathBuf::new(),
                    detail: "JPEG post-strip failed; refusing to ship partially-stripped image"
                        .to_string(),
                })?
            } else if mime == "image/png" {
                strip_png_text_chunks(&buf).ok_or_else(|| CoreError::CleanError {
                    path: std::path::PathBuf::new(),
                    detail: "PNG post-strip failed; refusing to ship partially-stripped image"
                        .to_string(),
                })?
            } else if mime == "image/webp" {
                strip_webp_extra_chunks(&buf).ok_or_else(|| CoreError::CleanError {
                    path: std::path::PathBuf::new(),
                    detail: "WebP post-strip failed; refusing to ship partially-stripped image"
                        .to_string(),
                })?
            } else {
                buf
            };
            Ok(final_data)
        }
        // Input does not parse as an image -> ParseError (HTTP 422), not
        // CleanError (500): a malformed/truncated/wrong-format body is a client
        // error, mirroring the native path and the gif/svg handlers. Only the
        // post-parse strip/encode failures above stay CleanError.
        Ok(None) => Err(CoreError::ParseError {
            path: std::path::PathBuf::new(),
            detail: "Could not parse image".to_string(),
        }),
        Err(e) => Err(CoreError::ParseError {
            path: std::path::PathBuf::new(),
            detail: format!("Image parse error: {e}"),
        }),
    }
}

#[cfg(feature = "native")]
impl FormatHandler for ImageHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
        super::check_input_size(path)?;
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

        // Share a single refcounted `Bytes` between every parser we
        // hand the file to. `img_parts::Bytes` is re-exported from the
        // `bytes` crate, so cloning it is an atomic refcount bump - not
        // a buffer copy. This replaces an earlier `data_vec = data.clone()`
        // that unconditionally copied the full file for every non-JPEG
        // image, even when the WebP fallback branch never fired.
        let shared: Bytes = data.into();
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        if mime == "image/jpeg" {
            match Jpeg::from_bytes(shared) {
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
            match DynImage::from_bytes(shared.clone()) {
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
                && let Ok(webp) = WebP::from_bytes(shared)
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
        super::check_input_size(path)?;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();

        // Single logic path: read the bytes, clean in memory, write the
        // result. `clean_bytes` is shared verbatim with the wasm build.
        let data = fs::read(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;
        let cleaned = clean_bytes(&data, &ext).map_err(|e| super::repath(e, path))?;
        fs::write(output_path, cleaned).map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("Failed to write output: {e}"),
        })?;
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

// -------------------------------------------------------------------------
// JPEG-XL ISO-BMFF metadata stripper
// -------------------------------------------------------------------------

/// JPEG-XL metadata stripper. `little_exif`'s `clear_metadata(.., JXL)`
/// only drops the `Exif` (and `brob`-wrapped-Exif) box, leaving the
/// `xml ` (XMP) and `jumb`/`jumbf` (JUMBF / C2PA) boxes that carry the
/// most identifying metadata (dc:creator, GPS, xmpMM:InstanceID). We walk
/// the ISO-BMFF box list ourselves and drop every metadata box, keeping
/// only structural and codestream boxes. A bare codestream (`0xFF 0x0A`)
/// carries no metadata, so it is returned unchanged.
///
/// JXL container boxes are flat (no internal absolute-offset references
/// the way HEIF `iloc` has), so physically removing a metadata box is
/// safe and does not corrupt sibling boxes.
fn strip_jxl_boxes(input: &[u8]) -> Result<Vec<u8>, CoreError> {
    const SIG_CODESTREAM: [u8; 2] = [0xFF, 0x0A];
    const SIG_ISOBMFF: [u8; 12] = [
        0x00, 0x00, 0x00, 0x0C, 0x4A, 0x58, 0x4C, 0x20, 0x0D, 0x0A, 0x87, 0x0A,
    ];

    let parse_err = |detail: &str| CoreError::ParseError {
        path: std::path::PathBuf::new(),
        detail: detail.to_string(),
    };

    // Bare codestream: no container, no metadata. Clean passthrough (NOT a
    // 500). Must be checked before the ISO-BMFF branch.
    if input.starts_with(&SIG_CODESTREAM) {
        return Ok(input.to_vec());
    }
    if !input.starts_with(&SIG_ISOBMFF) {
        return Err(parse_err("Not an ISO-BMFF JXL file"));
    }

    // Box types to DROP. `brob` payloads are inspected for their wrapped
    // type so a brob-compressed xml/Exif/jumb is also dropped.
    // ISO-BMFF box types are exactly 4 bytes: `xml ` (XMP), `Exif`, and
    // `jumb` (the JUMBF superbox, which carries C2PA / JPEG-universal
    // metadata). All four-byte fourccs.
    const fn is_metadata_type(ty: [u8; 4]) -> bool {
        matches!(&ty, b"xml " | b"Exif" | b"jumb")
    }

    let len = input.len();
    let mut out: Vec<u8> = Vec::with_capacity(len);
    let mut pos: usize = 0;

    while pos + 8 <= len {
        let box_size =
            u32::from_be_bytes([input[pos], input[pos + 1], input[pos + 2], input[pos + 3]])
                as usize;
        let ty = [
            input[pos + 4],
            input[pos + 5],
            input[pos + 6],
            input[pos + 7],
        ];

        // Resolve actual box extent. size==1 => 64-bit largesize in the
        // next 8 bytes; size==0 => box runs to EOF.
        let box_end = if box_size == 1 {
            if pos + 16 > len {
                return Err(parse_err("Truncated JXL extended-size box"));
            }
            let large = u64::from_be_bytes([
                input[pos + 8],
                input[pos + 9],
                input[pos + 10],
                input[pos + 11],
                input[pos + 12],
                input[pos + 13],
                input[pos + 14],
                input[pos + 15],
            ]);
            let large = usize::try_from(large).map_err(|_| parse_err("JXL box too large"))?;
            pos.checked_add(large)
                .ok_or_else(|| parse_err("JXL box length overflow"))?
        } else if box_size == 0 {
            len
        } else {
            pos.checked_add(box_size)
                .ok_or_else(|| parse_err("JXL box length overflow"))?
        };

        if box_size != 0 && (box_end < pos + 8 || box_end > len) {
            return Err(parse_err("JXL box length out of range"));
        }

        // Decide whether to drop. For `brob`, inspect the wrapped type
        // (first 4 bytes of payload) to catch brob-compressed metadata.
        let drop = if &ty == b"brob" {
            let payload = pos + 8 + if box_size == 1 { 8 } else { 0 };
            payload + 4 <= box_end
                && is_metadata_type([
                    input[payload],
                    input[payload + 1],
                    input[payload + 2],
                    input[payload + 3],
                ])
        } else {
            is_metadata_type(ty)
        };

        if !drop {
            out.extend_from_slice(&input[pos..box_end]);
        }

        pos = box_end;
        if box_size == 0 {
            break;
        }
    }

    Ok(out)
}

// -------------------------------------------------------------------------
// HEIC / HEIF ISO-BMFF metadata stripper
// -------------------------------------------------------------------------

/// Read a big-endian `u32` at `off`, or `None` if out of range.
fn be_u32(buf: &[u8], off: usize) -> Option<u32> {
    let end = off.checked_add(4)?;
    if end > buf.len() {
        return None;
    }
    Some(u32::from_be_bytes([
        buf[off],
        buf[off + 1],
        buf[off + 2],
        buf[off + 3],
    ]))
}

/// Resolve one ISO-BMFF box header at `pos`. Returns
/// `(payload_start, box_end, fourcc)`. Handles 64-bit `largesize`
/// (`size == 1`) and box-to-EOF (`size == 0`). Returns `None` on any
/// truncation / overflow / out-of-range length.
fn parse_box(buf: &[u8], pos: usize) -> Option<(usize, usize, [u8; 4])> {
    if pos + 8 > buf.len() {
        return None;
    }
    let size32 = be_u32(buf, pos)? as usize;
    let ty = [buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]];
    let (payload_start, box_end) = if size32 == 1 {
        if pos + 16 > buf.len() {
            return None;
        }
        let large = u64::from_be_bytes([
            buf[pos + 8],
            buf[pos + 9],
            buf[pos + 10],
            buf[pos + 11],
            buf[pos + 12],
            buf[pos + 13],
            buf[pos + 14],
            buf[pos + 15],
        ]);
        let large = usize::try_from(large).ok()?;
        (pos + 16, pos.checked_add(large)?)
    } else if size32 == 0 {
        (pos + 8, buf.len())
    } else {
        (pos + 8, pos.checked_add(size32)?)
    };
    if box_end < payload_start || box_end > buf.len() {
        return None;
    }
    Some((payload_start, box_end, ty))
}

/// HEIF metadata sweep performed *after* the little_exif Exif clear. It
/// (1) finds every XMP item (an `infe` of item_type `mime` whose
/// content_type is `application/rdf+xml`) and zeroes its data extents, and
/// (2) zeroes the ICC payload of every `colr` box of type `prof` / `rICC`.
///
/// Removal is done by overwriting the identifying bytes with zero rather
/// than physically deleting them: HEIF `iloc` extents are absolute byte
/// offsets, so shrinking the buffer would corrupt every other item's
/// offsets. Zeroing keeps the container structurally valid and decodable
/// while making the XMP packet and ICC profile unrecoverable.
///
/// Returns `None` only on a structural parse failure (so the caller can
/// refuse to ship a partially-stripped image); a HEIF that simply has no
/// XMP / ICC returns `Some` with the buffer unchanged.
/// Walk the top-level ISO-BMFF box chain and report whether every box fits
/// inside `input`.
///
/// `parse_box` returns `None` for any box whose declared extent runs past the
/// buffer (or whose 64-bit largesize does not fit a `usize`), which is exactly
/// the shape that makes `little_exif`'s HEIF reader allocate the declared size
/// on trust. A trailing run shorter than a box header is tolerated: that is a
/// truncated tail, not an amplification primitive.
fn heif_top_level_boxes_fit(input: &[u8]) -> bool {
    let mut pos = 0usize;
    while pos + 8 <= input.len() {
        let Some((_payload, end, _ty)) = parse_box(input, pos) else {
            return false;
        };
        if end <= pos {
            return false; // zero-length box: refuse rather than spin
        }
        pos = end;
    }
    true
}

fn strip_heif_extra_metadata(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = input.to_vec();

    // Locate the top-level `meta` box. Also note `idat`'s payload start so
    // construction_method==1 (idat-relative) extents resolve correctly.
    let mut meta: Option<(usize, usize)> = None; // (payload_start, end)
    let mut pos = 0usize;
    while pos + 8 <= out.len() {
        let (pstart, end, ty) = parse_box(&out, pos)?;
        if &ty == b"meta" {
            meta = Some((pstart, end));
        }
        if end <= pos {
            return None; // zero-length: refuse rather than spin
        }
        pos = end;
    }
    let Some((meta_payload, meta_end)) = meta else {
        // No meta box: nothing item-based to strip (and no colr lives
        // outside meta). Return unchanged.
        return Some(out);
    };

    // `meta` is a FullBox: 4 bytes version/flags before its child boxes.
    let meta_children = meta_payload.checked_add(4)?;
    if meta_children > meta_end {
        return None;
    }

    // Walk meta's children to find iinf, iloc, idat, iprp.
    let mut iinf: Option<(usize, usize)> = None;
    let mut iloc: Option<(usize, usize)> = None;
    let mut idat_payload: Option<usize> = None;
    let mut iprp: Option<(usize, usize)> = None;
    let mut c = meta_children;
    while c + 8 <= meta_end {
        let (pstart, end, ty) = parse_box(&out, c)?;
        match &ty {
            b"iinf" => iinf = Some((pstart, end)),
            b"iloc" => iloc = Some((pstart, end)),
            b"idat" => idat_payload = Some(pstart),
            b"iprp" => iprp = Some((pstart, end)),
            _ => {}
        }
        if end <= c {
            return None;
        }
        c = end;
    }

    // (1) Collect XMP item IDs from iinf.
    let mut xmp_ids: Vec<u32> = Vec::new();
    if let Some((iinf_payload, iinf_end)) = iinf {
        // iinf FullBox: version(1)+flags(3), then entry_count
        // (u16 for version 0, u32 for version >= 1), then `infe` children.
        let version = *out.get(iinf_payload)?;
        let mut ic = if version == 0 {
            iinf_payload.checked_add(4 + 2)?
        } else {
            iinf_payload.checked_add(4 + 4)?
        };
        while ic + 8 <= iinf_end {
            let (infe_payload, infe_end, ty) = parse_box(&out, ic)?;
            if &ty == b"infe"
                && let Some(id) = parse_infe_xmp_id(&out, infe_payload, infe_end)
            {
                xmp_ids.push(id);
            }
            if infe_end <= ic {
                return None;
            }
            ic = infe_end;
        }
    }

    // (2) For each XMP item, zero its data extents (from iloc).
    if !xmp_ids.is_empty()
        && let Some((iloc_payload, iloc_end)) = iloc
    {
        zero_item_extents(&mut out, iloc_payload, iloc_end, idat_payload, &xmp_ids)?;
    }

    // (3) Zero the ICC payload of every `colr` box of type prof/rICC,
    // found under meta -> iprp -> ipco.
    if let Some((iprp_payload, iprp_end)) = iprp {
        let mut p = iprp_payload;
        while p + 8 <= iprp_end {
            let (ipco_payload, ipco_end, ty) = parse_box(&out, p)?;
            if &ty == b"ipco" {
                zero_colr_icc(&mut out, ipco_payload, ipco_end)?;
            }
            if ipco_end <= p {
                return None;
            }
            p = ipco_end;
        }
    }

    Some(out)
}

/// Parse one `infe` (FullBox) payload and return the item_ID iff this is an
/// XMP item: item_type == `mime` and content_type == `application/rdf+xml`.
/// Supports infe version 2 (16-bit id) and version 3 (32-bit id).
fn parse_infe_xmp_id(buf: &[u8], payload: usize, end: usize) -> Option<u32> {
    let version = *buf.get(payload)?;
    // After version(1)+flags(3):
    let mut p = payload.checked_add(4)?;
    let item_id: u32 = match version {
        2 => {
            let v = u16::from_be_bytes([*buf.get(p)?, *buf.get(p + 1)?]);
            p = p.checked_add(2)?;
            u32::from(v)
        }
        3 => {
            let v = be_u32(buf, p)?;
            p = p.checked_add(4)?;
            v
        }
        // version 0/1 use a different layout without item_type; XMP items
        // in practice always use version >= 2. Ignore older forms.
        _ => return None,
    };
    // item_protection_index (u16), then item_type (4 bytes).
    p = p.checked_add(2)?;
    let item_type = [
        *buf.get(p)?,
        *buf.get(p + 1)?,
        *buf.get(p + 2)?,
        *buf.get(p + 3)?,
    ];
    p = p.checked_add(4)?;
    if &item_type != b"mime" {
        return None;
    }
    // item_name: null-terminated string, then content_type:
    // null-terminated string. Skip item_name.
    while p < end && *buf.get(p)? != 0 {
        p = p.checked_add(1)?;
    }
    p = p.checked_add(1)?; // skip the null terminator
    // Read content_type up to its null terminator.
    let ct_start = p;
    while p < end && *buf.get(p)? != 0 {
        p = p.checked_add(1)?;
    }
    let content_type = buf.get(ct_start..p)?;
    if content_type == b"application/rdf+xml" {
        Some(item_id)
    } else {
        None
    }
}

/// Parse `iloc` and zero the data bytes of every extent belonging to an
/// item in `ids`. Returns `None` on a structural parse failure.
fn zero_item_extents(
    out: &mut [u8],
    payload: usize,
    end: usize,
    idat_payload: Option<usize>,
    ids: &[u32],
) -> Option<()> {
    // iloc FullBox: version(1)+flags(3).
    let version = *out.get(payload)?;
    let mut p = payload.checked_add(4)?;
    // 1 byte: (offset_size << 4) | length_size
    let b0 = *out.get(p)?;
    let offset_size = usize::from(b0 >> 4);
    let length_size = usize::from(b0 & 0x0F);
    // 1 byte: (base_offset_size << 4) | index_size(v1/2) / reserved(v0)
    let b1 = *out.get(p + 1)?;
    let base_offset_size = usize::from(b1 >> 4);
    let index_size = if version == 1 || version == 2 {
        usize::from(b1 & 0x0F)
    } else {
        0
    };
    p = p.checked_add(2)?;
    // item_count: u16 (version < 2) or u32 (version 2).
    let item_count: u32 = if version < 2 {
        let v = u16::from_be_bytes([*out.get(p)?, *out.get(p + 1)?]);
        p = p.checked_add(2)?;
        u32::from(v)
    } else {
        let v = be_u32(out, p)?;
        p = p.checked_add(4)?;
        v
    };

    let read_uint = |out: &[u8], at: usize, size: usize| -> Option<u64> {
        if size == 0 {
            return Some(0);
        }
        let bytes = out.get(at..at.checked_add(size)?)?;
        let mut v: u64 = 0;
        for &b in bytes {
            v = (v << 8) | u64::from(b);
        }
        Some(v)
    };

    for _ in 0..item_count {
        // Stop if the declared item_count over-runs the iloc box bounds.
        if p >= end {
            break;
        }
        // item_ID: u16 (version < 2) or u32 (version 2).
        let item_id: u32 = if version < 2 {
            let v = u16::from_be_bytes([*out.get(p)?, *out.get(p + 1)?]);
            p = p.checked_add(2)?;
            u32::from(v)
        } else {
            let v = be_u32(out, p)?;
            p = p.checked_add(4)?;
            v
        };
        // construction_method (only v1/v2): reserved(12 bits)+method(4 bits) = u16.
        let construction_method = if version == 1 || version == 2 {
            let v = u16::from_be_bytes([*out.get(p)?, *out.get(p + 1)?]);
            p = p.checked_add(2)?;
            (v & 0x0F) as u8
        } else {
            0
        };
        // data_reference_index: u16.
        p = p.checked_add(2)?;
        let base_offset = read_uint(out, p, base_offset_size)?;
        p = p.checked_add(base_offset_size)?;
        // extent_count: u16.
        let extent_count = u16::from_be_bytes([*out.get(p)?, *out.get(p + 1)?]);
        p = p.checked_add(2)?;

        let want = ids.contains(&item_id);
        for _ in 0..extent_count {
            if (version == 1 || version == 2) && index_size > 0 {
                p = p.checked_add(index_size)?;
            }
            let extent_offset = read_uint(out, p, offset_size)?;
            p = p.checked_add(offset_size)?;
            let extent_length = read_uint(out, p, length_size)?;
            p = p.checked_add(length_size)?;

            if !want {
                continue;
            }
            // construction_method: 0 = file offset, 1 = idat-relative.
            let base = if construction_method == 1 {
                u64::try_from(idat_payload?).ok()?
            } else {
                0
            };
            let start =
                usize::try_from(base.checked_add(base_offset)?.checked_add(extent_offset)?).ok()?;
            let elen = usize::try_from(extent_length).ok()?;
            let stop = start.checked_add(elen)?;
            // Only zero in-range extents; a malformed offset must not panic.
            if start <= out.len() && stop <= out.len() {
                for byte in &mut out[start..stop] {
                    *byte = 0;
                }
            }
        }
    }
    Some(())
}

/// Within an `ipco` box, zero the ICC payload of every `colr` box whose
/// colour_type is `prof` (embedded ICC) or `rICC` (restricted ICC).
fn zero_colr_icc(out: &mut [u8], ipco_payload: usize, ipco_end: usize) -> Option<()> {
    let mut p = ipco_payload;
    while p + 8 <= ipco_end {
        let (cpayload, cend, ty) = parse_box(out, p)?;
        if &ty == b"colr" {
            // colour_type is the first 4 bytes of the colr payload.
            if cpayload + 4 <= cend {
                let ctype = [
                    out[cpayload],
                    out[cpayload + 1],
                    out[cpayload + 2],
                    out[cpayload + 3],
                ];
                if &ctype == b"prof" || &ctype == b"rICC" {
                    // Zero everything after the 4-byte colour_type (the
                    // embedded ICC profile bytes). The box header and
                    // colour_type stay intact so sizes/offsets are
                    // preserved; the ICC fingerprint is gone.
                    for byte in &mut out[cpayload + 4..cend] {
                        *byte = 0;
                    }
                }
            }
        }
        if cend <= p {
            return None;
        }
        p = cend;
    }
    Some(())
}

/// Split a Debug-formatted `ExifTag` string like `ImageDescription("Hello")`
/// into (name, value).
#[cfg(feature = "native")]
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
#[cfg(feature = "native")]
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

#[cfg(all(test, feature = "native"))]
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

    // ---------- strip_jxl_boxes ----------

    /// Build one ISO-BMFF box: 4-byte big-endian size + 4-byte type +
    /// payload.
    fn jxl_box(ty: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let total = (8 + payload.len()) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(&total.to_be_bytes());
        b.extend_from_slice(&ty);
        b.extend_from_slice(payload);
        b
    }

    /// A minimal ISO-BMFF JXL: signature box + ftyp + `xml ` (XMP) box
    /// carrying a recognizable creator token + a `jxlc` codestream box.
    fn jxl_isobmff_with_xmp() -> Vec<u8> {
        const SIG_ISOBMFF: [u8; 12] = [
            0x00, 0x00, 0x00, 0x0C, 0x4A, 0x58, 0x4C, 0x20, 0x0D, 0x0A, 0x87, 0x0A,
        ];
        let mut out = Vec::new();
        out.extend_from_slice(&SIG_ISOBMFF);
        out.extend_from_slice(&jxl_box(*b"ftyp", b"jxl \x00\x00\x00\x00jxl "));
        out.extend_from_slice(&jxl_box(
            *b"xml ",
            b"<?xpacket?><rdf:RDF>creator=Alice</rdf:RDF>",
        ));
        out.extend_from_slice(&jxl_box(*b"jxlc", &[0x01, 0x02, 0x03, 0x04]));
        out
    }

    #[test]
    fn strip_jxl_removes_xml_xmp_box_keeps_codestream() {
        let dirty = jxl_isobmff_with_xmp();
        // Sanity: the dirty input does carry the XMP token.
        assert!(dirty.windows(13).any(|w| w == b"creator=Alice"));

        let cleaned = strip_jxl_boxes(&dirty).expect("valid ISO-BMFF JXL must parse");

        // The XMP packet must be gone, both the box type and its payload.
        assert!(
            !cleaned.windows(13).any(|w| w == b"creator=Alice"),
            "XMP creator token survived JXL strip"
        );
        assert!(
            !cleaned.windows(4).any(|w| w == b"xml "),
            "`xml ` box survived JXL strip"
        );
        // Structural + codestream boxes must survive.
        assert!(cleaned.windows(4).any(|w| w == b"ftyp"));
        assert!(cleaned.windows(4).any(|w| w == b"jxlc"));
    }

    #[test]
    fn strip_jxl_removes_exif_and_jumbf_boxes() {
        const SIG_ISOBMFF: [u8; 12] = [
            0x00, 0x00, 0x00, 0x0C, 0x4A, 0x58, 0x4C, 0x20, 0x0D, 0x0A, 0x87, 0x0A,
        ];
        let mut dirty = Vec::new();
        dirty.extend_from_slice(&SIG_ISOBMFF);
        dirty.extend_from_slice(&jxl_box(*b"Exif", b"\x00\x00\x00\x00MM\x00\x2aSECRET-EXIF"));
        dirty.extend_from_slice(&jxl_box(*b"jumb", b"jumbf-c2pa-SECRET"));
        dirty.extend_from_slice(&jxl_box(*b"jxlc", &[0xDE, 0xAD]));

        let cleaned = strip_jxl_boxes(&dirty).expect("valid JXL must parse");
        assert!(!cleaned.windows(11).any(|w| w == b"SECRET-EXIF"));
        assert!(!cleaned.windows(6).any(|w| w == b"SECRET"));
        assert!(!cleaned.windows(4).any(|w| w == b"Exif"));
        assert!(!cleaned.windows(4).any(|w| w == b"jumb"));
        assert!(cleaned.windows(4).any(|w| w == b"jxlc"));
    }

    #[test]
    fn strip_jxl_removes_brob_wrapped_xmp() {
        const SIG_ISOBMFF: [u8; 12] = [
            0x00, 0x00, 0x00, 0x0C, 0x4A, 0x58, 0x4C, 0x20, 0x0D, 0x0A, 0x87, 0x0A,
        ];
        let mut dirty = Vec::new();
        dirty.extend_from_slice(&SIG_ISOBMFF);
        // brob box whose first 4 payload bytes are the wrapped type `xml `.
        dirty.extend_from_slice(&jxl_box(*b"brob", b"xml COMPRESSED-XMP-PAYLOAD"));
        dirty.extend_from_slice(&jxl_box(*b"jxlc", &[0x00]));

        let cleaned = strip_jxl_boxes(&dirty).expect("valid JXL must parse");
        assert!(
            !cleaned.windows(4).any(|w| w == b"brob"),
            "brob-wrapped XMP survived"
        );
        assert!(!cleaned.windows(8).any(|w| w == b"COMPRESS"));
        assert!(cleaned.windows(4).any(|w| w == b"jxlc"));
    }

    #[test]
    fn strip_jxl_bare_codestream_is_passthrough() {
        // 0xFF 0x0A bare codestream: no container, no metadata. Must come
        // back byte-identical (not a 500/parse error).
        let raw = [0xFFu8, 0x0A, 0x11, 0x22, 0x33, 0x44, 0x55];
        let cleaned = strip_jxl_boxes(&raw).expect("bare codestream must pass through");
        assert_eq!(cleaned, raw);
    }

    #[test]
    fn strip_jxl_rejects_non_jxl() {
        assert!(strip_jxl_boxes(b"not a jxl file at all").is_err());
    }

    // ---------- strip_heif_extra_metadata ----------

    /// Build a synthetic HEIF carrying an Exif item, an XMP item
    /// (`mime`/application/rdf+xml) and a `colr` box of type `prof`
    /// (embedded ICC). The XMP and ICC payloads live in `mdat`; `iloc`
    /// points at their absolute file offsets. Returns the bytes plus the
    /// recognizable tokens we assert on.
    fn heif_with_xmp_and_icc() -> Vec<u8> {
        // ---- mdat payload: [exif][xmp] ----
        // The ICC profile lives ONLY in the colr box (the place this sweep
        // targets); keeping it out of mdat lets the test prove the colr
        // copy was zeroed without a duplicate copy masking the assertion.
        let exif_data = b"EXIF-SECRET".to_vec();
        let xmp_data = b"<?xpacket?>rdf:RDF GPSLatitude=48.0".to_vec();
        // ICC profile: a recognizable signature `acsp` at offset 36 + a
        // device-description token.
        let mut icc_data = vec![0u8; 36];
        icc_data.extend_from_slice(b"acsp");
        icc_data.extend_from_slice(b"ICC-DEVICE-FINGERPRINT");

        // Assemble mdat payload and remember offsets (relative to mdat
        // payload start; the absolute offset is filled in once we know
        // where mdat lands).
        let exif_off_in_mdat = 0usize;
        let xmp_off_in_mdat = exif_data.len();
        let mut mdat_payload = Vec::new();
        mdat_payload.extend_from_slice(&exif_data);
        mdat_payload.extend_from_slice(&xmp_data);

        // ---- build a v2 infe (FullBox) ----
        // version(1)=2, flags(3)=0, item_id(u16), protection(u16),
        // item_type(4), item_name(null), content_type(null).
        fn infe_mime(item_id: u16, content_type: &[u8]) -> Vec<u8> {
            let mut p = Vec::new();
            p.push(2); // version
            p.extend_from_slice(&[0, 0, 0]); // flags
            p.extend_from_slice(&item_id.to_be_bytes());
            p.extend_from_slice(&0u16.to_be_bytes()); // protection
            p.extend_from_slice(b"mime");
            p.push(0); // empty item_name
            p.extend_from_slice(content_type);
            p.push(0); // content_type null terminator
            jxl_box(*b"infe", &p)
        }
        let infe_exif = infe_mime(1, b"image/x-exif"); // not XMP -> kept
        let infe_xmp = infe_mime(2, b"application/rdf+xml"); // XMP -> stripped

        // ---- iinf (FullBox v0): entry_count u16 then infe children ----
        let mut iinf_payload = Vec::new();
        iinf_payload.push(0); // version 0
        iinf_payload.extend_from_slice(&[0, 0, 0]); // flags
        iinf_payload.extend_from_slice(&2u16.to_be_bytes()); // entry_count
        iinf_payload.extend_from_slice(&infe_exif);
        iinf_payload.extend_from_slice(&infe_xmp);
        let iinf = jxl_box(*b"iinf", &iinf_payload);

        // ---- colr box (prof) inside ipco inside iprp ----
        let mut colr_payload = Vec::new();
        colr_payload.extend_from_slice(b"prof");
        colr_payload.extend_from_slice(&icc_data); // duplicate ICC in colr too
        let colr = jxl_box(*b"colr", &colr_payload);
        let ipco = jxl_box(*b"ipco", &colr);
        let iprp = jxl_box(*b"iprp", &ipco);

        // ---- iloc (FullBox v1) ----
        // We need the absolute mdat payload offset, which depends on the
        // sizes of ftyp + meta. Build meta with a placeholder iloc, measure,
        // then patch the extent offsets. Simpler: compute sizes up front.
        //
        // iloc v1 layout per item:
        //   item_id(u16), construction_method(u16, =0 file),
        //   data_reference_index(u16), base_offset(0 bytes, base_offset_size=0),
        //   extent_count(u16), then per extent:
        //     extent_offset(offset_size=4), extent_length(length_size=4).
        // (index_size=0 so no extent_index field.)
        fn iloc_item(item_id: u16, abs_off: u32, len: u32) -> Vec<u8> {
            let mut e = Vec::new();
            e.extend_from_slice(&item_id.to_be_bytes());
            e.extend_from_slice(&0u16.to_be_bytes()); // construction_method=0
            e.extend_from_slice(&0u16.to_be_bytes()); // data_reference_index
            e.extend_from_slice(&1u16.to_be_bytes()); // extent_count
            e.extend_from_slice(&abs_off.to_be_bytes()); // extent_offset (4)
            e.extend_from_slice(&len.to_be_bytes()); // extent_length (4)
            e
        }

        // Compute the absolute position where mdat payload begins. The file
        // layout is: ftyp | meta | mdat. We must know meta's total size.
        // Build meta with the iloc using placeholder offsets first to learn
        // its length, then rebuild with real offsets. Because offset/length
        // field widths are fixed, meta's size is stable across the patch.
        let build_meta = |exif_abs: u32, xmp_abs: u32| -> Vec<u8> {
            let mut iloc_payload = Vec::new();
            iloc_payload.push(1); // version 1
            iloc_payload.extend_from_slice(&[0, 0, 0]); // flags
            iloc_payload.push(0x44u8); // offset_size=4, length_size=4
            iloc_payload.push(0x00u8); // base_offset_size=0, index_size=0
            iloc_payload.extend_from_slice(&2u16.to_be_bytes()); // item_count
            iloc_payload.extend_from_slice(&iloc_item(1, exif_abs, exif_data.len() as u32));
            iloc_payload.extend_from_slice(&iloc_item(2, xmp_abs, xmp_data.len() as u32));
            let iloc = jxl_box(*b"iloc", &iloc_payload);

            // meta is a FullBox: 4 bytes version/flags then children.
            let mut meta_payload = Vec::new();
            meta_payload.extend_from_slice(&[0, 0, 0, 0]); // version/flags
            meta_payload.extend_from_slice(&iinf);
            meta_payload.extend_from_slice(&iloc);
            meta_payload.extend_from_slice(&iprp);
            jxl_box(*b"meta", &meta_payload)
        };

        let ftyp = jxl_box(*b"ftyp", b"heic\x00\x00\x00\x00heic");
        // First pass with zero offsets to size meta.
        let meta_sized = build_meta(0, 0);
        let mdat_payload_abs = (ftyp.len() + meta_sized.len() + 8) as u32; // +8 mdat header
        let exif_abs = mdat_payload_abs + exif_off_in_mdat as u32;
        let xmp_abs = mdat_payload_abs + xmp_off_in_mdat as u32;
        let meta = build_meta(exif_abs, xmp_abs);
        assert_eq!(meta.len(), meta_sized.len(), "meta size must be stable");

        let mdat = jxl_box(*b"mdat", &mdat_payload);

        let mut out = Vec::new();
        out.extend_from_slice(&ftyp);
        out.extend_from_slice(&meta);
        out.extend_from_slice(&mdat);
        out
    }

    /// Regression: a HEIF body whose first box header declares a size far
    /// larger than the file used to reach `little_exif`, which allocates the
    /// declared size on trust (0.6.23: an 8-byte body asking for 3.36 GiB).
    /// In the efrei.app wasm component that request exceeds the 3 GiB linear
    /// memory ceiling and traps the instance: a remote DoS from a 9-byte
    /// request. Found by fuzzing 2026-07-29; the gate is now a pre-parse
    /// bounds walk, so these must be rejected without any large allocation.
    #[test]
    fn heif_oversized_box_header_rejected_before_alloc() {
        // Exact fuzz artifacts (box size 0xd743e620 and 0xffffffff).
        for body in [
            vec![0xd7u8, 0x43, 0xe6, 0x20, 0xd7, 0xd7, 0x12, 0x49],
            vec![0xffu8, 0xff, 0xff, 0xff, 0xff, 0xef, 0xbb, 0x49],
        ] {
            assert!(
                !heif_top_level_boxes_fit(&body),
                "oversized top-level box must fail the fit check"
            );
            let err = clean_bytes(&body, "heic");
            assert!(err.is_err(), "malformed HEIF must be rejected, not parsed");
        }
        // A well-formed file still passes the gate (guards against the check
        // being too strict and rejecting real images).
        assert!(heif_top_level_boxes_fit(&heif_with_xmp_and_icc()));
    }

    #[test]
    fn strip_heif_removes_xmp_item_and_icc_keeps_exif() {
        let dirty = heif_with_xmp_and_icc();
        // Sanity: the identifying tokens are present pre-strip.
        assert!(dirty.windows(11).any(|w| w == b"GPSLatitude"));
        assert!(dirty.windows(22).any(|w| w == b"ICC-DEVICE-FINGERPRINT"));

        let cleaned = strip_heif_extra_metadata(&dirty).expect("synthetic HEIF must parse");
        assert_eq!(cleaned.len(), dirty.len(), "zeroing must not resize buffer");

        // XMP packet bytes must be gone (zeroed in mdat).
        assert!(
            !cleaned.windows(11).any(|w| w == b"GPSLatitude"),
            "XMP GPS token survived HEIF strip"
        );
        assert!(!cleaned.windows(9).any(|w| w == b"<?xpacket"));
        // ICC fingerprint must be gone (zeroed in the colr box; the mdat
        // copy isn't iloc-referenced so the colr copy is the one we test).
        assert!(
            !cleaned.windows(22).any(|w| w == b"ICC-DEVICE-FINGERPRINT"),
            "ICC device fingerprint survived HEIF strip"
        );
        // The kept Exif item's data must survive (we only strip XMP + ICC).
        assert!(
            cleaned.windows(11).any(|w| w == b"EXIF-SECRET"),
            "Exif item data must be preserved by this sweep (handled by little_exif clear in production)"
        );
        // Structural boxes survive.
        assert!(cleaned.windows(4).any(|w| w == b"meta"));
        assert!(cleaned.windows(4).any(|w| w == b"iinf"));
    }

    #[test]
    fn strip_heif_no_meta_is_passthrough() {
        // A file with no `meta` box: nothing item-based to strip. Returns
        // the buffer unchanged (Some), never an error.
        let mut buf = Vec::new();
        buf.extend_from_slice(&jxl_box(*b"ftyp", b"heic\x00\x00\x00\x00heic"));
        buf.extend_from_slice(&jxl_box(*b"mdat", b"pixels"));
        let cleaned = strip_heif_extra_metadata(&buf).expect("no-meta HEIF must pass through");
        assert_eq!(cleaned, buf);
    }
}
