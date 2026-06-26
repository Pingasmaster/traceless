// Pure-Rust, fully in-memory ISO Base Media File Format (ISO-BMFF)
// metadata stripper for the `wasm-inmem` build.
//
// Covers the MP4 / QuickTime family (`video/mp4`, `video/quicktime`,
// `audio/mp4`, `audio/m4a`, `audio/x-m4a`, `audio/aac` when carried in an
// MP4 container). It mirrors what the native ffmpeg path does:
//
// ```text
// ffmpeg -i in -map 0 -c copy -map_metadata -1 -map_chapters -1 \
//        -disposition 0 -fflags +bitexact out
// ```
//
// that is, a *metadata-only* strip + remux: every audio / video sample
// stays byte-for-byte (no transcode), but all container / global / track
// tags, chapters and attachments are dropped, and the result is a valid
// file.
//
// ## What it drops
//
// - `moov/udta` (the whole user-data subtree: its `meta`/`keys`/`ilst`
//   children, the freeform `©xyz` GPS atom, encoder name, etc.).
// - `moov/meta` (the metadata box inside moov, iTunes-style).
// - every per-`trak` `udta` and `trak`-level `meta`.
// - a top-level (file-level) `meta` box (ISO 14496-12 §8.11.1, an
//   iTunes/QuickTime metadata sibling of `moov`; the native ffmpeg
//   `-map_metadata -1` baseline drops it too).
// - top-level `free` / `skip` / `uuid` boxes (and `free`/`skip` found
//   anywhere we rewrite).
//
// ## What it keeps verbatim
//
// - `ftyp`, `mdat` (the media payload), and the rest of `moov`
//   (`mvhd`, every `trak`'s `tkhd` / `mdia` / `minf` / `stbl` sample
//   tables and codec configuration).
//
// ## The chunk-offset trap
//
// `stco` (32-bit) and `co64` (64-bit) hold *absolute file offsets* into
// `mdat`. When `moov` precedes `mdat` (faststart / progressive-download
// layout) shrinking `moov` shifts `mdat` earlier, so every chunk-offset
// entry must be decremented by the exact number of bytes `moov` shrank,
// or playback breaks. When `mdat` precedes `moov` no patching is needed.
// This module computes the `moov` delta precisely and patches.
//
// ## Approach
//
// It does *not* fully decode the codec atoms into typed structs (which
// risks a non-byte-exact round-trip of exotic boxes). Instead it walks
// the box framing using [`mp4_atom::Header`] for spec-correct
// size/largesize handling, copies kept boxes byte-for-byte, and only
// reaches inside `moov` / `trak` / `mdia` / `minf` / `stbl` far enough to
// drop the metadata boxes and locate the chunk-offset tables. Every
// length and offset read from the (attacker-controlled) input is
// bounds-checked and uses checked arithmetic; a malformed file yields a
// `CoreError`, never a panic or unbounded allocation.

use std::io::Cursor;

use mp4_atom::{Decode, Encode, FourCC, Header};

use crate::error::CoreError;

const FTYP: &[u8; 4] = b"ftyp";
const MOOV: &[u8; 4] = b"moov";
const TRAK: &[u8; 4] = b"trak";
const MDIA: &[u8; 4] = b"mdia";
const MINF: &[u8; 4] = b"minf";
const STBL: &[u8; 4] = b"stbl";
const UDTA: &[u8; 4] = b"udta";
const META: &[u8; 4] = b"meta";
const STCO: &[u8; 4] = b"stco";
const CO64: &[u8; 4] = b"co64";
const FREE: &[u8; 4] = b"free";
const SKIP: &[u8; 4] = b"skip";
const UUID: &[u8; 4] = b"uuid";
const MDAT: &[u8; 4] = b"mdat";
const MVHD: &[u8; 4] = b"mvhd";
const TKHD: &[u8; 4] = b"tkhd";
const MDHD: &[u8; 4] = b"mdhd";

/// Maximum container nesting depth we will recurse through. Real files
/// nest only a handful of levels (`moov/trak/mdia/minf/stbl`); a crafted
/// file with hundreds of thousands of nested containers would otherwise
/// blow the small wasm guest stack (SIGABRT). Exceeding this bound yields
/// a `ParseError` instead of recursing further.
const MAX_BOX_DEPTH: u32 = 64;

fn parse_err(detail: impl Into<String>) -> CoreError {
    CoreError::ParseError {
        path: std::path::PathBuf::new(),
        detail: detail.into(),
    }
}

fn clean_err(detail: impl Into<String>) -> CoreError {
    CoreError::CleanError {
        path: std::path::PathBuf::new(),
        detail: detail.into(),
    }
}

/// A single top-level box read from the input, in file order. Pass-through
/// boxes are stored as a `[start, end)` byte range into the original
/// `input` slice (no copy: this is what avoids materializing the multi-GiB
/// `mdat` twice). Only `moov` is rewritten, and that happens out of band.
struct TopBox {
    kind: [u8; 4],
    start: usize,
    end: usize,
}

/// Read one box header at the cursor's current position. Returns the
/// FourCC, the *body* length in bytes (the payload after the header), and
/// the *header* length in bytes (8 or 16). `None` body length means the
/// box extends to EOF (a `size==0` box, only legal for the last box).
fn read_header(cur: &mut Cursor<&[u8]>) -> Result<(FourCC, Option<usize>, usize), CoreError> {
    let start =
        usize::try_from(cur.position()).map_err(|_| parse_err("cursor position overflow"))?;
    let header = Header::decode(cur).map_err(|e| parse_err(format!("bad box header: {e}")))?;
    let after =
        usize::try_from(cur.position()).map_err(|_| parse_err("cursor position overflow"))?;
    let header_len = after
        .checked_sub(start)
        .ok_or_else(|| parse_err("header length underflow"))?;
    if header_len != 8 && header_len != 16 {
        return Err(parse_err(format!(
            "unexpected box header length {header_len}"
        )));
    }
    Ok((header.kind, header.size, header_len))
}

/// Serialize a box header for `kind` with the given *body* length, always
/// using a 32-bit size when it fits (the common case) and a 64-bit
/// largesize otherwise. Returns the header bytes.
fn write_header(kind: [u8; 4], body_len: usize) -> Result<Vec<u8>, CoreError> {
    let header = Header {
        kind: FourCC::from(kind),
        size: Some(body_len),
    };
    let mut out = Vec::new();
    header
        .encode(&mut out)
        .map_err(|e| clean_err(format!("failed to encode box header: {e}")))?;
    Ok(out)
}

/// Borrow `len` bytes of `data` starting at `off`, bounds-checked.
fn slice_at(data: &[u8], off: usize, len: usize) -> Result<&[u8], CoreError> {
    let end = off
        .checked_add(len)
        .ok_or_else(|| parse_err("box extent overflow"))?;
    data.get(off..end)
        .ok_or_else(|| parse_err("box extends past end of input"))
}

