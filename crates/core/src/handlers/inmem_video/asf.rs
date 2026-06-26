//! In-memory ASF / WMV (Windows Media) metadata stripper.
//!
//! ASF is a tree of *objects*, each laid out on disk as:
//!
//! ```text
//! [16-byte GUID][u64 LE object size, incl. this 24-byte header][payload]
//! ```
//!
//! The first top-level object is the **Header Object** whose payload
//! begins with `[u32 LE number-of-header-objects][u8 reserved1][u8
//! reserved2]` followed by the child header objects. The second
//! top-level object is the **Data Object** carrying the actual media
//! packets, and after it come optional index objects.
//!
//! This stripper mirrors the native ffmpeg path
//! (`-map_metadata -1 -map_chapters -1 -disposition 0`): it performs a
//! pure metadata strip + remux. It keeps every media byte (the Data
//! Object and all stream/index objects) verbatim and only removes the
//! tag-bearing child objects inside the Header Object:
//!
//! * Content Description Object (title/author/copyright/description/rating)
//! * Extended Content Description Object (the `WM/*` tags, incl. GPS / encoder)
//! * Metadata Object
//! * Metadata Library Object
//!
//! The Content Description and Extended Content Description objects are
//! direct children of the Header Object, but the Metadata Object and
//! Metadata Library Object are NOT: they live inside the **Header
//! Extension Object** (one direct child whose payload nests further ASF
//! objects). So the stripper recurses into the Header Extension Object,
//! drops those two nested objects in place, and fixes the Header Extension
//! Data Size (u32) + Header Extension Object size (u64) accordingly. The
//! Header Object's child count is unchanged in that case (the Header
//! Extension Object survives, just smaller); everything else there
//! (encryption, stream properties, extended stream properties, language
//! list, advanced mutual exclusion) is kept verbatim.
//!
//! After removing those children it fixes up the on-disk size accounting:
//! the Header Object's `size` (u64) and its `number-of-header-objects`
//! (u32) count are both decremented, and the File Properties Object's
//! `File Size` field (u64 at payload offset 16) is reduced by the number
//! of bytes removed so the container stays self-consistent.
//!
//! Every length / offset read from the (attacker-controlled) input is
//! bounds-checked and uses checked arithmetic; a malformed file yields a
//! [`CoreError::ParseError`] rather than a panic or an allocation blow-up.

use crate::error::CoreError;
use std::path::PathBuf;

/// ASF GUIDs as they appear on disk (Data1 u32 LE, Data2/Data3 u16 LE,
/// Data4 the trailing 8 bytes verbatim).
const HEADER_OBJECT: [u8; 16] = [
    0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C,
];
const FILE_PROPERTIES_OBJECT: [u8; 16] = [
    0xA1, 0xDC, 0xAB, 0x8C, 0x47, 0xA9, 0xCF, 0x11, 0x8E, 0xE4, 0x00, 0xC0, 0x0C, 0x20, 0x53, 0x65,
];
const CONTENT_DESCRIPTION_OBJECT: [u8; 16] = [
    0x33, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C,
];
const EXTENDED_CONTENT_DESCRIPTION_OBJECT: [u8; 16] = [
    0x40, 0xA4, 0xD0, 0xD2, 0x07, 0xE3, 0xD2, 0x11, 0x97, 0xF0, 0x00, 0xA0, 0xC9, 0x5E, 0xA8, 0x50,
];
const METADATA_OBJECT: [u8; 16] = [
    0xEA, 0xCB, 0xF8, 0xC5, 0xAF, 0x5B, 0x77, 0x48, 0x84, 0x67, 0xAA, 0x8C, 0x44, 0xFA, 0x4C, 0xCA,
];
const METADATA_LIBRARY_OBJECT: [u8; 16] = [
    0x94, 0x1C, 0x23, 0x44, 0x98, 0x94, 0xD0, 0x49, 0xA1, 0x41, 0x1D, 0x13, 0x4E, 0x45, 0x70, 0x54,
];
/// Header Extension Object (`5FBF03B5-A92E-11CF-8EE3-00C00C205365`). A
/// single direct child of the Header Object whose payload nests further
/// ASF objects (incl. the privacy-sensitive Metadata / Metadata Library
/// objects), so the stripper must recurse into it.
const HEADER_EXTENSION_OBJECT: [u8; 16] = [
    0xB5, 0x03, 0xBF, 0x5F, 0x2E, 0xA9, 0xCF, 0x11, 0x8E, 0xE3, 0x00, 0xC0, 0x0C, 0x20, 0x53, 0x65,
];

/// Every ASF object header is a 16-byte GUID + an 8-byte size.
const OBJ_HEADER_LEN: u64 = 24;
/// The Header Object payload prefix: u32 count + 2 reserved bytes.
const HEADER_PAYLOAD_PREFIX: usize = 6;
/// Offset of the `File Size` u64 inside the File Properties Object payload.
const FILE_PROPS_FILE_SIZE_OFFSET: usize = 16;
/// Header Extension Object payload prefix before the nested-object data:
/// `[Reserved Field 1: 16-byte GUID][Reserved Field 2: u16][Header
/// Extension Data Size: u32]` = 22 bytes.
const HEADER_EXT_PAYLOAD_PREFIX: usize = 22;
/// Byte offset of the `Header Extension Data Size` (u32 LE) inside the
/// Header Extension Object payload (after the 16-byte reserved GUID + the
/// 2-byte reserved field).
const HEADER_EXT_DATA_SIZE_OFFSET: usize = 18;

/// One parsed child object inside the Header Object payload.
enum Child {
    /// Copy this child verbatim from `input[start..start+size]`.
    Keep { start: usize, size: usize },
    /// Drop this child entirely (it carries metadata). Removed bytes are
    /// accounted for by rebuilding the output from the kept children, so
    /// the size is not retained here.
    Drop,
    /// A Header Extension Object that was rewritten in place: the parent's
    /// child count is unchanged but its byte length shrank.
    Rewrite { bytes: Vec<u8> },
}

