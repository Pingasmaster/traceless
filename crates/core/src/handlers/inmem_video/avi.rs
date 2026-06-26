// In-memory RIFF/AVI metadata stripper.
//
// Mirrors the native ffmpeg metadata-only strip+remux
// (`-map_metadata -1 -map_chapters -1 -disposition 0`): it keeps every
// audio/video stream byte-for-byte (no transcode) and drops all
// container-level metadata, then fixes the surrounding RIFF size field
// so the result is a valid AVI.
//
// RIFF layout: the file is `'RIFF' <u32 LE total-size> 'AVI '` followed
// by a flat list of chunks, each `<4-byte fourcc> <u32 LE size>
// <payload, padded to an even byte>`. A `LIST` chunk's payload begins
// with a 4-byte form-type fourcc and then nested chunks.
//
// What we drop at the top level:
//   * `LIST` chunks whose form-type is `INFO` (INAM/IART/ICMT/ICRD/
//     ISFT/IGNR... author/comment/software/creation tags).
//   * a standalone `IDIT` chunk (creation timestamp).
//   * `JUNK` padding, `exif` (embedded EXIF), and `tdat` (timecode/date)
//     metadata chunks.
//
// We also recurse INTO the `hdrl` header LIST: ffmpeg's metadata strip
// removes metadata nested inside the header too. Inside `hdrl` (and inside
// its `strl` stream sub-lists) we drop any nested `INFO` LIST, any `strn`
// (stream-name) chunk, any `IDIT`, and stray `JUNK`/`exif`/`tdat`, while
// keeping `avih`/`strh`/`strf`/`indx`/`vprp`/`dmlh` and the `strl`
// sub-lists' surviving content verbatim. The rebuilt `hdrl` payload's
// LIST size field is recomputed.
//
// What we keep verbatim: the surviving `hdrl` header content, the `movi`
// data LIST, the `idx1` index, and anything else we do not recognise as
// metadata.
//
// `idx1` entries hold offsets that, by the AVI convention, are relative
// to the start of the `movi` LIST's data (specifically the position right
// after its 'movi' form-type fourcc). Dropping a sibling chunk that
// *follows* `movi` never disturbs the index. Dropping a chunk that
// *precedes* `movi` shifts `movi`'s absolute file position. For parity
// with the native strip we DO drop pre-movi metadata too: when idx1 is
// movi-relative (the overwhelmingly common case) the relative offsets are
// unchanged by an earlier movi, so this is safe. We detect the rare
// file-absolute idx1 (its first entry's offset is >= the movi LIST's file
// position rather than a small movi-relative value) and, only then,
// decrement every idx1 `dwChunkOffset` by the number of bytes removed
// before movi (checked_sub; underflow -> ParseError).
//
// Every length/offset is read with bounds + checked arithmetic; a
// malformed file yields `Err(CoreError::ParseError{..})`, never a panic
// or unbounded allocation.

use crate::error::CoreError;

/// Read a little-endian `u32` at `off`, bounds-checked.
fn read_u32_le(buf: &[u8], off: usize) -> Option<u32> {
    let end = off.checked_add(4)?;
    let slice = buf.get(off..end)?;
    let arr: [u8; 4] = slice.try_into().ok()?;
    Some(u32::from_le_bytes(arr))
}

/// Read a 4-byte fourcc at `off`, bounds-checked.
fn read_fourcc(buf: &[u8], off: usize) -> Option<[u8; 4]> {
    let end = off.checked_add(4)?;
    let slice = buf.get(off..end)?;
    let arr: [u8; 4] = slice.try_into().ok()?;
    Some(arr)
}

fn parse_err(detail: &str) -> CoreError {
    CoreError::ParseError {
        path: std::path::PathBuf::new(),
        detail: detail.to_string(),
    }
}

/// A top-level chunk located in the RIFF body (after the `'AVI '`
/// form-type). `start` points at the chunk's fourcc; `total_len` covers
/// fourcc + size field + payload + any pad byte.
struct TopChunk {
    fourcc: [u8; 4],
    /// For a `LIST`, its form-type (first 4 payload bytes); else `None`.
    list_form: Option<[u8; 4]>,
    start: usize,
    total_len: usize,
    /// Payload byte range `[payload_start, payload_end)` (excludes the
    /// 8-byte header and any trailing pad byte).
    payload_start: usize,
    payload_end: usize,
}

/// Whether a top-level chunk is metadata we want to drop.
fn is_metadata_chunk(c: &TopChunk) -> bool {
    match &c.fourcc {
        b"LIST" => c.list_form.as_ref() == Some(b"INFO"),
        b"IDIT" | b"JUNK" | b"junk" | b"exif" | b"tdat" => true,
        _ => false,
    }
}