/// Strip metadata from an ISO-BMFF buffer.
///
/// # Errors
///
/// Returns [`CoreError::ParseError`] if the box structure is malformed
/// (bad sizes, truncated boxes, missing `moov`), or
/// [`CoreError::CleanError`] for an internal re-encode failure.
// `pub(crate)` is the cross-handler convention (the dispatcher calls
// `inmem_video_isobmff::strip`); the module is private only because the
// integrator has not promoted it yet, so silence the nursery lint.
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn strip(input: &[u8]) -> Result<Vec<u8>, CoreError> {
    crate::handlers::check_input_len(input.len())?;

    // 1. Split the file into its top-level boxes, in order. We keep the
    //    raw bytes of each so non-moov boxes pass through byte-for-byte.
    let mut top: Vec<TopBox> = Vec::new();
    let mut cur = Cursor::new(input);
    let mut saw_ftyp = false;
    let mut saw_moov = false;
    // Track whether we have passed `mdat` yet, and the cumulative size of
    // every top-level box we DROP *before* `mdat`. Chunk offsets in
    // `stco`/`co64` are absolute into the file, so any box dropped ahead of
    // `mdat` shifts the media payload earlier and must be compensated for
    // (see the offset-patch block below), not just the `moov` shrink.
    let mut saw_mdat = false;
    let mut pre_mdat_dropped: usize = 0;
    loop {
        let pos =
            usize::try_from(cur.position()).map_err(|_| parse_err("cursor position overflow"))?;
        if pos == input.len() {
            break;
        }
        let box_start = pos;
        let (kind, body_len, header_len) = read_header(&mut cur)?;
        let kind4: [u8; 4] = kind.into();

        // Total on-disk size of this box (header + body). A `None` body
        // length means "to EOF".
        let total = match body_len {
            Some(b) => header_len
                .checked_add(b)
                .ok_or_else(|| parse_err("box size overflow"))?,
            None => input
                .len()
                .checked_sub(box_start)
                .ok_or_else(|| parse_err("box size underflow"))?,
        };
        // Bounds-check the box extent against the input, then record its
        // range (no copy). `slice_at` validates `box_start + total` fits.
        let _ = slice_at(input, box_start, total)?;
        let box_end = box_start
            .checked_add(total)
            .ok_or_else(|| parse_err("box end overflow"))?;
        // Advance the cursor past this whole box (the header was already
        // consumed by `read_header`; jump straight to the box end).
        let new_pos = u64::try_from(box_end).map_err(|_| parse_err("cursor position overflow"))?;
        cur.set_position(new_pos);

        if &kind4 == FTYP {
            saw_ftyp = true;
        }
        if &kind4 == MOOV {
            saw_moov = true;
        }
        if &kind4 == MDAT {
            saw_mdat = true;
        }

        // Drop top-level free/skip/uuid and a file-level (top-level) `meta`
        // box (ISO 14496-12 §8.11.1, an iTunes/QuickTime metadata sibling
        // of moov; the native ffmpeg `-map_metadata -1` baseline drops it).
        // Everything else is recorded and passes through verbatim.
        if matches!(&kind4, FREE | SKIP | UUID | META) {
            // Track the size of anything dropped *before* mdat so chunk
            // offsets (absolute into mdat) can be compensated below. A
            // box at or after mdat does not shift mdat, so it adds nothing.
            if !saw_mdat {
                pre_mdat_dropped = pre_mdat_dropped
                    .checked_add(total)
                    .ok_or_else(|| parse_err("dropped-size overflow"))?;
            }
            continue;
        }
        top.push(TopBox {
            kind: kind4,
            start: box_start,
            end: box_end,
        });
    }

    if !saw_ftyp {
        return Err(parse_err("not an ISO-BMFF file (no ftyp box)"));
    }
    if !saw_moov {
        return Err(parse_err("ISO-BMFF file has no moov box"));
    }

    // 2. Determine whether moov precedes mdat in the original file. If so,
    //    shrinking moov shifts mdat, and chunk offsets need patching.
    let moov_idx = top
        .iter()
        .position(|b| &b.kind == MOOV)
        .ok_or_else(|| parse_err("ISO-BMFF file has no moov box"))?;
    let mdat_idx = top.iter().position(|b| &b.kind == MDAT);
    let moov_before_mdat = match mdat_idx {
        Some(mi) => moov_idx < mi,
        None => false,
    };

    // 3. Rewrite the moov box: drop metadata subtrees, and (if needed)
    //    patch chunk offsets by the moov shrink delta.
    let moov_box = {
        let m = top
            .get(moov_idx)
            .ok_or_else(|| clean_err("moov index out of range"))?;
        slice_at(
            input,
            m.start,
            m.end
                .checked_sub(m.start)
                .ok_or_else(|| clean_err("moov range underflow"))?,
        )?
    };
    let old_moov_total = moov_box.len();
    let new_moov = rewrite_moov(moov_box)?;
    let new_moov_total = new_moov.len();
    let moov_delta = old_moov_total
        .checked_sub(new_moov_total)
        .ok_or_else(|| clean_err("moov grew during strip (unexpected)"))?;

    // mdat's absolute position drops by every top-level box dropped before
    // mdat (`pre_mdat_dropped`) PLUS the moov shrink (only when moov itself
    // precedes mdat). Patch chunk offsets whenever mdat exists and anything
    // ahead of it shrank, not just when moov precedes mdat: a top-level
    // `meta` dropped before an mdat-first moov still shifts mdat.
    let mut total_shift = pre_mdat_dropped;
    if moov_before_mdat {
        total_shift = total_shift
            .checked_add(moov_delta)
            .ok_or_else(|| clean_err("offset shift overflow"))?;
    }
    let new_moov = if mdat_idx.is_some() && total_shift != 0 {
        patch_chunk_offsets(new_moov, total_shift)?
    } else {
        new_moov
    };

    // 4. Re-assemble: every kept top-level box in order, with moov swapped
    //    for its rewritten form. Pass-through boxes (incl. mdat) are sliced
    //    straight from `input` so the media payload is copied at most once.
    let mut out = Vec::with_capacity(input.len());
    for (i, b) in top.iter().enumerate() {
        if i == moov_idx {
            out.extend_from_slice(&new_moov);
        } else {
            let span = b
                .end
                .checked_sub(b.start)
                .ok_or_else(|| clean_err("box range underflow"))?;
            out.extend_from_slice(slice_at(input, b.start, span)?);
        }
    }
    Ok(out)
}

/// Rewrite a whole `moov` box (header + body), dropping the metadata
/// subtrees. Returns the new box bytes (header recomputed for the new
/// body length).
fn rewrite_moov(moov_box: &[u8]) -> Result<Vec<u8>, CoreError> {
    // Re-read the header to find the body offset (8 or 16 bytes).
    let mut cur = Cursor::new(moov_box);
    let (_kind, _body_len, header_len) = read_header(&mut cur)?;
    let body = moov_box
        .get(header_len..)
        .ok_or_else(|| parse_err("moov body out of range"))?;

    let new_body = rewrite_container(body, ContainerKind::Moov, 0)?;
    let mut out = write_header(*MOOV, new_body.len())?;
    out.extend_from_slice(&new_body);
    Ok(out)
}