fn parse_err(detail: impl Into<String>) -> CoreError {
    CoreError::ParseError {
        path: PathBuf::new(),
        detail: detail.into(),
    }
}

/// Read a little-endian u64 starting at `off` from `buf`, bounds-checked.
fn read_u64_le(buf: &[u8], off: usize) -> Result<u64, CoreError> {
    let end = off
        .checked_add(8)
        .ok_or_else(|| parse_err("u64 offset overflow"))?;
    let slice = buf
        .get(off..end)
        .ok_or_else(|| parse_err("truncated u64 field"))?;
    let arr: [u8; 8] = slice.try_into().map_err(|_| parse_err("u64 slice size"))?;
    Ok(u64::from_le_bytes(arr))
}

/// Read a little-endian u32 starting at `off` from `buf`, bounds-checked.
fn read_u32_le(buf: &[u8], off: usize) -> Result<u32, CoreError> {
    let end = off
        .checked_add(4)
        .ok_or_else(|| parse_err("u32 offset overflow"))?;
    let slice = buf
        .get(off..end)
        .ok_or_else(|| parse_err("truncated u32 field"))?;
    let arr: [u8; 4] = slice.try_into().map_err(|_| parse_err("u32 slice size"))?;
    Ok(u32::from_le_bytes(arr))
}

/// True if the 16 bytes at `off` equal `guid`.
fn guid_at(buf: &[u8], off: usize, guid: &[u8; 16]) -> bool {
    let Some(end) = off.checked_add(16) else {
        return false;
    };
    matches!(buf.get(off..end), Some(g) if g == guid)
}

/// Convert a checked object size (`u64`) to a `usize` index without ever
/// panicking on a 32-bit (wasm) target.
fn as_usize(v: u64, what: &str) -> Result<usize, CoreError> {
    usize::try_from(v).map_err(|_| parse_err(format!("{what} exceeds addressable range")))
}

/// Rewrite a Header Extension Object, dropping any nested Metadata Object
/// and Metadata Library Object from its Header Extension Data and fixing
/// the two affected size fields (Header Extension Data Size u32, and the
/// Header Extension Object size u64).
///
/// `obj` is the full Header Extension Object slice (GUID + u64 size +
/// payload) copied out of the input. Returns `(new_object_bytes,
/// removed_bytes)` where `removed_bytes` is how many bytes shrank (0 if
/// nothing nested needed dropping). All offsets/lengths are
/// bounds-checked; malformed nesting yields [`CoreError::ParseError`].
fn rewrite_header_extension(obj: &[u8]) -> Result<(Vec<u8>, u64), CoreError> {
    let obj_header_len = as_usize(OBJ_HEADER_LEN, "object header length")?;
    // Sanity: we must at least have the object header + the extension
    // payload prefix.
    let data_start = obj_header_len
        .checked_add(HEADER_EXT_PAYLOAD_PREFIX)
        .ok_or_else(|| parse_err("header extension prefix overflow"))?;
    if data_start > obj.len() {
        return Err(parse_err(
            "Header Extension Object too small for its prefix",
        ));
    }
    // Header Extension Data Size lives at payload offset 18, i.e. object
    // offset 24 + 18 = 42.
    let data_size_off = obj_header_len
        .checked_add(HEADER_EXT_DATA_SIZE_OFFSET)
        .ok_or_else(|| parse_err("header extension data-size offset overflow"))?;
    let data_size = as_usize(
        u64::from(read_u32_le(obj, data_size_off)?),
        "Header Extension Data Size",
    )?;
    let data_end = data_start
        .checked_add(data_size)
        .ok_or_else(|| parse_err("header extension data end overflow"))?;
    if data_end > obj.len() {
        return Err(parse_err(
            "Header Extension Data Size exceeds the object payload",
        ));
    }

    // Walk the nested objects inside the Header Extension Data, collecting
    // the byte ranges to keep (everything except the two metadata GUIDs).
    let mut kept: Vec<(usize, usize)> = Vec::new();
    let mut removed: u64 = 0;
    let mut cursor = data_start;
    while cursor < data_end {
        let hdr_end = cursor
            .checked_add(obj_header_len)
            .ok_or_else(|| parse_err("nested header offset overflow"))?;
        if hdr_end > data_end {
            return Err(parse_err("nested object header runs past extension data"));
        }
        let size_off = cursor
            .checked_add(16)
            .ok_or_else(|| parse_err("nested size offset overflow"))?;
        let nested_size_u64 = read_u64_le(obj, size_off)?;
        if nested_size_u64 < OBJ_HEADER_LEN {
            return Err(parse_err("nested object size smaller than its header"));
        }
        let nested_size = as_usize(nested_size_u64, "nested object size")?;
        let nested_end = cursor
            .checked_add(nested_size)
            .ok_or_else(|| parse_err("nested object end overflow"))?;
        if nested_end > data_end {
            return Err(parse_err("nested object extends past extension data"));
        }

        let drop = guid_at(obj, cursor, &METADATA_OBJECT)
            || guid_at(obj, cursor, &METADATA_LIBRARY_OBJECT);
        if drop {
            removed = removed
                .checked_add(nested_size_u64)
                .ok_or_else(|| parse_err("nested removed-bytes overflow"))?;
        } else {
            kept.push((cursor, nested_size));
        }
        cursor = nested_end;
    }
    if cursor != data_end {
        return Err(parse_err(
            "nested objects do not exactly fill the Header Extension Data",
        ));
    }

    // Nothing nested to drop: hand back the object verbatim.
    if removed == 0 {
        return Ok((obj.to_vec(), 0));
    }

    // New sizes.
    let old_obj_size = read_u64_le(obj, 16)?;
    let new_obj_size = old_obj_size
        .checked_sub(removed)
        .ok_or_else(|| parse_err("Header Extension Object size underflow"))?;
    let new_data_size = (data_size as u64)
        .checked_sub(removed)
        .ok_or_else(|| parse_err("Header Extension Data Size underflow"))?;
    let new_data_size_u32 = u32::try_from(new_data_size)
        .map_err(|_| parse_err("Header Extension Data Size overflow"))?;

    // Rebuild: prefix (object header + reserved fields) with a patched
    // data-size field, then the kept nested objects.
    let out_len = (obj.len() as u64)
        .checked_sub(removed)
        .ok_or_else(|| parse_err("header extension output underflow"))?;
    let mut out: Vec<u8> = Vec::with_capacity(as_usize(out_len, "header extension output")?);

    // GUID (16) verbatim.
    let guid = obj
        .get(0..16)
        .ok_or_else(|| parse_err("missing header extension GUID"))?;
    out.extend_from_slice(guid);
    // Patched object size (u64).
    out.extend_from_slice(&new_obj_size.to_le_bytes());
    // Reserved Field 1 (16-byte GUID) + Reserved Field 2 (u16), verbatim:
    // object offset 24..42 (i.e. payload offset 0..18).
    let reserved = obj
        .get(obj_header_len..data_size_off)
        .ok_or_else(|| parse_err("missing header extension reserved fields"))?;
    out.extend_from_slice(reserved);
    // Patched Header Extension Data Size (u32).
    out.extend_from_slice(&new_data_size_u32.to_le_bytes());
    // Kept nested objects.
    for (start, size) in &kept {
        let end = start
            .checked_add(*size)
            .ok_or_else(|| parse_err("kept nested end overflow"))?;
        let bytes = obj
            .get(*start..end)
            .ok_or_else(|| parse_err("kept nested slice out of range"))?;
        out.extend_from_slice(bytes);
    }

    // Internal consistency: the rebuilt object must be exactly new_obj_size.
    if out.len() as u64 != new_obj_size {
        return Err(CoreError::CleanError {
            path: PathBuf::new(),
            detail: "rebuilt Header Extension Object size mismatch".into(),
        });
    }
    Ok((out, removed))
}