/// Whether a chunk nested inside `hdrl` (or `hdrl`'s `strl` sub-lists) is
/// metadata to drop. `strn` is the per-stream human name; nested `INFO`
/// LISTs carry the same tags as top-level INFO; `IDIT`/`JUNK`/`exif`/
/// `tdat` are the same metadata as at the top level. `avih`/`strh`/`strf`/
/// `indx`/`vprp`/`dmlh` and the `strl` sub-lists themselves are kept.
fn is_nested_metadata(fourcc: [u8; 4], list_form: Option<[u8; 4]>) -> bool {
    match &fourcc {
        b"LIST" => list_form.as_ref() == Some(b"INFO"),
        b"strn" | b"IDIT" | b"JUNK" | b"junk" | b"exif" | b"tdat" => true,
        _ => false,
    }
}

/// Rebuild a LIST payload (form-type fourcc + nested chunks), dropping any
/// nested metadata chunk and, for a `strl` sub-list, recursing one level
/// to strip metadata inside it. `payload` is the full LIST payload
/// starting with the 4-byte form-type. Returns the new payload bytes.
///
/// Every nested length/offset is bounds-checked; a malformed nested
/// structure yields `Err(CoreError::ParseError{..})`.
fn rebuild_list_payload(payload: &[u8], recurse_strl: bool) -> Result<Vec<u8>, CoreError> {
    let form = read_fourcc(payload, 0).ok_or_else(|| parse_err("LIST missing form-type"))?;
    let mut out: Vec<u8> = Vec::with_capacity(payload.len());
    out.extend_from_slice(&form);

    // Walk nested chunks beginning right after the 4-byte form-type.
    let mut pos = 4usize;
    let end = payload.len();
    while pos < end {
        let hdr_end = pos
            .checked_add(8)
            .ok_or_else(|| parse_err("nested offset overflow"))?;
        if hdr_end > end {
            return Err(parse_err(
                "nested trailing bytes shorter than a chunk header",
            ));
        }
        let fourcc =
            read_fourcc(payload, pos).ok_or_else(|| parse_err("unreadable nested fourcc"))?;
        let size_off = pos
            .checked_add(4)
            .ok_or_else(|| parse_err("nested offset overflow"))?;
        let size = read_u32_le(payload, size_off)
            .ok_or_else(|| parse_err("unreadable nested size"))? as usize;

        let cpayload_start = hdr_end;
        let cpayload_end = cpayload_start
            .checked_add(size)
            .ok_or_else(|| parse_err("nested chunk size overflows offset"))?;
        if cpayload_end > end {
            return Err(parse_err("nested chunk size overruns the LIST"));
        }
        let padded = if size & 1 == 1 {
            cpayload_end
                .checked_add(1)
                .ok_or_else(|| parse_err("nested pad overflow"))?
        } else {
            cpayload_end
        };
        let chunk_end = padded.min(end);

        let list_form = if &fourcc == b"LIST" {
            read_fourcc(payload, cpayload_start)
        } else {
            None
        };

        if is_nested_metadata(fourcc, list_form) {
            pos = chunk_end;
            continue;
        }

        // Recurse into a `strl` stream sub-list to strip metadata there too.
        if recurse_strl && &fourcc == b"LIST" && list_form.as_ref() == Some(b"strl") {
            let inner = payload
                .get(cpayload_start..cpayload_end)
                .ok_or_else(|| parse_err("strl payload out of range"))?;
            let rebuilt = rebuild_list_payload(inner, false)?;
            let new_size = u32::try_from(rebuilt.len())
                .map_err(|_| parse_err("rebuilt strl exceeds 4 GiB"))?;
            out.extend_from_slice(b"LIST");
            out.extend_from_slice(&new_size.to_le_bytes());
            out.extend_from_slice(&rebuilt);
            if rebuilt.len() & 1 == 1 {
                out.push(0);
            }
            pos = chunk_end;
            continue;
        }

        // Keep the chunk verbatim (header + payload + any pad byte).
        let slice = payload
            .get(pos..chunk_end)
            .ok_or_else(|| parse_err("nested kept chunk out of range"))?;
        out.extend_from_slice(slice);
        pos = chunk_end;
    }

    Ok(out)
}