/// Which container we are rewriting, so we know which children to drop
/// and which to recurse into.
#[derive(Clone, Copy)]
enum ContainerKind {
    /// `moov`: drop `udta` + `meta`; recurse into `trak`.
    Moov,
    /// `trak`: drop `udta` + `meta`; recurse into `mdia`.
    Trak,
    /// `mdia`: recurse into `minf` (also drop any stray `udta`/`meta`).
    Mdia,
    /// `minf`: recurse into `stbl`.
    Minf,
    /// `stbl`: leave `stco`/`co64` in place (offsets patched later); drop
    /// nothing here but pass through verbatim. We still walk it so a
    /// later pass can locate the tables, but rewriting copies bytes
    /// unchanged.
    Stbl,
}

/// Walk the children of a container body and emit a new body with the
/// metadata children removed and the relevant sub-containers recursively
/// rewritten. Returns the new body bytes.
fn rewrite_container(body: &[u8], kind: ContainerKind, depth: u32) -> Result<Vec<u8>, CoreError> {
    if depth > MAX_BOX_DEPTH {
        return Err(parse_err(
            "ISO-BMFF container nesting exceeds maximum depth",
        ));
    }
    let mut out = Vec::with_capacity(body.len());
    let mut cur = Cursor::new(body);
    loop {
        let pos =
            usize::try_from(cur.position()).map_err(|_| parse_err("cursor position overflow"))?;
        if pos == body.len() {
            break;
        }
        if pos > body.len() {
            return Err(parse_err("child box overran container"));
        }
        let child_start = pos;
        let (ckind, cbody_len, cheader_len) = read_header(&mut cur)?;
        let ckind4: [u8; 4] = ckind.into();
        let total = match cbody_len {
            Some(b) => cheader_len
                .checked_add(b)
                .ok_or_else(|| parse_err("child box size overflow"))?,
            None => body
                .len()
                .checked_sub(child_start)
                .ok_or_else(|| parse_err("child box size underflow"))?,
        };
        let child = slice_at(body, child_start, total)?;
        let new_pos = u64::try_from(
            child_start
                .checked_add(total)
                .ok_or_else(|| parse_err("child box end overflow"))?,
        )
        .map_err(|_| parse_err("cursor position overflow"))?;
        cur.set_position(new_pos);

        // Drop free/skip wherever we rewrite.
        if matches!(&ckind4, FREE | SKIP) {
            continue;
        }

        match kind {
            ContainerKind::Moov => {
                if matches!(&ckind4, UDTA | META) {
                    continue; // drop file-level user-data / metadata
                }
                if &ckind4 == TRAK {
                    let rewritten =
                        rewrite_subbox(child, cheader_len, *TRAK, ContainerKind::Trak, depth)?;
                    out.extend_from_slice(&rewritten);
                    continue;
                }
                if &ckind4 == MVHD {
                    // Zero creation_time / modification_time (bitexact baseline).
                    let zeroed = zero_timestamps(child, cheader_len)?;
                    out.extend_from_slice(&zeroed);
                    continue;
                }
                out.extend_from_slice(child); // mvex, etc. verbatim
            }
            ContainerKind::Trak => {
                if matches!(&ckind4, UDTA | META) {
                    continue; // drop per-track user-data / metadata
                }
                if &ckind4 == MDIA {
                    let rewritten =
                        rewrite_subbox(child, cheader_len, *MDIA, ContainerKind::Mdia, depth)?;
                    out.extend_from_slice(&rewritten);
                    continue;
                }
                if &ckind4 == TKHD {
                    let zeroed = zero_timestamps(child, cheader_len)?;
                    out.extend_from_slice(&zeroed);
                    continue;
                }
                out.extend_from_slice(child); // edts, tref, etc.
            }
            ContainerKind::Mdia => {
                if matches!(&ckind4, UDTA | META) {
                    continue;
                }
                if &ckind4 == MINF {
                    let rewritten =
                        rewrite_subbox(child, cheader_len, *MINF, ContainerKind::Minf, depth)?;
                    out.extend_from_slice(&rewritten);
                    continue;
                }
                if &ckind4 == MDHD {
                    let zeroed = zero_timestamps(child, cheader_len)?;
                    out.extend_from_slice(&zeroed);
                    continue;
                }
                out.extend_from_slice(child); // hdlr, etc.
            }
            ContainerKind::Minf => {
                if &ckind4 == STBL {
                    let rewritten =
                        rewrite_subbox(child, cheader_len, *STBL, ContainerKind::Stbl, depth)?;
                    out.extend_from_slice(&rewritten);
                    continue;
                }
                out.extend_from_slice(child); // vmhd, smhd, dinf, etc.
            }
            ContainerKind::Stbl => {
                // Leave the sample-table children alone (stco/co64 are
                // patched in a later pass that scans the assembled moov).
                out.extend_from_slice(child);
            }
        }
    }
    Ok(out)
}

/// Rewrite a sub-container box (`trak`/`mdia`/`minf`/`stbl`): re-read its
/// header, rewrite its body via [`rewrite_container`], and re-frame with a
/// fresh header for the new body length.
fn rewrite_subbox(
    child: &[u8],
    header_len: usize,
    kind4: [u8; 4],
    container: ContainerKind,
    depth: u32,
) -> Result<Vec<u8>, CoreError> {
    let inner = child
        .get(header_len..)
        .ok_or_else(|| parse_err("sub-box body out of range"))?;
    let next_depth = depth
        .checked_add(1)
        .ok_or_else(|| parse_err("box depth overflow"))?;
    let new_inner = rewrite_container(inner, container, next_depth)?;
    let mut out = write_header(kind4, new_inner.len())?;
    out.extend_from_slice(&new_inner);
    Ok(out)
}

/// Zero the `creation_time` and `modification_time` fields of an
/// `mvhd` / `tkhd` / `mdhd` box (they share the same leading layout), to
/// match the ffmpeg `+bitexact` baseline. The box body starts after
/// `header_len` bytes; its first byte is the version (0 or 1), then 3
/// flag bytes, then the two timestamps. Version 0 uses u32 fields
/// (creation @ body+4, modification @ body+8); version 1 uses u64 fields
/// (creation @ body+4, modification @ body+12). No other field, and
/// neither the box size nor the moov size, changes. Returns the box bytes
/// with those fields zeroed.
fn zero_timestamps(child: &[u8], header_len: usize) -> Result<Vec<u8>, CoreError> {
    let mut out = child.to_vec();
    // version byte is the first body byte.
    let version = *out
        .get(header_len)
        .ok_or_else(|| parse_err("timestamp box has no version byte"))?;
    // creation_time starts 4 bytes into the body (after version + 3 flags).
    let creation_off = header_len
        .checked_add(4)
        .ok_or_else(|| parse_err("timestamp offset overflow"))?;
    let field_size = match version {
        0 => 4usize,
        1 => 8usize,
        v => return Err(parse_err(format!("unsupported timestamp box version {v}"))),
    };
    // modification_time immediately follows creation_time.
    let modification_off = creation_off
        .checked_add(field_size)
        .ok_or_else(|| parse_err("timestamp offset overflow"))?;
    let zeroed_end = modification_off
        .checked_add(field_size)
        .ok_or_else(|| parse_err("timestamp offset overflow"))?;
    let zone = out
        .get_mut(creation_off..zeroed_end)
        .ok_or_else(|| parse_err("timestamp fields extend past box"))?;
    for b in zone.iter_mut() {
        *b = 0;
    }
    Ok(out)
}