/// Strip all container/global/track metadata from an ASF/WMV byte stream.
///
/// Keeps every media stream byte-for-byte; drops the Content Description,
/// Extended Content Description, Metadata and Metadata Library objects;
/// fixes the Header Object size + child count and the File Properties
/// `File Size` field.
///
/// # Errors
///
/// Returns [`CoreError::ParseError`] for a truncated / malformed file
/// (bad GUID, inconsistent object sizes, arithmetic overflow) and
/// [`CoreError::CleanError`] for an internal inconsistency while
/// rebuilding the output.
pub(crate) fn strip(input: &[u8]) -> Result<Vec<u8>, CoreError> {
    // The first top-level object must be the Header Object.
    if !guid_at(input, 0, &HEADER_OBJECT) {
        return Err(parse_err("not an ASF file (missing Header Object GUID)"));
    }
    let header_size_u64 = read_u64_le(input, 16)?;
    if header_size_u64 < OBJ_HEADER_LEN {
        return Err(parse_err("Header Object size smaller than its own header"));
    }
    let header_size = as_usize(header_size_u64, "Header Object size")?;
    let header_end = header_size; // header object starts at offset 0
    if header_end > input.len() {
        return Err(parse_err("Header Object size exceeds file length"));
    }

    // Header Object payload: [u32 count][u8 reserved1][u8 reserved2] then
    // the child objects. The payload begins right after the 24-byte
    // object header.
    let payload_start = as_usize(OBJ_HEADER_LEN, "object header length")?;
    let children_start = payload_start
        .checked_add(HEADER_PAYLOAD_PREFIX)
        .ok_or_else(|| parse_err("header payload prefix overflow"))?;
    if children_start > header_end {
        return Err(parse_err("Header Object too small for its payload prefix"));
    }
    let original_count = read_u32_le(input, payload_start)?;

    // Walk the child objects inside the Header Object.
    let mut children: Vec<Child> = Vec::new();
    // Bytes removed by dropping whole direct children.
    let mut removed_bytes: u64 = 0;
    // Direct children dropped (the only thing that changes the count).
    let mut removed_count: u32 = 0;
    // Bytes removed from *inside* a kept child (the Header Extension
    // Object). These shrink the Header Object + File Size but do NOT
    // change the child count.
    let mut inner_removed_bytes: u64 = 0;
    let mut cursor = children_start;
    while cursor < header_end {
        // Need at least a full object header (GUID + size).
        let hdr_end = cursor
            .checked_add(as_usize(OBJ_HEADER_LEN, "object header length")?)
            .ok_or_else(|| parse_err("child header offset overflow"))?;
        if hdr_end > header_end {
            return Err(parse_err("child object header runs past Header Object"));
        }
        let guid_off = cursor;
        let size_off = guid_off
            .checked_add(16)
            .ok_or_else(|| parse_err("child size offset overflow"))?;
        let child_size_u64 = read_u64_le(input, size_off)?;
        if child_size_u64 < OBJ_HEADER_LEN {
            return Err(parse_err("child object size smaller than its header"));
        }
        let child_size = as_usize(child_size_u64, "child object size")?;
        let child_end = cursor
            .checked_add(child_size)
            .ok_or_else(|| parse_err("child object extends past Header Object"))?;
        if child_end > header_end {
            return Err(parse_err("child object extends past Header Object"));
        }

        if guid_at(input, guid_off, &CONTENT_DESCRIPTION_OBJECT)
            || guid_at(input, guid_off, &EXTENDED_CONTENT_DESCRIPTION_OBJECT)
            || guid_at(input, guid_off, &METADATA_OBJECT)
            || guid_at(input, guid_off, &METADATA_LIBRARY_OBJECT)
        {
            // A tag-bearing direct child: drop it whole.
            removed_bytes = removed_bytes
                .checked_add(child_size_u64)
                .ok_or_else(|| parse_err("removed-bytes overflow"))?;
            removed_count = removed_count
                .checked_add(1)
                .ok_or_else(|| parse_err("removed-count overflow"))?;
            children.push(Child::Drop);
        } else if guid_at(input, guid_off, &HEADER_EXTENSION_OBJECT) {
            // The Metadata / Metadata Library objects live *inside* here.
            // Recurse: rebuild this child without them. The child stays
            // (count unchanged) but may shrink.
            let obj = input
                .get(cursor..child_end)
                .ok_or_else(|| parse_err("Header Extension slice out of range"))?;
            let (rebuilt, inner) = rewrite_header_extension(obj)?;
            inner_removed_bytes = inner_removed_bytes
                .checked_add(inner)
                .ok_or_else(|| parse_err("inner removed-bytes overflow"))?;
            if inner == 0 {
                // Unchanged: copy verbatim.
                children.push(Child::Keep {
                    start: cursor,
                    size: child_size,
                });
            } else {
                children.push(Child::Rewrite { bytes: rebuilt });
            }
        } else {
            children.push(Child::Keep {
                start: cursor,
                size: child_size,
            });
        }
        cursor = child_end;
    }
    if cursor != header_end {
        return Err(parse_err(
            "child objects do not exactly fill the Header Object",
        ));
    }

    // Total bytes removed from the Header Object (whole dropped children +
    // bytes carved out of the Header Extension Object).
    let removed_bytes = removed_bytes
        .checked_add(inner_removed_bytes)
        .ok_or_else(|| parse_err("total removed-bytes overflow"))?;

    // Nothing to strip: return the input unchanged (still a valid file).
    if removed_bytes == 0 {
        return Ok(input.to_vec());
    }
    if removed_count > original_count {
        return Err(parse_err(
            "more metadata objects than the header count claims",
        ));
    }

    // New Header Object size + child count.
    let new_header_size = header_size_u64
        .checked_sub(removed_bytes)
        .ok_or_else(|| parse_err("Header Object size underflow"))?;
    let new_count = original_count
        .checked_sub(removed_count)
        .ok_or_else(|| parse_err("header count underflow"))?;

    // Build the output. Capacity = input length minus what we drop.
    let out_cap = (input.len() as u64)
        .checked_sub(removed_bytes)
        .ok_or_else(|| parse_err("output length underflow"))?;
    let mut out: Vec<u8> = Vec::with_capacity(as_usize(out_cap, "output length")?);

    // 1. Header Object GUID (16 bytes) verbatim.
    let header_guid = input
        .get(0..16)
        .ok_or_else(|| parse_err("missing header GUID bytes"))?;
    out.extend_from_slice(header_guid);
    // 2. Patched Header Object size.
    out.extend_from_slice(&new_header_size.to_le_bytes());
    // 3. Patched child count.
    out.extend_from_slice(&new_count.to_le_bytes());
    // 4. The two reserved bytes verbatim.
    let reserved_off = payload_start
        .checked_add(4)
        .ok_or_else(|| parse_err("reserved offset overflow"))?;
    let reserved = input
        .get(reserved_off..children_start)
        .ok_or_else(|| parse_err("missing reserved bytes"))?;
    out.extend_from_slice(reserved);

    // 5. The kept child objects (verbatim or rewritten). Patch the File
    //    Properties Object's File Size field as we copy it.
    for c in &children {
        let mut bytes = match c {
            Child::Drop => continue,
            Child::Rewrite { bytes } => bytes.clone(),
            Child::Keep { start, size } => {
                let end = start
                    .checked_add(*size)
                    .ok_or_else(|| parse_err("kept child end overflow"))?;
                input
                    .get(*start..end)
                    .ok_or_else(|| parse_err("kept child slice out of range"))?
                    .to_vec()
            }
        };

        if guid_at(&bytes, 0, &FILE_PROPERTIES_OBJECT) {
            // File Size is a u64 at payload offset 16, i.e. object offset
            // 24 + 16 = 40. Read the local copy, subtract removed bytes,
            // write it back.
            let fs_off = as_usize(OBJ_HEADER_LEN, "object header length")?
                .checked_add(FILE_PROPS_FILE_SIZE_OFFSET)
                .ok_or_else(|| parse_err("File Size offset overflow"))?;
            let old_fs = read_u64_le(&bytes, fs_off)?;
            let new_fs = old_fs
                .checked_sub(removed_bytes)
                .ok_or_else(|| parse_err("File Properties File Size underflow"))?;
            let fs_end = fs_off
                .checked_add(8)
                .ok_or_else(|| parse_err("File Size end overflow"))?;
            let dst = bytes
                .get_mut(fs_off..fs_end)
                .ok_or_else(|| CoreError::CleanError {
                    path: PathBuf::new(),
                    detail: "File Size field vanished while patching".into(),
                })?;
            dst.copy_from_slice(&new_fs.to_le_bytes());
        }
        out.extend_from_slice(&bytes);
    }

    // 6. Everything after the Header Object (Data Object + index objects)
    //    verbatim.
    let tail = input
        .get(header_end..)
        .ok_or_else(|| parse_err("Header Object end past file"))?;
    out.extend_from_slice(tail);

    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Append a complete ASF object (GUID + u64 size + payload) to `buf`.
    fn push_object(buf: &mut Vec<u8>, guid: &[u8; 16], payload: &[u8]) {
        let size = (16u64 + 8 + payload.len() as u64).to_le_bytes();
        buf.extend_from_slice(guid);
        buf.extend_from_slice(&size);
        buf.extend_from_slice(payload);
    }

    /// Read a u64 LE at `off` (test helper, panics on bad fixture which is fine).
    fn u64_at(buf: &[u8], off: usize) -> u64 {
        u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
    }
    fn u32_at(buf: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
    }

    /// Build a minimal but structurally valid ASF file:
    /// Header Object { File Properties, Content Description("author=secret"),
    /// Stream-Properties-shaped filler } + Data Object.
    fn build_asf() -> Vec<u8> {
        // --- children of the Header Object ---
        let mut children = Vec::new();

        // File Properties Object. Payload: 16-byte File ID, then File Size
        // (u64) at offset 16, then a few more bytes of (ignored) fields.
        // We set File Size to the eventual whole-file size placeholder; the
        // exact value does not matter for the test except that strip()
        // decrements it by removed_bytes.
        let mut fp_payload = Vec::new();
        fp_payload.extend_from_slice(&[0xAB; 16]); // File ID GUID (dummy)
        let placeholder_file_size: u64 = 4096;
        fp_payload.extend_from_slice(&placeholder_file_size.to_le_bytes()); // File Size @16
        fp_payload.extend_from_slice(&[0u8; 24]); // remaining FP fields (dummy)
        push_object(&mut children, &FILE_PROPERTIES_OBJECT, &fp_payload);

        // Content Description Object carrying the secret author string.
        // Real layout is 5 u16 length fields + 5 strings; for the test we
        // just embed a recognisable secret blob in the payload. strip()
        // treats the whole object as opaque and removes it by GUID.
        let cdo_payload = b"\x00\x00\x00\x00\x00author=secret\x00".to_vec();
        push_object(&mut children, &CONTENT_DESCRIPTION_OBJECT, &cdo_payload);

        // A stream-properties-shaped object we must KEEP verbatim.
        const STREAM_PROPS: [u8; 16] = [
            0x91, 0x07, 0xDC, 0xB7, 0xB7, 0xA9, 0xCF, 0x11, 0x8E, 0xE6, 0x00, 0xC0, 0x0C, 0x20,
            0x53, 0x65,
        ];
        let stream_payload = b"STREAMDATA-keep-me".to_vec();
        push_object(&mut children, &STREAM_PROPS, &stream_payload);

        let child_count: u32 = 3;

        // --- Header Object ---
        // payload = [u32 count][u8 res1][u8 res2][children...]
        let mut header_payload = Vec::new();
        header_payload.extend_from_slice(&child_count.to_le_bytes());
        header_payload.push(0x01); // reserved1
        header_payload.push(0x02); // reserved2
        header_payload.extend_from_slice(&children);

        let mut file = Vec::new();
        push_object(&mut file, &HEADER_OBJECT, &header_payload);

        // --- Data Object (media payload), kept verbatim ---
        let data_payload = b"\xDE\xAD\xBE\xEF MEDIA PACKETS \x00\x01\x02".to_vec();
        const DATA_OBJECT: [u8; 16] = [
            0x36, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62,
            0xCE, 0x6C,
        ];
        push_object(&mut file, &DATA_OBJECT, &data_payload);

        file
    }

    /// Re-parse an ASF file: return (header_size, header_count, list of
    /// top-level objects after the header, and the header children GUIDs).
    fn reparse(buf: &[u8]) -> (u64, u32, Vec<[u8; 16]>) {
        assert!(
            guid_at(buf, 0, &HEADER_OBJECT),
            "must start with header obj"
        );
        let header_size = u64_at(buf, 16);
        let count = u32_at(buf, 24);
        let mut cursor = 24 + 4 + 2; // after count + reserved
        let header_end = header_size as usize;
        let mut guids = Vec::new();
        while cursor < header_end {
            let mut g = [0u8; 16];
            g.copy_from_slice(&buf[cursor..cursor + 16]);
            let sz = u64_at(buf, cursor + 16) as usize;
            guids.push(g);
            cursor += sz;
        }
        assert_eq!(cursor, header_end, "children must fill header exactly");
        (header_size, count, guids)
    }

    #[test]
    fn strips_content_description_and_fixes_sizes() {
        let dirty = build_asf();

        // Sanity: the secret is present, the CDO GUID is present.
        assert!(
            dirty
                .windows(b"author=secret".len())
                .any(|w| w == b"author=secret"),
            "fixture must contain the secret"
        );
        let (orig_hsize, orig_count, orig_guids) = reparse(&dirty);
        assert_eq!(orig_count, 3);
        assert!(orig_guids.contains(&CONTENT_DESCRIPTION_OBJECT));

        // Compute the size of the CDO object as built, for size-math checks.
        let cdo_obj_size = orig_guids
            .iter()
            .position(|g| g == &CONTENT_DESCRIPTION_OBJECT)
            .map(|_| {
                // find its on-disk size by scanning
                let mut cursor = 24 + 4 + 2usize;
                loop {
                    let mut g = [0u8; 16];
                    g.copy_from_slice(&dirty[cursor..cursor + 16]);
                    let sz = u64_at(&dirty, cursor + 16);
                    if g == CONTENT_DESCRIPTION_OBJECT {
                        break sz;
                    }
                    cursor += sz as usize;
                }
            })
            .expect("cdo present");

        let cleaned = strip(&dirty).expect("strip must succeed");

        // (a) The secret metadata bytes are gone.
        assert!(
            !cleaned
                .windows(b"author=secret".len())
                .any(|w| w == b"author=secret"),
            "cleaned output must not contain the secret author string"
        );

        // (d) structural integrity: still parses, header shrank, count dropped by 1.
        let (new_hsize, new_count, new_guids) = reparse(&cleaned);
        assert_eq!(new_count, orig_count - 1, "child count must drop by one");
        assert!(
            !new_guids.contains(&CONTENT_DESCRIPTION_OBJECT),
            "Content Description Object must be gone"
        );
        // The kept children (File Properties + Stream Properties) survive.
        assert!(new_guids.contains(&FILE_PROPERTIES_OBJECT));
        assert_eq!(new_guids.len(), 2, "two header children must remain");

        // header size decremented by exactly the CDO object size.
        assert_eq!(
            new_hsize,
            orig_hsize - cdo_obj_size,
            "Header Object size must shrink by the removed object's size"
        );

        // File Properties File Size field decremented by the same amount.
        // Locate the File Properties object in the cleaned output.
        let mut cursor = 24 + 4 + 2usize;
        let header_end = new_hsize as usize;
        let mut fp_file_size = None;
        while cursor < header_end {
            if guid_at(&cleaned, cursor, &FILE_PROPERTIES_OBJECT) {
                fp_file_size = Some(u64_at(&cleaned, cursor + 24 + 16));
                break;
            }
            let sz = u64_at(&cleaned, cursor + 16) as usize;
            cursor += sz;
        }
        assert_eq!(
            fp_file_size,
            Some(4096 - cdo_obj_size),
            "File Properties File Size must drop by the removed bytes"
        );

        // Data Object (media payload) intact and byte-identical.
        assert!(
            cleaned
                .windows(b"\xDE\xAD\xBE\xEF MEDIA PACKETS \x00\x01\x02".len())
                .any(|w| w == b"\xDE\xAD\xBE\xEF MEDIA PACKETS \x00\x01\x02"),
            "Data Object media payload must be preserved verbatim"
        );

        // Kept stream payload intact.
        assert!(
            cleaned
                .windows(b"STREAMDATA-keep-me".len())
                .any(|w| w == b"STREAMDATA-keep-me"),
            "Stream Properties payload must be preserved"
        );

        // Whole-file length shrank by exactly the removed object.
        assert_eq!(cleaned.len() as u64, dirty.len() as u64 - cdo_obj_size);
    }

    /// Build a Header Extension Object whose Header Extension Data nests
    /// two objects: a Stream-Properties-shaped object (must survive) and a
    /// Metadata Library Object carrying the secret (`WM/EncodingSettings`).
    /// Returns the full object bytes.
    fn build_header_extension() -> Vec<u8> {
        // --- nested objects living inside the Header Extension Data ---
        let mut nested = Vec::new();

        // A nested stream-properties-shaped object we must KEEP verbatim.
        const STREAM_PROPS: [u8; 16] = [
            0x91, 0x07, 0xDC, 0xB7, 0xB7, 0xA9, 0xCF, 0x11, 0x8E, 0xE6, 0x00, 0xC0, 0x0C, 0x20,
            0x53, 0x65,
        ];
        push_object(&mut nested, &STREAM_PROPS, b"NESTED-STREAM-keep-me");

        // The Metadata Library Object carrying the secret. strip() drops it
        // by GUID; we only need a recognisable blob in the payload.
        let mlo_payload = b"\x00\x00WM/EncodingSettings=secret\x00".to_vec();
        push_object(&mut nested, &METADATA_LIBRARY_OBJECT, &mlo_payload);

        // --- Header Extension Object payload ---
        // [Reserved Field 1: 16-byte GUID][Reserved Field 2: u16]
        // [Header Extension Data Size: u32][Header Extension Data]
        let mut ext_payload = Vec::new();
        ext_payload.extend_from_slice(&[0xCD; 16]); // Reserved Field 1 (Clock GUID)
        ext_payload.extend_from_slice(&6u16.to_le_bytes()); // Reserved Field 2 (=6)
        ext_payload.extend_from_slice(&(nested.len() as u32).to_le_bytes()); // data size
        ext_payload.extend_from_slice(&nested);

        let mut obj = Vec::new();
        push_object(&mut obj, &HEADER_EXTENSION_OBJECT, &ext_payload);
        obj
    }

    /// Locate a top-level header child by GUID in `buf` and return its
    /// on-disk size (panics on a bad fixture, which is fine in tests).
    fn header_child_size(buf: &[u8], guid: &[u8; 16]) -> u64 {
        let header_end = u64_at(buf, 16) as usize;
        let mut cursor = 24 + 4 + 2usize;
        while cursor < header_end {
            let sz = u64_at(buf, cursor + 16);
            if guid_at(buf, cursor, guid) {
                return sz;
            }
            cursor += sz as usize;
        }
        panic!("child GUID not found");
    }

    /// Regression test for the silent metadata leak: the Metadata Library
    /// Object lives INSIDE the Header Extension Object, not as a direct
    /// child of the Header Object, so the old direct-children-only walker
    /// let `WM/EncodingSettings=secret` survive. This builds exactly that
    /// nesting and asserts the secret is gone, the nested Stream Properties
    /// object survives, all three size fields are corrected, and the Data
    /// Object is byte-identical.
    #[test]
    fn strips_metadata_library_inside_header_extension() {
        // --- children of the Header Object ---
        let mut children = Vec::new();

        // File Properties Object (kept, File Size patched).
        let mut fp_payload = Vec::new();
        fp_payload.extend_from_slice(&[0xAB; 16]); // File ID GUID (dummy)
        let placeholder_file_size: u64 = 8192;
        fp_payload.extend_from_slice(&placeholder_file_size.to_le_bytes()); // File Size @16
        fp_payload.extend_from_slice(&[0u8; 24]); // remaining FP fields (dummy)
        push_object(&mut children, &FILE_PROPERTIES_OBJECT, &fp_payload);

        // Header Extension Object containing the secret Metadata Library
        // Object + a nested Stream Properties object.
        let ext_obj = build_header_extension();
        children.extend_from_slice(&ext_obj);

        let child_count: u32 = 2;
        let mut header_payload = Vec::new();
        header_payload.extend_from_slice(&child_count.to_le_bytes());
        header_payload.push(0x01);
        header_payload.push(0x02);
        header_payload.extend_from_slice(&children);

        let mut dirty = Vec::new();
        push_object(&mut dirty, &HEADER_OBJECT, &header_payload);

        // Data Object (media payload), kept verbatim.
        const DATA_OBJECT: [u8; 16] = [
            0x36, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62,
            0xCE, 0x6C,
        ];
        let data_obj_start = dirty.len();
        push_object(&mut dirty, &DATA_OBJECT, b"\xDE\xAD\xBE\xEF MEDIA \x00\x01");
        let data_obj_bytes = dirty[data_obj_start..].to_vec();

        // Sanity: the secret IS present before stripping.
        assert!(
            dirty
                .windows(b"WM/EncodingSettings=secret".len())
                .any(|w| w == b"WM/EncodingSettings=secret"),
            "fixture must contain the secret"
        );

        // Size accounting expectations.
        let orig_hsize = u64_at(&dirty, 16);
        let orig_ext_size = header_child_size(&dirty, &HEADER_EXTENSION_OBJECT);
        let orig_ext_data_size = {
            // find the extension object, read its data-size u32 at +42.
            let header_end = orig_hsize as usize;
            let mut cursor = 24 + 4 + 2usize;
            let mut ds = None;
            while cursor < header_end {
                if guid_at(&dirty, cursor, &HEADER_EXTENSION_OBJECT) {
                    ds = Some(u32_at(&dirty, cursor + 24 + 18));
                    break;
                }
                cursor += u64_at(&dirty, cursor + 16) as usize;
            }
            ds.expect("ext present")
        };
        // The on-disk size of the Metadata Library Object (the removed bytes).
        let mlo_size = {
            // scan inside the extension object's nested data.
            let header_end = orig_hsize as usize;
            let mut cursor = 24 + 4 + 2usize;
            let mut ext_start = None;
            while cursor < header_end {
                if guid_at(&dirty, cursor, &HEADER_EXTENSION_OBJECT) {
                    ext_start = Some(cursor);
                    break;
                }
                cursor += u64_at(&dirty, cursor + 16) as usize;
            }
            let ext_start = ext_start.expect("ext present");
            let data_start = ext_start + 24 + 22;
            let data_end = data_start + orig_ext_data_size as usize;
            let mut nc = data_start;
            let mut found = None;
            while nc < data_end {
                let sz = u64_at(&dirty, nc + 16);
                if guid_at(&dirty, nc, &METADATA_LIBRARY_OBJECT) {
                    found = Some(sz);
                    break;
                }
                nc += sz as usize;
            }
            found.expect("mlo present")
        };

        let cleaned = strip(&dirty).expect("strip must succeed");

        // (a) THE LEAK IS CLOSED: the secret bytes are gone.
        assert!(
            !cleaned
                .windows(b"WM/EncodingSettings=secret".len())
                .any(|w| w == b"WM/EncodingSettings=secret"),
            "cleaned output must NOT contain the WM/EncodingSettings secret"
        );
        // The Metadata Library GUID must be absent everywhere.
        assert!(
            !cleaned.windows(16).any(|w| w == METADATA_LIBRARY_OBJECT),
            "Metadata Library Object GUID must be gone"
        );

        // (b) the nested Stream Properties object SURVIVES.
        assert!(
            cleaned
                .windows(b"NESTED-STREAM-keep-me".len())
                .any(|w| w == b"NESTED-STREAM-keep-me"),
            "nested Stream Properties payload must be preserved"
        );

        // (c) child count UNCHANGED (the Header Extension Object remains).
        let (new_hsize, new_count, new_guids) = reparse(&cleaned);
        assert_eq!(new_count, 2, "Header Object child count must not change");
        assert!(new_guids.contains(&HEADER_EXTENSION_OBJECT));
        assert!(new_guids.contains(&FILE_PROPERTIES_OBJECT));

        // (d) Header Extension Data Size + Header Extension Object size +
        //     Header Object size are all corrected by exactly mlo_size.
        let new_ext_size = header_child_size(&cleaned, &HEADER_EXTENSION_OBJECT);
        assert_eq!(
            new_ext_size,
            orig_ext_size - mlo_size,
            "Header Extension Object size must shrink by the removed object"
        );
        // read patched data-size u32.
        let new_ext_data_size = {
            let header_end = new_hsize as usize;
            let mut cursor = 24 + 4 + 2usize;
            let mut ds = None;
            while cursor < header_end {
                if guid_at(&cleaned, cursor, &HEADER_EXTENSION_OBJECT) {
                    ds = Some(u32_at(&cleaned, cursor + 24 + 18));
                    break;
                }
                cursor += u64_at(&cleaned, cursor + 16) as usize;
            }
            ds.expect("ext present")
        };
        assert_eq!(
            u64::from(new_ext_data_size),
            u64::from(orig_ext_data_size) - mlo_size,
            "Header Extension Data Size must shrink by the removed object"
        );
        assert_eq!(
            new_hsize,
            orig_hsize - mlo_size,
            "Header Object size must shrink by the removed object"
        );

        // (e) File Properties File Size field decremented by the same amount.
        let header_end = new_hsize as usize;
        let mut cursor = 24 + 4 + 2usize;
        let mut fp_file_size = None;
        while cursor < header_end {
            if guid_at(&cleaned, cursor, &FILE_PROPERTIES_OBJECT) {
                fp_file_size = Some(u64_at(&cleaned, cursor + 24 + 16));
                break;
            }
            cursor += u64_at(&cleaned, cursor + 16) as usize;
        }
        assert_eq!(
            fp_file_size,
            Some(8192 - mlo_size),
            "File Properties File Size must drop by the removed bytes"
        );

        // (f) the Data Object is byte-identical (and still present once).
        let data_pos = cleaned
            .windows(data_obj_bytes.len())
            .position(|w| w == data_obj_bytes.as_slice())
            .expect("Data Object must survive byte-identical");
        // It must sit at the new end (after the shrunken header).
        assert_eq!(
            data_pos, header_end,
            "Data Object must immediately follow the shrunken Header Object"
        );

        // (g) whole-file length shrank by exactly the removed object.
        assert_eq!(cleaned.len() as u64, dirty.len() as u64 - mlo_size);

        // (h) the nested extension data must still parse and exactly fill.
        let ext_start = data_pos - new_ext_size as usize; // ext is the last header child
        let nested_start = ext_start + 24 + 22;
        let nested_end = nested_start + new_ext_data_size as usize;
        let mut nc = nested_start;
        let mut seen = 0;
        while nc < nested_end {
            seen += 1;
            nc += u64_at(&cleaned, nc + 16) as usize;
        }
        assert_eq!(nc, nested_end, "nested objects must exactly fill ext data");
        assert_eq!(seen, 1, "exactly one nested object (Stream Props) remains");
    }

    /// A Header Extension Object with no metadata objects nested inside it
    /// must be returned untouched (no spurious shrink, no corruption).
    #[test]
    fn header_extension_without_metadata_unchanged() {
        let mut children = Vec::new();
        let mut fp_payload = Vec::new();
        fp_payload.extend_from_slice(&[0xAB; 16]);
        fp_payload.extend_from_slice(&512u64.to_le_bytes());
        fp_payload.extend_from_slice(&[0u8; 8]);
        push_object(&mut children, &FILE_PROPERTIES_OBJECT, &fp_payload);

        // Header Extension with only a (kept) nested stream object.
        const STREAM_PROPS: [u8; 16] = [
            0x91, 0x07, 0xDC, 0xB7, 0xB7, 0xA9, 0xCF, 0x11, 0x8E, 0xE6, 0x00, 0xC0, 0x0C, 0x20,
            0x53, 0x65,
        ];
        let mut nested = Vec::new();
        push_object(&mut nested, &STREAM_PROPS, b"keep");
        let mut ext_payload = Vec::new();
        ext_payload.extend_from_slice(&[0xCD; 16]);
        ext_payload.extend_from_slice(&6u16.to_le_bytes());
        ext_payload.extend_from_slice(&(nested.len() as u32).to_le_bytes());
        ext_payload.extend_from_slice(&nested);
        push_object(&mut children, &HEADER_EXTENSION_OBJECT, &ext_payload);

        let mut header_payload = Vec::new();
        header_payload.extend_from_slice(&2u32.to_le_bytes());
        header_payload.push(0x01);
        header_payload.push(0x02);
        header_payload.extend_from_slice(&children);

        let mut file = Vec::new();
        push_object(&mut file, &HEADER_OBJECT, &header_payload);

        let cleaned = strip(&file).expect("strip ok");
        assert_eq!(
            cleaned, file,
            "Header Extension without metadata must be returned unchanged"
        );
    }

    /// A truncated Header Extension Data Size (claiming more nested bytes
    /// than the object holds) must be rejected, not panic.
    #[test]
    fn rejects_bogus_header_extension_data_size() {
        let mut children = Vec::new();
        let mut fp_payload = Vec::new();
        fp_payload.extend_from_slice(&[0xAB; 16]);
        fp_payload.extend_from_slice(&512u64.to_le_bytes());
        fp_payload.extend_from_slice(&[0u8; 8]);
        push_object(&mut children, &FILE_PROPERTIES_OBJECT, &fp_payload);

        // Extension object whose data-size claims 9999 but holds far less,
        // and which nests a Metadata Library Object so the rewrite path runs.
        let mut nested = Vec::new();
        push_object(&mut nested, &METADATA_LIBRARY_OBJECT, b"x");
        let mut ext_payload = Vec::new();
        ext_payload.extend_from_slice(&[0xCD; 16]);
        ext_payload.extend_from_slice(&6u16.to_le_bytes());
        ext_payload.extend_from_slice(&9999u32.to_le_bytes()); // bogus, too big
        ext_payload.extend_from_slice(&nested);
        push_object(&mut children, &HEADER_EXTENSION_OBJECT, &ext_payload);

        let mut header_payload = Vec::new();
        header_payload.extend_from_slice(&2u32.to_le_bytes());
        header_payload.push(0x01);
        header_payload.push(0x02);
        header_payload.extend_from_slice(&children);

        let mut file = Vec::new();
        push_object(&mut file, &HEADER_OBJECT, &header_payload);

        assert!(
            matches!(strip(&file), Err(CoreError::ParseError { .. })),
            "bogus Header Extension Data Size must be a ParseError, not a panic"
        );
    }

    #[test]
    fn rejects_non_asf() {
        let junk = vec![0u8; 64];
        assert!(matches!(strip(&junk), Err(CoreError::ParseError { .. })));
    }

    #[test]
    fn rejects_truncated_header() {
        // Valid header GUID but a size field claiming more than the buffer.
        let mut buf = Vec::new();
        buf.extend_from_slice(&HEADER_OBJECT);
        buf.extend_from_slice(&9999u64.to_le_bytes());
        buf.extend_from_slice(&[0u8; 8]);
        assert!(matches!(strip(&buf), Err(CoreError::ParseError { .. })));
    }

    #[test]
    fn no_metadata_returns_unchanged() {
        // Header with only a File Properties + a (kept) stream object.
        let mut children = Vec::new();
        let mut fp_payload = Vec::new();
        fp_payload.extend_from_slice(&[0xAB; 16]);
        fp_payload.extend_from_slice(&512u64.to_le_bytes());
        fp_payload.extend_from_slice(&[0u8; 8]);
        push_object(&mut children, &FILE_PROPERTIES_OBJECT, &fp_payload);

        let mut header_payload = Vec::new();
        header_payload.extend_from_slice(&1u32.to_le_bytes());
        header_payload.push(0x01);
        header_payload.push(0x02);
        header_payload.extend_from_slice(&children);

        let mut file = Vec::new();
        push_object(&mut file, &HEADER_OBJECT, &header_payload);

        let cleaned = strip(&file).expect("strip ok");
        assert_eq!(
            cleaned, file,
            "no-metadata input must be returned unchanged"
        );
    }
}