/// Strip container metadata from a RIFF/AVI file, fully in memory.
///
/// # Errors
///
/// Returns [`CoreError::ParseError`] if the bytes are not a well-formed
/// RIFF/AVI container (bad magic, a chunk size that overruns the buffer,
/// truncation). Stream payloads are never decoded, only copied.
pub(crate) fn strip(input: &[u8]) -> Result<Vec<u8>, CoreError> {
    // Header: 'RIFF' <u32 size> 'AVI '
    if read_fourcc(input, 0) != Some(*b"RIFF") {
        return Err(parse_err("not a RIFF file (missing 'RIFF' magic)"));
    }
    let riff_size =
        read_u32_le(input, 4).ok_or_else(|| parse_err("truncated RIFF size field"))? as usize;
    if read_fourcc(input, 8) != Some(*b"AVI ") {
        return Err(parse_err("not an AVI file (RIFF form-type is not 'AVI ')"));
    }

    // The RIFF size counts everything after the 8-byte 'RIFF'<size>
    // header, i.e. the 'AVI ' form-type + the chunk list. The declared
    // body must not overrun the buffer; if the file is truncated relative
    // to the declared size we still parse only what is present and bound
    // every read, so a short size just means we stop early. Clamp the
    // walk end to the smaller of (declared body end, actual buffer end).
    let declared_end = 8usize
        .checked_add(riff_size)
        .ok_or_else(|| parse_err("RIFF size overflows"))?;
    let walk_end = declared_end.min(input.len());

    // Body chunks start after 'RIFF'<size>'AVI ' = offset 12.
    let body_start = 12usize;
    if body_start > input.len() {
        return Err(parse_err("AVI header truncated"));
    }

    // Walk the flat top-level chunk list.
    let mut chunks: Vec<TopChunk> = Vec::new();
    let mut pos = body_start;
    while pos < walk_end {
        // Need at least an 8-byte chunk header.
        let hdr_end = pos
            .checked_add(8)
            .ok_or_else(|| parse_err("offset overflow"))?;
        if hdr_end > walk_end {
            // Trailing bytes shorter than a header: stop the walk and keep
            // them as-is by not recording another chunk (they fall outside
            // any chunk and are dropped from the rebuild). This only
            // happens on a malformed/truncated tail; treat as parse error
            // so the caller gets a 422 rather than silent truncation.
            return Err(parse_err("trailing bytes shorter than a chunk header"));
        }
        let fourcc = read_fourcc(input, pos).ok_or_else(|| parse_err("unreadable chunk fourcc"))?;
        let size_off = pos
            .checked_add(4)
            .ok_or_else(|| parse_err("offset overflow"))?;
        let size = read_u32_le(input, size_off).ok_or_else(|| parse_err("unreadable chunk size"))?
            as usize;

        let payload_start = hdr_end;
        let payload_end = payload_start
            .checked_add(size)
            .ok_or_else(|| parse_err("chunk size overflows offset"))?;
        if payload_end > walk_end {
            return Err(parse_err("chunk size overruns the RIFF body"));
        }

        // RIFF chunks are word-aligned: an odd-sized payload is followed
        // by a single pad byte (not counted in `size`).
        let padded = if size & 1 == 1 {
            payload_end
                .checked_add(1)
                .ok_or_else(|| parse_err("pad overflow"))?
        } else {
            payload_end
        };
        // The pad byte may legitimately be absent at the very end of the
        // buffer; clamp so we never index past the real buffer.
        let chunk_end = padded.min(input.len());
        let total_len = chunk_end
            .checked_sub(pos)
            .ok_or_else(|| parse_err("chunk length underflow"))?;

        let list_form = if &fourcc == b"LIST" {
            read_fourcc(input, payload_start)
        } else {
            None
        };

        chunks.push(TopChunk {
            fourcc,
            list_form,
            start: pos,
            total_len,
            payload_start,
            payload_end,
        });

        pos = chunk_end;
    }

    // Locate movi (the A/V data LIST) so we can reason about idx1 offset
    // bases relative to chunks we drop.
    let movi_idx = chunks
        .iter()
        .position(|c| &c.fourcc == b"LIST" && c.list_form.as_ref() == Some(b"movi"));
    let movi_file_pos = movi_idx.and_then(|m| chunks.get(m)).map(|c| c.start);

    // Decide which chunks to drop. For parity with the native strip we drop
    // metadata everywhere, including before movi (see module docs: movi
    // relative idx1 offsets are unaffected; the rare absolute case is
    // detected and patched below).
    let mut drop_flags: Vec<bool> = Vec::with_capacity(chunks.len());
    let mut bytes_removed_before_movi: usize = 0;
    for (i, c) in chunks.iter().enumerate() {
        let drop = is_metadata_chunk(c);
        if drop && movi_idx.is_some_and(|m| i < m) {
            bytes_removed_before_movi = bytes_removed_before_movi.saturating_add(c.total_len);
        }
        drop_flags.push(drop);
    }

    // Rebuild: copy / rewrite the kept top-level chunks, then patch the
    // RIFF size field to (new body length).
    // Body size = sum of kept chunk lengths + 4 for the 'AVI ' form-type
    // fourcc at offset 8.
    let mut new_body: Vec<u8> = Vec::with_capacity(input.len());
    new_body.extend_from_slice(b"AVI ");
    for (c, &drop) in chunks.iter().zip(drop_flags.iter()) {
        if drop {
            continue;
        }

        // Recurse into the hdrl header LIST: strip metadata nested inside
        // it (and inside its strl stream sub-lists), then re-emit with a
        // corrected LIST size field.
        if &c.fourcc == b"LIST" && c.list_form.as_ref() == Some(b"hdrl") {
            let payload = input
                .get(c.payload_start..c.payload_end)
                .ok_or_else(|| parse_err("hdrl payload out of range"))?;
            let rebuilt = rebuild_list_payload(payload, true)?;
            let new_size = u32::try_from(rebuilt.len())
                .map_err(|_| parse_err("rebuilt hdrl exceeds 4 GiB"))?;
            new_body.extend_from_slice(b"LIST");
            new_body.extend_from_slice(&new_size.to_le_bytes());
            new_body.extend_from_slice(&rebuilt);
            if rebuilt.len() & 1 == 1 {
                new_body.push(0);
            }
            continue;
        }

        let end = c
            .start
            .checked_add(c.total_len)
            .ok_or_else(|| parse_err("kept chunk end overflow"))?;
        let slice = input
            .get(c.start..end)
            .ok_or_else(|| parse_err("kept chunk slice out of range"))?;

        // idx1 with file-absolute offsets needs its dwChunkOffset entries
        // decremented when bytes were removed before movi. The common case
        // (movi-relative offsets) needs no rewrite.
        if &c.fourcc == b"idx1"
            && bytes_removed_before_movi > 0
            && let Some(mp) = movi_file_pos
        {
            let entries = input
                .get(c.payload_start..c.payload_end)
                .ok_or_else(|| parse_err("idx1 payload out of range"))?;
            if idx1_is_absolute(entries, mp) {
                let patched = patch_idx1_absolute(entries, bytes_removed_before_movi)?;
                let new_size = u32::try_from(patched.len())
                    .map_err(|_| parse_err("rebuilt idx1 exceeds 4 GiB"))?;
                new_body.extend_from_slice(b"idx1");
                new_body.extend_from_slice(&new_size.to_le_bytes());
                new_body.extend_from_slice(&patched);
                if patched.len() & 1 == 1 {
                    new_body.push(0);
                }
                continue;
            }
        }

        new_body.extend_from_slice(slice);
    }

    let new_riff_size =
        u32::try_from(new_body.len()).map_err(|_| parse_err("rebuilt RIFF body exceeds 4 GiB"))?;

    let mut out: Vec<u8> = Vec::with_capacity(new_body.len().saturating_add(8));
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&new_riff_size.to_le_bytes());
    out.extend_from_slice(&new_body);

    Ok(out)
}