/// Scan a rewritten `moov` box and decrement every `stco` (32-bit) and
/// `co64` (64-bit) chunk-offset entry by `delta`. Operates on the moov
/// bytes after the drop pass, so offsets are walked relative to a fully
/// assembled box. Returns the patched moov bytes.
fn patch_chunk_offsets(mut moov_box: Vec<u8>, delta: usize) -> Result<Vec<u8>, CoreError> {
    let delta_u32 = u32::try_from(delta).map_err(|_| clean_err("moov delta exceeds u32"))?;
    let delta_u64 = delta as u64;
    // Walk the box tree in place, collecting the byte ranges of stco/co64
    // entry tables, then patch them.
    let mut patches: Vec<(usize, bool)> = Vec::new(); // (entry_table_start, is_co64)
    collect_offset_tables(&moov_box, 0, moov_box.len(), &mut patches, 0)?;

    for (table_start, is_co64) in patches {
        // The table layout: version(1) + flags(3) + entry_count(4) +
        // entries. `table_start` points at the version byte (the box body
        // start). Read entry_count, then patch each entry.
        let count_off = table_start
            .checked_add(4)
            .ok_or_else(|| parse_err("offset table header overflow"))?;
        let count_end = count_off
            .checked_add(4)
            .ok_or_else(|| parse_err("offset table header overflow"))?;
        let count_bytes = moov_box
            .get(count_off..count_end)
            .ok_or_else(|| parse_err("offset table truncated"))?;
        let entry_count = u32::from_be_bytes([
            count_bytes[0],
            count_bytes[1],
            count_bytes[2],
            count_bytes[3],
        ]);
        let mut entry_off = count_end;
        let entry_size = if is_co64 { 8usize } else { 4usize };
        for _ in 0..entry_count {
            let entry_end = entry_off
                .checked_add(entry_size)
                .ok_or_else(|| parse_err("offset entry overflow"))?;
            let entry = moov_box
                .get(entry_off..entry_end)
                .ok_or_else(|| parse_err("offset table truncated"))?;
            if is_co64 {
                let v = u64::from_be_bytes([
                    entry[0], entry[1], entry[2], entry[3], entry[4], entry[5], entry[6], entry[7],
                ]);
                let nv = v
                    .checked_sub(delta_u64)
                    .ok_or_else(|| clean_err("co64 chunk offset underflowed when shifting moov"))?;
                let dst = moov_box
                    .get_mut(entry_off..entry_end)
                    .ok_or_else(|| clean_err("offset table write out of range"))?;
                dst.copy_from_slice(&nv.to_be_bytes());
            } else {
                let v = u32::from_be_bytes([entry[0], entry[1], entry[2], entry[3]]);
                let nv = v
                    .checked_sub(delta_u32)
                    .ok_or_else(|| clean_err("stco chunk offset underflowed when shifting moov"))?;
                let dst = moov_box
                    .get_mut(entry_off..entry_end)
                    .ok_or_else(|| clean_err("offset table write out of range"))?;
                dst.copy_from_slice(&nv.to_be_bytes());
            }
            entry_off = entry_end;
        }
    }
    Ok(moov_box)
}