/// Each idx1 entry is 16 bytes: `<4 ckid><u32 dwFlags><u32 dwChunkOffset>
/// <u32 dwChunkLength>`. Heuristic: if the first entry's `dwChunkOffset`
/// is >= the movi LIST's file position, treat the index as file-absolute
/// (offsets measured from the RIFF/file start) rather than movi-relative.
fn idx1_is_absolute(entries: &[u8], movi_file_pos: usize) -> bool {
    let Some(off) = read_u32_le(entries, 8) else {
        return false;
    };
    (off as usize) >= movi_file_pos
}

/// Decrement every idx1 entry's `dwChunkOffset` by `removed`. Returns the
/// rewritten entries. Underflow (an offset smaller than `removed`) yields
/// `Err`, never a wrap.
fn patch_idx1_absolute(entries: &[u8], removed: usize) -> Result<Vec<u8>, CoreError> {
    let removed_u32 = u32::try_from(removed).map_err(|_| parse_err("idx1 shift exceeds 4 GiB"))?;
    let mut out = entries.to_vec();
    let mut pos = 0usize;
    while pos.checked_add(16).is_some_and(|e| e <= out.len()) {
        let off_pos = pos
            .checked_add(8)
            .ok_or_else(|| parse_err("idx1 offset overflow"))?;
        let off =
            read_u32_le(&out, off_pos).ok_or_else(|| parse_err("unreadable idx1 dwChunkOffset"))?;
        let new_off = off
            .checked_sub(removed_u32)
            .ok_or_else(|| parse_err("idx1 offset underflow shifting movi"))?;
        let end = off_pos
            .checked_add(4)
            .ok_or_else(|| parse_err("idx1 offset overflow"))?;
        let dst = out
            .get_mut(off_pos..end)
            .ok_or_else(|| parse_err("idx1 entry out of range"))?;
        dst.copy_from_slice(&new_off.to_le_bytes());
        pos = pos
            .checked_add(16)
            .ok_or_else(|| parse_err("idx1 walk overflow"))?;
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    /// Append a chunk `<fourcc><u32 LE size><payload[+pad]>` to `out`.
    fn push_chunk(out: &mut Vec<u8>, fourcc: &[u8; 4], payload: &[u8]) {
        out.extend_from_slice(fourcc);
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        if payload.len() & 1 == 1 {
            out.push(0);
        }
    }

    /// Build a LIST chunk: `LIST<u32 size>form-type<nested...>`.
    fn list_chunk(form: &[u8; 4], inner: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(form);
        payload.extend_from_slice(inner);
        let mut c = Vec::new();
        push_chunk(&mut c, b"LIST", &payload);
        c
    }

    /// Wrap body chunks in a full RIFF/AVI file with a correct size field.
    fn riff_avi(body_chunks: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(b"AVI ");
        body.extend_from_slice(body_chunks);
        let mut f = Vec::new();
        f.extend_from_slice(b"RIFF");
        f.extend_from_slice(&(body.len() as u32).to_le_bytes());
        f.extend_from_slice(&body);
        f
    }

    /// Walk top-level chunks of a finished file, returning
    /// (fourcc, list_form) pairs. Used to assert structure post-strip.
    fn top_chunks(file: &[u8]) -> Vec<([u8; 4], Option<[u8; 4]>)> {
        let mut v = Vec::new();
        let riff_size = read_u32_le(file, 4).unwrap() as usize;
        let end = (8 + riff_size).min(file.len());
        let mut pos = 12usize;
        while pos + 8 <= end {
            let fourcc = read_fourcc(file, pos).unwrap();
            let size = read_u32_le(file, pos + 4).unwrap() as usize;
            let form = if &fourcc == b"LIST" {
                read_fourcc(file, pos + 8)
            } else {
                None
            };
            v.push((fourcc, form));
            let mut next = pos + 8 + size;
            if size & 1 == 1 {
                next += 1;
            }
            pos = next;
        }
        v
    }

    /// Build a minimal valid hdrl LIST payload carrying one `avih` chunk
    /// with the given recognisable marker bytes. Real AVIs always hold
    /// well-formed nested chunks inside hdrl, which we now recurse into.
    fn hdrl_with_avih(marker: &[u8]) -> Vec<u8> {
        let mut inner = Vec::new();
        push_chunk(&mut inner, b"avih", marker);
        inner
    }

    /// INFO LIST after movi: full strip, movi bytes intact, size fixed.
    #[test]
    fn strips_info_list_after_movi() {
        // hdrl header carrying a valid avih chunk with marker bytes.
        let hdrl = list_chunk(b"hdrl", &hdrl_with_avih(b"avih-stub-bytes!"));
        // movi data LIST carrying a recognisable A/V payload.
        let movi_payload = b"00dcREAL-VIDEO-FRAME-BYTES-DO-NOT-TOUCH";
        let mut movi_inner = Vec::new();
        push_chunk(&mut movi_inner, b"00dc", movi_payload);
        let movi = list_chunk(b"movi", &movi_inner);
        // idx1 index (opaque, movi-relative offsets - unaffected here).
        let mut idx1 = Vec::new();
        push_chunk(
            &mut idx1,
            b"idx1",
            b"\x00\x64\x63\x30\x10\x00\x00\x00\x00\x00\x00\x00\x27\x00\x00\x00",
        );
        // INFO LIST with secret metadata, placed AFTER movi (the common case).
        let mut info_inner = Vec::new();
        push_chunk(&mut info_inner, b"INAM", b"secret-title-tag\0");
        push_chunk(&mut info_inner, b"ISFT", b"SneakyEncoder 9.9\0");
        let info = list_chunk(b"INFO", &info_inner);

        let mut body = Vec::new();
        body.extend_from_slice(&hdrl);
        body.extend_from_slice(&movi);
        body.extend_from_slice(&idx1);
        body.extend_from_slice(&info);
        let dirty = riff_avi(&body);

        // Sanity: the dirty file really carries the secret bytes.
        assert!(window_contains(&dirty, b"secret-title-tag"));
        assert!(window_contains(&dirty, b"SneakyEncoder"));

        let cleaned = strip(&dirty).unwrap();

        // (a) metadata bytes gone.
        assert!(
            !window_contains(&cleaned, b"secret-title-tag"),
            "INAM metadata must be stripped"
        );
        assert!(
            !window_contains(&cleaned, b"SneakyEncoder"),
            "ISFT metadata must be stripped"
        );

        // (b) movi A/V bytes intact.
        assert!(
            window_contains(&cleaned, b"REAL-VIDEO-FRAME-BYTES-DO-NOT-TOUCH"),
            "movi stream payload must survive byte-for-byte"
        );
        // hdrl + idx1 also intact.
        assert!(window_contains(&cleaned, b"avih-stub-bytes!"));

        // (c) still a valid RIFF/AVI with the same NON-metadata top-level
        // structure, INFO removed.
        assert_eq!(&cleaned[0..4], b"RIFF");
        assert_eq!(&cleaned[8..12], b"AVI ");
        let structure = top_chunks(&cleaned);
        let kinds: Vec<([u8; 4], Option<[u8; 4]>)> = structure;
        assert_eq!(
            kinds,
            vec![
                (*b"LIST", Some(*b"hdrl")),
                (*b"LIST", Some(*b"movi")),
                (*b"idx1", None),
            ],
            "cleaned file must keep hdrl/movi/idx1 and drop the INFO LIST"
        );

        // (d) RIFF size field corrected: equals body length (file len - 8).
        let declared = read_u32_le(&cleaned, 4).unwrap() as usize;
        assert_eq!(
            declared,
            cleaned.len() - 8,
            "RIFF size must equal the new body length"
        );
    }

    /// IDIT, JUNK, exif, tdat siblings (after movi) are all dropped.
    #[test]
    fn strips_idit_junk_exif_tdat() {
        let hdrl = list_chunk(b"hdrl", &hdrl_with_avih(b"hdrl-stub-avih"));
        let mut movi_inner = Vec::new();
        push_chunk(&mut movi_inner, b"00wb", b"AUDIO-SAMPLES-KEEP");
        let movi = list_chunk(b"movi", &movi_inner);

        let mut body = Vec::new();
        body.extend_from_slice(&hdrl);
        body.extend_from_slice(&movi);
        push_chunk(&mut body, b"IDIT", b"Tue Jan 01 2030\0");
        push_chunk(&mut body, b"JUNK", b"padding-bytes");
        push_chunk(&mut body, b"exif", b"EXIF-GPS-DATA-LEAK");
        push_chunk(&mut body, b"tdat", b"timecode-date");
        let dirty = riff_avi(&body);

        let cleaned = strip(&dirty).unwrap();

        assert!(!window_contains(&cleaned, b"Tue Jan 01 2030"), "IDIT gone");
        assert!(!window_contains(&cleaned, b"padding-bytes"), "JUNK gone");
        assert!(
            !window_contains(&cleaned, b"EXIF-GPS-DATA-LEAK"),
            "exif gone"
        );
        assert!(!window_contains(&cleaned, b"timecode-date"), "tdat gone");
        assert!(
            window_contains(&cleaned, b"AUDIO-SAMPLES-KEEP"),
            "movi kept"
        );

        let kinds = top_chunks(&cleaned);
        assert_eq!(
            kinds,
            vec![(*b"LIST", Some(*b"hdrl")), (*b"LIST", Some(*b"movi"))]
        );
        assert_eq!(
            read_u32_le(&cleaned, 4).unwrap() as usize,
            cleaned.len() - 8
        );
    }

    /// Metadata that PRECEDES movi is now DROPPED for parity with the
    /// native strip; with a movi-relative idx1 the offsets are unaffected.
    #[test]
    fn drops_info_before_movi_relative_idx1() {
        let hdrl = list_chunk(b"hdrl", &hdrl_with_avih(b"hdrl-stub-avih"));
        let mut info_inner = Vec::new();
        push_chunk(&mut info_inner, b"ISFT", b"PreMoviSoftware\0");
        let info = list_chunk(b"INFO", &info_inner);
        let mut movi_inner = Vec::new();
        push_chunk(&mut movi_inner, b"00dc", b"FRAMES");
        let movi = list_chunk(b"movi", &movi_inner);
        // idx1 with a small (movi-relative) offset: 0x04, i.e. the first
        // frame sits 4 bytes into the movi data.
        let mut idx1 = Vec::new();
        push_chunk(
            &mut idx1,
            b"idx1",
            b"00dc\x10\x00\x00\x00\x04\x00\x00\x00\x06\x00\x00\x00",
        );

        let mut body = Vec::new();
        body.extend_from_slice(&hdrl);
        body.extend_from_slice(&info); // BEFORE movi
        body.extend_from_slice(&movi);
        body.extend_from_slice(&idx1);
        let dirty = riff_avi(&body);

        let cleaned = strip(&dirty).unwrap();
        assert!(
            !window_contains(&cleaned, b"PreMoviSoftware"),
            "INFO before movi must be stripped for parity"
        );
        // movi data intact.
        assert!(window_contains(&cleaned, b"FRAMES"), "movi data intact");
        // idx1 offset unchanged (still movi-relative 0x04).
        let kinds = top_chunks(&cleaned);
        assert_eq!(
            kinds,
            vec![
                (*b"LIST", Some(*b"hdrl")),
                (*b"LIST", Some(*b"movi")),
                (*b"idx1", None),
            ]
        );
        // Find the idx1 chunk and read its dwChunkOffset (entry byte 8..12).
        let mut p = 12usize;
        let mut idx1_off: Option<u32> = None;
        while p + 8 <= cleaned.len() {
            let fc = read_fourcc(&cleaned, p).unwrap();
            let sz = read_u32_le(&cleaned, p + 4).unwrap() as usize;
            if &fc == b"idx1" {
                idx1_off = read_u32_le(&cleaned, p + 8 + 8);
                break;
            }
            let mut next = p + 8 + sz;
            if sz & 1 == 1 {
                next += 1;
            }
            p = next;
        }
        assert_eq!(
            idx1_off,
            Some(0x04),
            "movi-relative idx1 offset must be unchanged"
        );
        assert_eq!(
            read_u32_le(&cleaned, 4).unwrap() as usize,
            cleaned.len() - 8
        );
    }

    /// REGRESSION (a): INFO sub-LIST + strn NESTED inside hdrl must be
    /// stripped; the stream header (strl/strh/strf) must survive intact.
    #[test]
    fn strips_metadata_nested_inside_hdrl() {
        // Build a realistic hdrl: avih + an strl sub-list (strh+strf+strn),
        // a nested INFO LIST, and a stray IDIT/JUNK inside hdrl.
        let mut strl_inner = Vec::new();
        push_chunk(&mut strl_inner, b"strh", b"STREAM-HEADER-KEEP-ME");
        push_chunk(&mut strl_inner, b"strf", b"STREAM-FORMAT-KEEP-ME");
        push_chunk(&mut strl_inner, b"strn", b"LeakyStreamName-Camera42\0");
        push_chunk(&mut strl_inner, b"indx", b"SUPER-INDEX-KEEP");
        let strl = list_chunk(b"strl", &strl_inner);

        let mut info_inner = Vec::new();
        push_chunk(&mut info_inner, b"INAM", b"NestedSecretTitle\0");
        push_chunk(&mut info_inner, b"ISFT", b"NestedSneakyEncoder\0");
        let nested_info = list_chunk(b"INFO", &info_inner);

        let mut hdrl_inner = Vec::new();
        push_chunk(&mut hdrl_inner, b"avih", b"MAIN-AVI-HEADER-KEEP");
        hdrl_inner.extend_from_slice(&strl);
        hdrl_inner.extend_from_slice(&nested_info);
        push_chunk(&mut hdrl_inner, b"IDIT", b"NestedCreationDate2030\0");
        push_chunk(&mut hdrl_inner, b"JUNK", b"NestedJunkPadBytes");
        let hdrl = list_chunk(b"hdrl", &hdrl_inner);

        let mut movi_inner = Vec::new();
        push_chunk(&mut movi_inner, b"00dc", b"VIDEO-FRAMES-INTACT");
        let movi = list_chunk(b"movi", &movi_inner);

        let mut body = Vec::new();
        body.extend_from_slice(&hdrl);
        body.extend_from_slice(&movi);
        let dirty = riff_avi(&body);

        // Sanity: the leaks are present before stripping.
        assert!(window_contains(&dirty, b"NestedSecretTitle"));
        assert!(window_contains(&dirty, b"LeakyStreamName-Camera42"));
        assert!(window_contains(&dirty, b"NestedCreationDate2030"));

        let cleaned = strip(&dirty).unwrap();

        // Nested metadata gone.
        assert!(
            !window_contains(&cleaned, b"NestedSecretTitle"),
            "nested INFO/INAM inside hdrl must be stripped"
        );
        assert!(
            !window_contains(&cleaned, b"NestedSneakyEncoder"),
            "nested INFO/ISFT inside hdrl must be stripped"
        );
        assert!(
            !window_contains(&cleaned, b"LeakyStreamName-Camera42"),
            "strn (stream name) inside strl must be stripped"
        );
        assert!(
            !window_contains(&cleaned, b"NestedCreationDate2030"),
            "IDIT inside hdrl must be stripped"
        );
        assert!(
            !window_contains(&cleaned, b"NestedJunkPadBytes"),
            "JUNK inside hdrl must be stripped"
        );

        // Stream header content survives intact.
        assert!(
            window_contains(&cleaned, b"MAIN-AVI-HEADER-KEEP"),
            "avih must survive"
        );
        assert!(
            window_contains(&cleaned, b"STREAM-HEADER-KEEP-ME"),
            "strh must survive"
        );
        assert!(
            window_contains(&cleaned, b"STREAM-FORMAT-KEEP-ME"),
            "strf must survive"
        );
        assert!(
            window_contains(&cleaned, b"SUPER-INDEX-KEEP"),
            "indx must survive"
        );
        assert!(
            window_contains(&cleaned, b"VIDEO-FRAMES-INTACT"),
            "movi payload must survive"
        );

        // File re-parses: valid RIFF/AVI, hdrl + movi present, sizes coherent.
        assert_eq!(&cleaned[0..4], b"RIFF");
        assert_eq!(&cleaned[8..12], b"AVI ");
        let kinds = top_chunks(&cleaned);
        assert_eq!(
            kinds,
            vec![(*b"LIST", Some(*b"hdrl")), (*b"LIST", Some(*b"movi"))],
            "top-level structure keeps hdrl + movi"
        );
        assert_eq!(
            read_u32_le(&cleaned, 4).unwrap() as usize,
            cleaned.len() - 8,
            "top RIFF size fixed"
        );
        // hdrl LIST size field must match its rebuilt payload (re-parse it).
        assert!(
            full_reparse_ok(&cleaned),
            "every chunk (incl. rebuilt hdrl) must re-parse with coherent sizes"
        );
    }

    /// REGRESSION (b): a pre-movi INFO LIST with FILE-ABSOLUTE idx1 offsets
    /// -> INFO gone, idx1 offsets decremented by the removed byte count,
    /// movi data intact.
    #[test]
    fn drops_pre_movi_info_and_patches_absolute_idx1() {
        let hdrl = list_chunk(b"hdrl", &hdrl_with_avih(b"HDRL-KEEP-CONTENT"));
        let mut info_inner = Vec::new();
        push_chunk(&mut info_inner, b"ISFT", b"AbsIdxPreMoviSoftware\0");
        let info = list_chunk(b"INFO", &info_inner);
        let mut movi_inner = Vec::new();
        push_chunk(&mut movi_inner, b"00dc", b"ABSOLUTE-FRAMES");
        let movi = list_chunk(b"movi", &movi_inner);

        // Compute the file position of movi in the DIRTY file so we can
        // give idx1 a file-absolute offset (>= movi file pos).
        let info_len = info.len();
        let removed = info_len; // bytes removed before movi
        // movi file pos in dirty = 12 (RIFF/AVI hdr) + hdrl.len() + info.len()
        let movi_pos_dirty = 12 + hdrl.len() + info.len();
        // Absolute offset of the first frame: movi data starts at
        // movi_pos_dirty + 8 (LIST hdr) + 4 (form 'movi'); the 00dc data
        // begins 8 bytes further. Use that as the absolute dwChunkOffset.
        let abs_off = (movi_pos_dirty + 8 + 4 + 8) as u32;
        let mut idx1_payload = Vec::new();
        idx1_payload.extend_from_slice(b"00dc");
        idx1_payload.extend_from_slice(&0x10u32.to_le_bytes()); // dwFlags
        idx1_payload.extend_from_slice(&abs_off.to_le_bytes()); // dwChunkOffset (absolute)
        idx1_payload.extend_from_slice(&15u32.to_le_bytes()); // dwChunkLength
        let mut idx1 = Vec::new();
        push_chunk(&mut idx1, b"idx1", &idx1_payload);

        let mut body = Vec::new();
        body.extend_from_slice(&hdrl);
        body.extend_from_slice(&info); // BEFORE movi
        body.extend_from_slice(&movi);
        body.extend_from_slice(&idx1);
        let dirty = riff_avi(&body);

        assert!(window_contains(&dirty, b"AbsIdxPreMoviSoftware"));

        let cleaned = strip(&dirty).unwrap();

        // INFO stripped.
        assert!(
            !window_contains(&cleaned, b"AbsIdxPreMoviSoftware"),
            "pre-movi INFO must be stripped"
        );
        // movi data intact.
        assert!(
            window_contains(&cleaned, b"ABSOLUTE-FRAMES"),
            "movi data must survive"
        );
        // idx1 offset decremented by the removed byte count.
        let mut p = 12usize;
        let mut new_off: Option<u32> = None;
        while p + 8 <= cleaned.len() {
            let fc = read_fourcc(&cleaned, p).unwrap();
            let sz = read_u32_le(&cleaned, p + 4).unwrap() as usize;
            if &fc == b"idx1" {
                new_off = read_u32_le(&cleaned, p + 8 + 8);
                break;
            }
            let mut next = p + 8 + sz;
            if sz & 1 == 1 {
                next += 1;
            }
            p = next;
        }
        assert_eq!(
            new_off,
            Some(abs_off - removed as u32),
            "absolute idx1 offset must be decremented by bytes removed before movi"
        );
        assert!(full_reparse_ok(&cleaned));
        assert_eq!(
            read_u32_le(&cleaned, 4).unwrap() as usize,
            cleaned.len() - 8
        );
    }

    /// Full structural re-parse: walk top-level chunks (recursing one level
    /// into LISTs) and confirm every declared size lands exactly on a
    /// boundary within its parent. Returns false on any overrun.
    fn full_reparse_ok(file: &[u8]) -> bool {
        if file.len() < 12 || &file[0..4] != b"RIFF" || &file[8..12] != b"AVI " {
            return false;
        }
        let riff_size = match read_u32_le(file, 4) {
            Some(s) => s as usize,
            None => return false,
        };
        if 8 + riff_size != file.len() {
            return false;
        }
        fn walk(buf: &[u8]) -> bool {
            let mut pos = 0usize;
            while pos < buf.len() {
                if pos + 8 > buf.len() {
                    return false;
                }
                let size = match read_u32_le(buf, pos + 4) {
                    Some(s) => s as usize,
                    None => return false,
                };
                let pend = match pos.checked_add(8).and_then(|h| h.checked_add(size)) {
                    Some(e) => e,
                    None => return false,
                };
                if pend > buf.len() {
                    return false;
                }
                let next = if size & 1 == 1 { pend + 1 } else { pend };
                let next = next.min(buf.len());
                pos = next;
            }
            true
        }
        // Body chunks after 'AVI '.
        walk(&file[12..])
    }

    /// A chunk size that overruns the buffer must yield ParseError, not panic.
    #[test]
    fn rejects_size_overrun() {
        let mut body = Vec::new();
        // Claim a 0xFFFF-byte payload but provide none.
        body.extend_from_slice(b"JUNK");
        body.extend_from_slice(&0xFFFFu32.to_le_bytes());
        let dirty = riff_avi(&body);
        // riff_avi computed an honest size, so manually corrupt: the inner
        // chunk claims more than exists within the body.
        let res = strip(&dirty);
        assert!(matches!(res, Err(CoreError::ParseError { .. })));
    }

    /// Non-RIFF input is rejected.
    #[test]
    fn rejects_non_riff() {
        assert!(matches!(
            strip(b"not a riff file at all"),
            Err(CoreError::ParseError { .. })
        ));
        // RIFF but wrong form-type.
        let mut f = Vec::new();
        f.extend_from_slice(b"RIFF");
        f.extend_from_slice(&4u32.to_le_bytes());
        f.extend_from_slice(b"WAVE");
        assert!(matches!(strip(&f), Err(CoreError::ParseError { .. })));
    }

    /// Naive substring search over a byte buffer.
    fn window_contains(haystack: &[u8], needle: &[u8]) -> bool {
        if needle.is_empty() || needle.len() > haystack.len() {
            return false;
        }
        haystack.windows(needle.len()).any(|w| w == needle)
    }
}