/// Recursively scan `data[start..end]` (a sequence of boxes) for `stco` /
/// `co64` boxes, recording the byte offset of each one's body (the
/// version byte) and whether it is a `co64`. Only descends into the
/// container boxes that can hold a sample table (`trak`/`mdia`/`minf`/
/// `stbl`); leaf boxes are not scanned (so a `stco` FourCC appearing
/// inside opaque codec-config data is never misread).
fn collect_offset_tables(
    data: &[u8],
    start: usize,
    end: usize,
    out: &mut Vec<(usize, bool)>,
    depth: u32,
) -> Result<(), CoreError> {
    if depth > MAX_BOX_DEPTH {
        return Err(parse_err(
            "ISO-BMFF container nesting exceeds maximum depth",
        ));
    }
    let region = data
        .get(start..end)
        .ok_or_else(|| parse_err("scan region out of range"))?;
    let mut cur = Cursor::new(region);
    loop {
        let rel =
            usize::try_from(cur.position()).map_err(|_| parse_err("cursor position overflow"))?;
        if rel == region.len() {
            break;
        }
        if rel > region.len() {
            return Err(parse_err("box overran region during offset scan"));
        }
        let box_rel_start = rel;
        let (kind, body_len, header_len) = read_header(&mut cur)?;
        let kind4: [u8; 4] = kind.into();
        let total = match body_len {
            Some(b) => header_len
                .checked_add(b)
                .ok_or_else(|| parse_err("box size overflow"))?,
            None => region
                .len()
                .checked_sub(box_rel_start)
                .ok_or_else(|| parse_err("box size underflow"))?,
        };
        let abs_start = start
            .checked_add(box_rel_start)
            .ok_or_else(|| parse_err("offset overflow"))?;
        let body_abs = abs_start
            .checked_add(header_len)
            .ok_or_else(|| parse_err("offset overflow"))?;
        let body_len_resolved = total
            .checked_sub(header_len)
            .ok_or_else(|| parse_err("body underflow"))?;
        let body_abs_end = body_abs
            .checked_add(body_len_resolved)
            .ok_or_else(|| parse_err("offset overflow"))?;

        if matches!(&kind4, STCO | CO64) {
            out.push((body_abs, &kind4 == CO64));
        } else if matches!(&kind4, MOOV | TRAK | MDIA | MINF | STBL) {
            let next_depth = depth
                .checked_add(1)
                .ok_or_else(|| parse_err("box depth overflow"))?;
            collect_offset_tables(data, body_abs, body_abs_end, out, next_depth)?;
        }

        let new_pos = u64::try_from(
            box_rel_start
                .checked_add(total)
                .ok_or_else(|| parse_err("box end overflow"))?,
        )
        .map_err(|_| parse_err("cursor position overflow"))?;
        cur.set_position(new_pos);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use mp4_atom::{Decode, Header};
    use std::io::Cursor;

    /// Build a box: 4-byte big-endian size (header+body), 4-byte FourCC,
    /// then body.
    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        let size = (body.len() + 8) as u32;
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    /// Full-box body: 1 version byte + 3 flag bytes + payload.
    fn fullbox_body(payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0u8, 0, 0, 0];
        v.extend_from_slice(payload);
        v
    }

    /// Build a full `stco` box: header + version/flags + entry_count + N
    /// u32 offsets.
    fn stco_body(offsets: &[u32]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
        for o in offsets {
            payload.extend_from_slice(&o.to_be_bytes());
        }
        boxed(b"stco", &fullbox_body(&payload))
    }

    /// Build a full `co64` box: header + version/flags + entry_count + N
    /// u64 offsets.
    fn co64_body(offsets: &[u64]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
        for o in offsets {
            payload.extend_from_slice(&o.to_be_bytes());
        }
        boxed(b"co64", &fullbox_body(&payload))
    }

    /// Walk top-level boxes of `data`, returning (FourCC bytes, total size).
    fn top_level(data: &[u8]) -> Vec<([u8; 4], usize)> {
        let mut out = Vec::new();
        let mut cur = Cursor::new(data);
        while (cur.position() as usize) < data.len() {
            let start = cur.position() as usize;
            let h = Header::decode(&mut cur).unwrap();
            let hlen = cur.position() as usize - start;
            let total = hlen + h.size.unwrap();
            out.push((h.kind.into(), total));
            cur.set_position((start + total) as u64);
        }
        out
    }

    /// Find a box of `kind` anywhere by simple recursive scan (only used in
    /// tests to confirm absence). Returns true if found at any depth, by
    /// walking the proper box tree of containers.
    fn box_present(data: &[u8], target: &[u8; 4]) -> bool {
        fn walk(data: &[u8], target: &[u8; 4]) -> bool {
            let mut cur = Cursor::new(data);
            while (cur.position() as usize) < data.len() {
                let start = cur.position() as usize;
                let Ok(h) = Header::decode(&mut cur) else {
                    return false;
                };
                let hlen = cur.position() as usize - start;
                let total = match h.size {
                    Some(s) => hlen + s,
                    None => data.len() - start,
                };
                let kind4: [u8; 4] = h.kind.into();
                if &kind4 == target {
                    return true;
                }
                // Recurse into known container boxes.
                if matches!(
                    &kind4,
                    b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"udta" | b"meta"
                ) {
                    let body = &data[start + hlen..start + total];
                    if walk(body, target) {
                        return true;
                    }
                }
                cur.set_position((start + total) as u64);
            }
            false
        }
        walk(data, target)
    }

    /// A minimal `trak` carrying an stbl with an stco, plus a per-track
    /// `udta` holding a freeform GPS tag.
    fn build_trak(stco: &[u8]) -> Vec<u8> {
        let tkhd = boxed(b"tkhd", &fullbox_body(&[0xAA; 80]));
        let stbl = boxed(b"stbl", stco);
        let minf = boxed(b"minf", &stbl);
        let mdhd = boxed(b"mdhd", &fullbox_body(&[0xBB; 20]));
        let mut mdia_body = Vec::new();
        mdia_body.extend_from_slice(&mdhd);
        mdia_body.extend_from_slice(&minf);
        let mdia = boxed(b"mdia", &mdia_body);
        // Per-track udta with a freeform GPS atom (©xyz).
        let gps = boxed(b"\xA9xyz", b"+48.85-002.35/GPS-SECRET");
        let trak_udta = boxed(b"udta", &gps);

        let mut trak_body = Vec::new();
        trak_body.extend_from_slice(&tkhd);
        trak_body.extend_from_slice(&trak_udta);
        trak_body.extend_from_slice(&mdia);
        boxed(b"trak", &trak_body)
    }

    /// A moov with mvhd + one trak (built above) + file-level udta(meta/ilst)
    /// + a file-level meta box.
    fn build_moov(stco: &[u8]) -> Vec<u8> {
        let mvhd = boxed(b"mvhd", &fullbox_body(&[0xCC; 96]));
        let trak = build_trak(stco);

        // file-level udta -> meta -> ilst -> ©nam (title tag)
        let nam = boxed(b"\xA9nam", b"SECRET-TITLE");
        let ilst = boxed(b"ilst", &nam);
        let udta_meta = boxed(b"meta", &{
            let mut m = fullbox_body(&[]);
            m.extend_from_slice(&ilst);
            m
        });
        let udta = boxed(b"udta", &udta_meta);

        // file-level meta (sibling of udta) holding keys.
        let keys = boxed(b"keys", b"com.apple.quicktime.make=SECRETCAM");
        let file_meta = boxed(b"meta", &{
            let mut m = fullbox_body(&[]);
            m.extend_from_slice(&keys);
            m
        });

        let mut body = Vec::new();
        body.extend_from_slice(&mvhd);
        body.extend_from_slice(&trak);
        body.extend_from_slice(&udta);
        body.extend_from_slice(&file_meta);
        boxed(b"moov", &body)
    }

    fn ftyp() -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(b"isom"); // major brand
        body.extend_from_slice(&512u32.to_be_bytes()); // minor
        body.extend_from_slice(b"isomiso2mp41"); // compatible brands
        boxed(b"ftyp", &body)
    }

    /// mdat-before-moov layout: no offset patching expected.
    #[test]
    fn strips_metadata_keeps_streams_mdat_first() {
        let mdat_payload = b"\x00\x00\x00\x01THE-RAW-MEDIA-SAMPLES-VERBATIM";
        let mdat = boxed(b"mdat", mdat_payload);
        // stco offsets point into mdat (placed first). Values are absolute
        // but for this layout we don't patch, so any value is fine.
        let stco = stco_body(&[16, 40, 88]);
        let moov = build_moov(&stco);
        // top-level free + skip + uuid to drop.
        let free = boxed(b"free", &[0u8; 32]);
        let uuid = boxed(
            b"uuid",
            b"\x11\x22\x33\x44\x55\x66\x77\x88\x99\xAA\xBB\xCC\xDD\xEE\xFF\x00LEAKYUUIDPAYLOAD",
        );

        // free/skip/uuid are placed AFTER mdat so that nothing droppable
        // precedes mdat: this keeps the test's "mdat-first, no offset patch"
        // intent intact. (A droppable box BEFORE mdat now correctly shifts
        // the chunk offsets; that path is covered by the faststart and
        // top-level-meta tests.)
        let skip = boxed(b"skip", &[0u8; 16]);
        let mut input = Vec::new();
        input.extend_from_slice(&ftyp());
        input.extend_from_slice(&mdat);
        input.extend_from_slice(&moov);
        input.extend_from_slice(&free);
        input.extend_from_slice(&skip);
        input.extend_from_slice(&uuid);

        // Sanity: the metadata IS present in the dirty input.
        assert!(
            input
                .windows(b"GPS-SECRET".len())
                .any(|w| w == b"GPS-SECRET")
        );
        assert!(
            input
                .windows(b"SECRET-TITLE".len())
                .any(|w| w == b"SECRET-TITLE")
        );
        assert!(input.windows(b"SECRETCAM".len()).any(|w| w == b"SECRETCAM"));
        assert!(input.windows(b"LEAKYUUID".len()).any(|w| w == b"LEAKYUUID"));

        let out = strip(&input).unwrap();

        // Metadata bytes are gone.
        assert!(
            !out.windows(b"GPS-SECRET".len()).any(|w| w == b"GPS-SECRET"),
            "GPS tag leaked"
        );
        assert!(
            !out.windows(b"SECRET-TITLE".len())
                .any(|w| w == b"SECRET-TITLE"),
            "title tag leaked"
        );
        assert!(
            !out.windows(b"SECRETCAM".len()).any(|w| w == b"SECRETCAM"),
            "keys tag leaked"
        );
        assert!(
            !out.windows(b"LEAKYUUID".len()).any(|w| w == b"LEAKYUUID"),
            "uuid leaked"
        );

        // The udta / meta boxes are gone from the tree.
        assert!(!box_present(&out, b"udta"), "udta survived");
        assert!(!box_present(&out, b"meta"), "meta survived");
        assert!(!box_present(&out, b"free"), "free survived");
        assert!(!box_present(&out, b"uuid"), "uuid survived");

        // Structural integrity: same number of top-level real boxes
        // (ftyp, mdat, moov) and the mdat is byte-for-byte unchanged.
        let tl: Vec<[u8; 4]> = top_level(&out).into_iter().map(|(k, _)| k).collect();
        assert_eq!(
            tl,
            vec![*b"ftyp", *b"mdat", *b"moov"],
            "top-level box set changed"
        );

        // mdat verbatim.
        assert!(
            out.windows(mdat_payload.len()).any(|w| w == mdat_payload),
            "mdat payload altered"
        );

        // Still exactly one trak, still has an stco, still has tkhd/mvhd.
        assert!(box_present(&out, b"trak"));
        assert!(box_present(&out, b"stco"));
        assert!(box_present(&out, b"mvhd"));
        assert!(box_present(&out, b"tkhd"));
        // The stco entries were NOT changed (mdat-first layout).
        let stco_vals = read_first_stco(&out);
        assert_eq!(
            stco_vals,
            vec![16, 40, 88],
            "stco entries changed in mdat-first layout"
        );
    }

    /// faststart layout: moov before mdat. stco entries must be
    /// decremented by exactly the moov shrink delta, mdat untouched.
    #[test]
    fn faststart_patches_stco_by_moov_delta() {
        let mdat_payload = b"FASTSTART-RAW-SAMPLES-DO-NOT-TOUCH-THESE-BYTES";
        let mdat = boxed(b"mdat", mdat_payload);

        // In a faststart file, chunk offsets are absolute into the file.
        // Pick offsets that sit comfortably above the moov delta so the
        // subtraction never underflows.
        let orig_offsets = [100_000u32, 200_000u32, 300_000u32];
        let stco = stco_body(&orig_offsets);
        let moov = build_moov(&stco);

        let mut input = Vec::new();
        input.extend_from_slice(&ftyp());
        input.extend_from_slice(&moov); // moov FIRST
        input.extend_from_slice(&mdat);

        let old_moov_total = moov.len();

        let out = strip(&input).unwrap();

        // Find the new moov total to compute the expected delta.
        let new_moov_total = top_level(&out)
            .into_iter()
            .find(|(k, _)| k == b"moov")
            .map(|(_, sz)| sz)
            .expect("moov present");
        let delta = old_moov_total - new_moov_total;
        assert!(delta > 0, "moov should have shrunk after dropping metadata");

        // stco entries decremented by exactly delta.
        let patched = read_first_stco(&out);
        let expected: Vec<u32> = orig_offsets.iter().map(|o| o - delta as u32).collect();
        assert_eq!(patched, expected, "stco not patched by exact moov delta");

        // mdat bytes unchanged.
        assert!(
            out.windows(mdat_payload.len()).any(|w| w == mdat_payload),
            "mdat payload altered"
        );

        // Metadata gone.
        assert!(!box_present(&out, b"udta"));
        assert!(!box_present(&out, b"meta"));
        assert!(!out.windows(b"GPS-SECRET".len()).any(|w| w == b"GPS-SECRET"));

        // Top-level order preserved: ftyp, moov, mdat.
        let tl: Vec<[u8; 4]> = top_level(&out).into_iter().map(|(k, _)| k).collect();
        assert_eq!(tl, vec![*b"ftyp", *b"moov", *b"mdat"]);
    }

    /// co64 (64-bit) faststart patching.
    #[test]
    fn faststart_patches_co64_by_moov_delta() {
        let mdat = boxed(b"mdat", b"64BIT-OFFSET-MEDIA-PAYLOAD");
        let orig = [5_000_000_000u64, 6_000_000_000u64];
        let co64 = co64_body(&orig);
        let moov = build_moov(&co64);

        let mut input = Vec::new();
        input.extend_from_slice(&ftyp());
        input.extend_from_slice(&moov);
        input.extend_from_slice(&mdat);
        let old_moov_total = moov.len();

        let out = strip(&input).unwrap();
        let new_moov_total = top_level(&out)
            .into_iter()
            .find(|(k, _)| k == b"moov")
            .map(|(_, sz)| sz)
            .unwrap();
        let delta = (old_moov_total - new_moov_total) as u64;

        let patched = read_first_co64(&out);
        let expected: Vec<u64> = orig.iter().map(|o| o - delta).collect();
        assert_eq!(patched, expected, "co64 not patched by exact moov delta");
    }

    /// Malformed input (truncated box) must error, not panic.
    #[test]
    fn truncated_box_errors() {
        // ftyp claiming a huge body but no bytes follow.
        let mut bad = Vec::new();
        bad.extend_from_slice(&1000u32.to_be_bytes());
        bad.extend_from_slice(b"ftyp");
        bad.extend_from_slice(b"isom");
        let err = strip(&bad);
        assert!(err.is_err(), "truncated box should error");
    }

    /// Build a top-level (file-level) `meta` box (sibling of moov) holding
    /// a keys/ilst tree with a recognizable secret tag.
    fn file_level_meta() -> Vec<u8> {
        let nam = boxed(b"\xA9nam", b"TOPLEVEL-META-SECRET");
        let ilst = boxed(b"ilst", &nam);
        let keys = boxed(b"keys", b"com.apple.quicktime.location=GPS-TOP-SECRET");
        let mut m = fullbox_body(&[]);
        m.extend_from_slice(&keys);
        m.extend_from_slice(&ilst);
        boxed(b"meta", &m)
    }

    /// A top-level `meta` box (sibling of moov) carrying iTunes/QuickTime
    /// tags must be dropped, and a faststart `meta`-before-mdat layout must
    /// shift the stco offsets by exactly (meta_size + moov_delta).
    #[test]
    fn drops_top_level_meta_faststart() {
        let meta = file_level_meta();
        let meta_size = meta.len();

        // moov uses NO file-level metadata so its own delta is predictable;
        // but build_moov already adds udta + an inner meta, giving a known
        // shrink. We only need the offsets high enough not to underflow.
        let orig_offsets = [1_000_000u32, 2_000_000u32];
        let stco = stco_body(&orig_offsets);
        let moov = build_moov(&stco);
        let mdat_payload = b"FASTSTART-WITH-TOPLEVEL-META";
        let mdat = boxed(b"mdat", mdat_payload);

        // Layout: ftyp | meta(file-level) | moov | mdat. Both meta and moov
        // precede mdat, so both shifts apply.
        let mut input = Vec::new();
        input.extend_from_slice(&ftyp());
        input.extend_from_slice(&meta);
        input.extend_from_slice(&moov);
        input.extend_from_slice(&mdat);

        // Sanity: the secret tags are present in the dirty input.
        assert!(
            input
                .windows(b"TOPLEVEL-META-SECRET".len())
                .any(|w| w == b"TOPLEVEL-META-SECRET")
        );
        assert!(
            input
                .windows(b"GPS-TOP-SECRET".len())
                .any(|w| w == b"GPS-TOP-SECRET")
        );

        let old_moov_total = moov.len();
        let out = strip(&input).unwrap();

        // The file-level meta and its tags are gone.
        assert!(!box_present(&out, b"meta"), "top-level meta survived");
        assert!(
            !out.windows(b"TOPLEVEL-META-SECRET".len())
                .any(|w| w == b"TOPLEVEL-META-SECRET"),
            "top-level meta title tag leaked"
        );
        assert!(
            !out.windows(b"GPS-TOP-SECRET".len())
                .any(|w| w == b"GPS-TOP-SECRET"),
            "top-level meta GPS tag leaked"
        );

        // Top-level layout is now ftyp | moov | mdat (meta dropped).
        let tl: Vec<[u8; 4]> = top_level(&out).into_iter().map(|(k, _)| k).collect();
        assert_eq!(
            tl,
            vec![*b"ftyp", *b"moov", *b"mdat"],
            "top-level box set wrong"
        );

        // stco shifted by exactly (meta_size + moov_delta).
        let new_moov_total = top_level(&out)
            .into_iter()
            .find(|(k, _)| k == b"moov")
            .map(|(_, sz)| sz)
            .expect("moov present");
        let moov_delta = old_moov_total - new_moov_total;
        let total_shift = (meta_size + moov_delta) as u32;
        let patched = read_first_stco(&out);
        let expected: Vec<u32> = orig_offsets.iter().map(|o| o - total_shift).collect();
        assert_eq!(
            patched, expected,
            "stco not shifted by meta_size + moov_delta"
        );

        // mdat payload untouched.
        assert!(
            out.windows(mdat_payload.len()).any(|w| w == mdat_payload),
            "mdat altered"
        );
    }

    /// mdat-first variant: ftyp | mdat | meta | moov. The file-level meta
    /// sits AFTER mdat, so it must still be dropped but the stco offsets are
    /// NOT patched (nothing ahead of mdat shrank).
    #[test]
    fn drops_top_level_meta_mdat_first_no_patch() {
        let meta = file_level_meta();
        let stco = stco_body(&[16, 40, 88]);
        let moov = build_moov(&stco);
        let mdat_payload = b"MDAT-FIRST-THEN-META";
        let mdat = boxed(b"mdat", mdat_payload);

        let mut input = Vec::new();
        input.extend_from_slice(&ftyp());
        input.extend_from_slice(&mdat);
        input.extend_from_slice(&meta);
        input.extend_from_slice(&moov);

        assert!(
            input
                .windows(b"TOPLEVEL-META-SECRET".len())
                .any(|w| w == b"TOPLEVEL-META-SECRET")
        );

        let out = strip(&input).unwrap();

        // meta and its tags gone.
        assert!(
            !box_present(&out, b"meta"),
            "top-level meta survived (mdat-first)"
        );
        assert!(
            !out.windows(b"TOPLEVEL-META-SECRET".len())
                .any(|w| w == b"TOPLEVEL-META-SECRET"),
            "top-level meta tag leaked (mdat-first)"
        );

        // stco NOT patched: meta sits after mdat, so no shift.
        assert_eq!(
            read_first_stco(&out),
            vec![16, 40, 88],
            "stco changed for a meta dropped after mdat"
        );

        // mdat untouched, top-level now ftyp | mdat | moov.
        assert!(
            out.windows(mdat_payload.len()).any(|w| w == mdat_payload),
            "mdat altered"
        );
        let tl: Vec<[u8; 4]> = top_level(&out).into_iter().map(|(k, _)| k).collect();
        assert_eq!(
            tl,
            vec![*b"ftyp", *b"mdat", *b"moov"],
            "top-level box set wrong"
        );
    }

    #[test]
    fn missing_ftyp_errors() {
        let moov = build_moov(&stco_body(&[10]));
        assert!(strip(&moov).is_err(), "file without ftyp should error");
    }

    // ---- regression: DEFECT 1 (recursion depth -> stack overflow) ----

    /// Build a chain of `count` nested `minf` containers, the innermost
    /// holding `inner`. Each level wraps the previous in an `minf` box.
    fn nested_minf(count: usize, inner: &[u8]) -> Vec<u8> {
        let mut cur = inner.to_vec();
        for _ in 0..count {
            cur = boxed(b"minf", &cur);
        }
        cur
    }

    /// A crafted moov-before-mdat file with pathologically deep nested
    /// `minf` containers under a trak's mdia. Before the fix this drove
    /// `rewrite_container` / `collect_offset_tables` into unbounded
    /// recursion and aborted (SIGABRT) on the wasm guest stack. After the
    /// fix it must return Err (a ParseError), never panic/abort.
    #[test]
    fn deeply_nested_containers_error_not_abort() {
        // Innermost real minf carries an stbl with an stco (so the
        // faststart offset-patch / scan path is fully exercised).
        let stco = stco_body(&[100_000u32]);
        let stbl = boxed(b"stbl", &stco);
        // Wrap stbl in MAX_BOX_DEPTH * many nested minf boxes.
        let deep = nested_minf((MAX_BOX_DEPTH as usize) + 50, &stbl);

        let mdhd = boxed(b"mdhd", &fullbox_body(&[0xBB; 20]));
        let mut mdia_body = Vec::new();
        mdia_body.extend_from_slice(&mdhd);
        mdia_body.extend_from_slice(&deep);
        let mdia = boxed(b"mdia", &mdia_body);

        let tkhd = boxed(b"tkhd", &fullbox_body(&[0xAA; 80]));
        let mut trak_body = Vec::new();
        trak_body.extend_from_slice(&tkhd);
        trak_body.extend_from_slice(&mdia);
        let trak = boxed(b"trak", &trak_body);

        let mvhd = boxed(b"mvhd", &fullbox_body(&[0xCC; 96]));
        // udta forces metadata-drop shrink => faststart patch path engages.
        let gps = boxed(b"\xA9xyz", b"+00.0/GPS");
        let udta = boxed(b"udta", &gps);
        let mut moov_body = Vec::new();
        moov_body.extend_from_slice(&mvhd);
        moov_body.extend_from_slice(&trak);
        moov_body.extend_from_slice(&udta);
        let moov = boxed(b"moov", &moov_body);

        let mdat = boxed(b"mdat", b"RAW-MEDIA");

        let mut input = Vec::new();
        input.extend_from_slice(&ftyp());
        input.extend_from_slice(&moov); // moov FIRST -> faststart offset path
        input.extend_from_slice(&mdat);

        // Must be a clean Err, not a panic/abort. (cargo test catches a
        // panic as a failed test; a real SIGABRT would crash the runner.)
        let res = strip(&input);
        assert!(
            res.is_err(),
            "deeply nested containers must return Err, not abort"
        );
    }

    // ---- regression: DEFECT 2 (mvhd/tkhd/mdhd timestamp leak) ----

    /// Build a version-0 timestamp box (`mvhd`/`tkhd`/`mdhd` share layout)
    /// with the given creation/modification times and a trailing tail of
    /// distinctive bytes that must survive untouched.
    fn ts_box_v0(kind: &[u8; 4], creation: u32, modification: u32, tail: &[u8]) -> Vec<u8> {
        let mut body = vec![0u8, 0, 0, 0]; // version 0 + 3 flags
        body.extend_from_slice(&creation.to_be_bytes());
        body.extend_from_slice(&modification.to_be_bytes());
        body.extend_from_slice(tail);
        boxed(kind, &body)
    }

    /// mvhd, tkhd and mdhd creation_time / modification_time must be zeroed
    /// to match ffmpeg `+bitexact`. Everything else (the tail bytes, the
    /// box count, mdat, stco) must remain intact.
    #[test]
    fn zeroes_header_timestamps_mvhd_tkhd_mdhd() {
        const CREAT: u32 = 0xDEAD_BEEF;
        const MODIF: u32 = 0xCAFE_F00D;
        // Distinctive tails so we can prove the rest of each box survives.
        let mvhd = ts_box_v0(b"mvhd", CREAT, MODIF, &[0xC1; 80]);
        let tkhd = ts_box_v0(b"tkhd", CREAT, MODIF, &[0xA1; 72]);
        let mdhd = ts_box_v0(b"mdhd", CREAT, MODIF, &[0xB1; 12]);

        let stco = stco_body(&[16, 40]);
        let stbl = boxed(b"stbl", &stco);
        let minf = boxed(b"minf", &stbl);
        let mut mdia_body = Vec::new();
        mdia_body.extend_from_slice(&mdhd);
        mdia_body.extend_from_slice(&minf);
        let mdia = boxed(b"mdia", &mdia_body);
        let mut trak_body = Vec::new();
        trak_body.extend_from_slice(&tkhd);
        trak_body.extend_from_slice(&mdia);
        let trak = boxed(b"trak", &trak_body);
        let mut moov_body = Vec::new();
        moov_body.extend_from_slice(&mvhd);
        moov_body.extend_from_slice(&trak);
        let moov = boxed(b"moov", &moov_body);

        let mdat_payload = b"MEDIA-SAMPLES-UNTOUCHED";
        let mdat = boxed(b"mdat", mdat_payload);

        let mut input = Vec::new();
        input.extend_from_slice(&ftyp());
        input.extend_from_slice(&mdat); // mdat first: no offset patching
        input.extend_from_slice(&moov);

        // Sanity: the dirty input carries the non-zero timestamps.
        assert!(
            input.windows(4).any(|w| w == CREAT.to_be_bytes()),
            "creation ts absent from fixture"
        );
        assert!(
            input.windows(4).any(|w| w == MODIF.to_be_bytes()),
            "modification ts absent from fixture"
        );

        let out = strip(&input).unwrap();

        // The timestamps must be zeroed: read them back from each box.
        for kind in [b"mvhd", b"tkhd", b"mdhd"] {
            let body = find_box_body(&out, kind).expect("header box present");
            let version = body[0];
            assert_eq!(version, 0, "version unexpectedly changed for {:?}", kind);
            let creation = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
            let modification = u32::from_be_bytes([body[8], body[9], body[10], body[11]]);
            assert_eq!(creation, 0, "creation_time leaked in {:?}", kind);
            assert_eq!(modification, 0, "modification_time leaked in {:?}", kind);
        }

        // The raw timestamp byte patterns must not appear ANYWHERE in the
        // output (no copy survived in another box).
        assert!(
            !out.windows(4).any(|w| w == CREAT.to_be_bytes()),
            "creation_time bytes still present"
        );
        assert!(
            !out.windows(4).any(|w| w == MODIF.to_be_bytes()),
            "modification_time bytes still present"
        );

        // Everything else intact: tails survive, box count + mdat + stco.
        let mvhd_body = find_box_body(&out, b"mvhd").unwrap();
        assert!(
            mvhd_body[12..].iter().all(|&b| b == 0xC1),
            "mvhd tail altered"
        );
        let tkhd_body = find_box_body(&out, b"tkhd").unwrap();
        assert!(
            tkhd_body[12..].iter().all(|&b| b == 0xA1),
            "tkhd tail altered"
        );
        let mdhd_body = find_box_body(&out, b"mdhd").unwrap();
        assert!(
            mdhd_body[12..].iter().all(|&b| b == 0xB1),
            "mdhd tail altered"
        );

        assert!(
            out.windows(mdat_payload.len()).any(|w| w == mdat_payload),
            "mdat altered"
        );
        assert_eq!(read_first_stco(&out), vec![16, 40], "stco entries changed");
        let tl: Vec<[u8; 4]> = top_level(&out).into_iter().map(|(k, _)| k).collect();
        assert_eq!(
            tl,
            vec![*b"ftyp", *b"mdat", *b"moov"],
            "top-level box set changed"
        );
    }

    /// Version-1 (64-bit) timestamps must also be zeroed at the correct
    /// offsets (creation @ body+4 u64, modification @ body+12 u64).
    #[test]
    fn zeroes_header_timestamps_version1() {
        // version 1 + 3 flags, then u64 creation, u64 modification, tail.
        let mut body = vec![1u8, 0, 0, 0];
        body.extend_from_slice(&0x0102_0304_0506_0708u64.to_be_bytes()); // creation
        body.extend_from_slice(&0x1112_1314_1516_1718u64.to_be_bytes()); // modification
        body.extend_from_slice(&[0xE7; 24]); // tail (e.g. timescale/duration)
        let mvhd = boxed(b"mvhd", &body);

        let moov = boxed(b"moov", &mvhd);
        let mdat = boxed(b"mdat", b"PAYLOAD");
        let mut input = Vec::new();
        input.extend_from_slice(&ftyp());
        input.extend_from_slice(&mdat);
        input.extend_from_slice(&moov);

        let out = strip(&input).unwrap();
        let b = find_box_body(&out, b"mvhd").unwrap();
        assert_eq!(b[0], 1, "version changed");
        // creation @ 4..12, modification @ 12..20 must all be zero.
        assert!(b[4..20].iter().all(|&x| x == 0), "v1 timestamps not zeroed");
        // tail (20..) untouched.
        assert!(b[20..].iter().all(|&x| x == 0xE7), "v1 tail altered");
    }

    // ---- test helpers to read back chunk-offset tables ----

    fn find_box_body<'a>(data: &'a [u8], target: &[u8; 4]) -> Option<&'a [u8]> {
        fn walk<'a>(data: &'a [u8], target: &[u8; 4]) -> Option<&'a [u8]> {
            let mut cur = Cursor::new(data);
            while (cur.position() as usize) < data.len() {
                let start = cur.position() as usize;
                let h = Header::decode(&mut cur).ok()?;
                let hlen = cur.position() as usize - start;
                let total = match h.size {
                    Some(s) => hlen + s,
                    None => data.len() - start,
                };
                let kind4: [u8; 4] = h.kind.into();
                let body = &data[start + hlen..start + total];
                if &kind4 == target {
                    return Some(body);
                }
                if matches!(&kind4, b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl") {
                    if let Some(b) = walk(body, target) {
                        return Some(b);
                    }
                }
                cur.set_position((start + total) as u64);
            }
            None
        }
        walk(data, target)
    }

    fn read_first_stco(data: &[u8]) -> Vec<u32> {
        let body = find_box_body(data, b"stco").expect("stco present");
        let count = u32::from_be_bytes([body[4], body[5], body[6], body[7]]) as usize;
        let mut out = Vec::new();
        for i in 0..count {
            let off = 8 + i * 4;
            out.push(u32::from_be_bytes([
                body[off],
                body[off + 1],
                body[off + 2],
                body[off + 3],
            ]));
        }
        out
    }

    fn read_first_co64(data: &[u8]) -> Vec<u64> {
        let body = find_box_body(data, b"co64").expect("co64 present");
        let count = u32::from_be_bytes([body[4], body[5], body[6], body[7]]) as usize;
        let mut out = Vec::new();
        for i in 0..count {
            let off = 8 + i * 8;
            let mut b = [0u8; 8];
            b.copy_from_slice(&body[off..off + 8]);
            out.push(u64::from_be_bytes(b));
        }
        out
    }
}
