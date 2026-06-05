// Pure-Rust, fully in-memory Ogg metadata stripper.
//
// The native ffmpeg path runs
// `ffmpeg -i in -map 0 -c copy -map_metadata -1 -map_chapters -1
//  -disposition 0 -fflags +bitexact out`, i.e. a metadata-only strip +
// remux that keeps every codec packet byte-for-byte and drops all
// container / global / track tags. For Ogg the only tag-carrying surface
// is the per-logical-stream comment header (the SECOND header packet of
// each stream), so this module rewrites every comment header in place:
// it blanks the vendor string (`vendor_len = 0`) and drops all user
// comments (`count = 0`), keeping the codec identification + setup
// headers and every audio/video data packet untouched.
//
// The comment header packet is NOT assumed to fit in one page. A large
// comment header (e.g. a `METADATA_BLOCK_PICTURE` cover art > 255 bytes)
// is spec-legal and routinely spans multiple pages: the packet continues
// while a page's last lacing value is 255, and each continuation page
// carries the `header_type` continuation bit (0x01). This stripper
// reassembles the full comment packet across however many pages it spans
// (per logical stream / serial), rewrites it (blank vendor, zero
// comments, codec framing preserved), then RE-SEGMENTS the now-shorter
// packet and rebuilds the affected pages: correct segment table, body,
// num_segments, header_type continuation bits, and CRC32. Because the
// stripped packet is smaller, it can occupy fewer pages than before; when
// that happens the trailing pages of the same serial are renumbered so
// `page_sequence` stays gapless, matching the native path.
//
// All other packets / pages are copied byte-for-byte.
//
// Handled comment headers: Vorbis (`0x03 "vorbis"`), Opus (`"OpusTags"`),
// Theora (`0x81 "theora"`). See the codec-coverage note in the report.
//
// Everything read from the file (page header fields, segment lacing,
// comment-header lengths) is bounds-checked with checked arithmetic; a
// malformed input yields [`CoreError::ParseError`], never a panic or an
// unbounded allocation.

use crate::error::CoreError;

/// Ogg page capture pattern.
const CAPTURE: &[u8; 4] = b"OggS";
/// Fixed size of an Ogg page header up to (and including) the
/// `num_segments` byte: `OggS`(4) + version(1) + header_type(1) +
/// granule(8) + serial(4) + page_seq(4) + crc(4) + num_segments(1).
const PAGE_HEADER_FIXED: usize = 27;
/// Byte offset of the 4-byte little-endian page-sequence field.
const SEQ_OFFSET: usize = 18;
/// Byte offset of the 4-byte little-endian CRC field within a page header.
const CRC_OFFSET: usize = 22;
/// Byte offset of the `num_segments` field within a page header.
const NUM_SEGMENTS_OFFSET: usize = 26;
/// Maximum lacing values a single Ogg page may carry.
const MAX_SEGMENTS_PER_PAGE: usize = 255;

/// A parsed Ogg page: the slice bounds of its full header (capture
/// pattern through the segment table) and of its payload body.
struct Page {
    /// Offset of the page's first byte (the `O` of `OggS`).
    start: usize,
    /// Number of lacing values in this page's segment table.
    num_segments: usize,
    /// Offset of the payload body (just past the segment table).
    body_start: usize,
    /// Length of the payload body in bytes.
    body_len: usize,
    /// `header_type` flags byte (bit 0 continued, bit 1 BOS, bit 2 EOS).
    header_type: u8,
    /// Logical bitstream serial number.
    serial: u32,
}

fn parse_err(detail: impl Into<String>) -> CoreError {
    CoreError::ParseError {
        path: std::path::PathBuf::new(),
        detail: detail.into(),
    }
}

/// Parse a single Ogg page starting at `pos`. Returns the page and the
/// offset of the byte just past its body.
fn parse_page(input: &[u8], pos: usize) -> Result<(Page, usize), CoreError> {
    let header_end = pos
        .checked_add(PAGE_HEADER_FIXED)
        .ok_or_else(|| parse_err("page header offset overflow"))?;
    if header_end > input.len() {
        return Err(parse_err("truncated Ogg page header"));
    }
    let capture = input
        .get(
            pos..pos
                .checked_add(4)
                .ok_or_else(|| parse_err("capture overflow"))?,
        )
        .ok_or_else(|| parse_err("missing capture pattern"))?;
    if capture != CAPTURE {
        return Err(parse_err("bad Ogg capture pattern (expected 'OggS')"));
    }
    let version = *input
        .get(
            pos.checked_add(4)
                .ok_or_else(|| parse_err("offset overflow"))?,
        )
        .ok_or_else(|| parse_err("missing version byte"))?;
    if version != 0 {
        return Err(parse_err("unsupported Ogg page version"));
    }
    let header_type = *input
        .get(
            pos.checked_add(5)
                .ok_or_else(|| parse_err("offset overflow"))?,
        )
        .ok_or_else(|| parse_err("missing header_type byte"))?;
    let serial = read_u32_le(
        input,
        pos.checked_add(14)
            .ok_or_else(|| parse_err("offset overflow"))?,
    )?;

    let num_segments = usize::from(
        *input
            .get(
                pos.checked_add(NUM_SEGMENTS_OFFSET)
                    .ok_or_else(|| parse_err("offset overflow"))?,
            )
            .ok_or_else(|| parse_err("missing num_segments"))?,
    );
    let seg_table_start = header_end;
    let seg_table_end = seg_table_start
        .checked_add(num_segments)
        .ok_or_else(|| parse_err("segment table offset overflow"))?;
    if seg_table_end > input.len() {
        return Err(parse_err("truncated Ogg segment table"));
    }
    // Body length = sum of the lacing values (each 0..=255).
    let mut body_len: usize = 0;
    for i in seg_table_start..seg_table_end {
        let lacing = usize::from(
            *input
                .get(i)
                .ok_or_else(|| parse_err("segment table read out of range"))?,
        );
        body_len = body_len
            .checked_add(lacing)
            .ok_or_else(|| parse_err("page body length overflow"))?;
    }
    let body_start = seg_table_end;
    let body_end = body_start
        .checked_add(body_len)
        .ok_or_else(|| parse_err("page body offset overflow"))?;
    if body_end > input.len() {
        return Err(parse_err("page body overruns input"));
    }
    Ok((
        Page {
            start: pos,
            num_segments,
            body_start,
            body_len,
            header_type,
            serial,
        },
        body_end,
    ))
}

/// Read a little-endian `u32` at `off`, bounds-checked.
fn read_u32_le(input: &[u8], off: usize) -> Result<u32, CoreError> {
    let end = off
        .checked_add(4)
        .ok_or_else(|| parse_err("u32 offset overflow"))?;
    let bytes = input
        .get(off..end)
        .ok_or_else(|| parse_err("u32 read out of range"))?;
    let arr: [u8; 4] = bytes.try_into().map_err(|_| parse_err("u32 slice size"))?;
    Ok(u32::from_le_bytes(arr))
}

/// Identify whether a comment-header payload belongs to a known codec and
/// return the byte offset at which the Vorbis-comment structure (the
/// `[u32 vendor_len][vendor][u32 count]...`) begins. `None` means this is
/// not a comment header we rewrite.
fn comment_header_offset(payload: &[u8]) -> Option<usize> {
    // Vorbis: 0x03 "vorbis"
    if payload.starts_with(b"\x03vorbis") {
        return Some(7);
    }
    // Theora: 0x81 "theora"
    if payload.starts_with(b"\x81theora") {
        return Some(7);
    }
    // Opus: "OpusTags"
    if payload.starts_with(b"OpusTags") {
        return Some(8);
    }
    None
}

/// Given a comment-header payload, build a stripped replacement: the same
/// codec signature, an empty vendor string, and zero user comments.
/// Returns the new payload, or `None` if `payload` is not a rewritable
/// comment header.
///
/// The output preserves the leading codec signature, then writes
/// `vendor_len = 0` and `count = 0`. For Vorbis the framing bit (a
/// trailing `0x01`) is re-emitted so the packet stays spec-valid; Opus
/// and Theora comment packets have no framing bit.
fn strip_comment_payload(payload: &[u8]) -> Result<Option<Vec<u8>>, CoreError> {
    let Some(sig_len) = comment_header_offset(payload) else {
        return Ok(None);
    };
    let is_vorbis = payload.starts_with(b"\x03vorbis");

    // Validate the existing structure so we only rewrite well-formed
    // comment headers (and so a truncated/garbage one is reported, not
    // silently mangled).
    let vendor_len = read_u32_le(payload, sig_len)? as usize;
    let after_vendor = sig_len
        .checked_add(4)
        .and_then(|v| v.checked_add(vendor_len))
        .ok_or_else(|| parse_err("comment vendor length overflow"))?;
    let count = read_u32_le(payload, after_vendor)? as usize;
    let mut cursor = after_vendor
        .checked_add(4)
        .ok_or_else(|| parse_err("comment count offset overflow"))?;
    for _ in 0..count {
        let clen = read_u32_le(payload, cursor)? as usize;
        cursor = cursor
            .checked_add(4)
            .and_then(|v| v.checked_add(clen))
            .ok_or_else(|| parse_err("comment entry length overflow"))?;
        if cursor > payload.len() {
            return Err(parse_err("comment entry overruns header"));
        }
    }
    // Vorbis carries a trailing framing bit (lsb set) after the comment
    // list. Confirm it exists where the spec requires it; if the byte is
    // missing the header is malformed.
    if is_vorbis && cursor >= payload.len() {
        return Err(parse_err("vorbis comment header missing framing bit"));
    }

    // Build the stripped payload: signature + vendor_len(0) + count(0)
    // (+ framing bit for vorbis).
    let sig = payload
        .get(..sig_len)
        .ok_or_else(|| parse_err("signature slice"))?;
    let mut out = Vec::with_capacity(sig_len + 8 + usize::from(is_vorbis));
    out.extend_from_slice(sig);
    out.extend_from_slice(&0u32.to_le_bytes()); // vendor_len = 0
    out.extend_from_slice(&0u32.to_le_bytes()); // count = 0
    if is_vorbis {
        out.push(0x01); // framing bit
    }
    Ok(Some(out))
}

/// Build a segment (lacing) table for a packet of `packet_len` bytes that
/// is fully contained in one page. Each lacing value is at most 255; a
/// packet whose length is an exact multiple of 255 needs a trailing 0
/// lacing value to mark its end. Returns the lacing bytes.
fn lacing_for_packet(packet_len: usize) -> Vec<u8> {
    let mut table = Vec::new();
    let mut remaining = packet_len;
    while remaining >= 255 {
        table.push(255);
        remaining -= 255;
    }
    table.push(remaining as u8);
    table
}

/// Ogg CRC32 lookup table (poly 0x04C11DB7, MSB-first, no reflection).
/// Computed at first use.
const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        // `i` fits in u8; shift into the top byte.
        let mut crc = (i as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

/// Compute the Ogg page CRC over `data` (which must already have its CRC
/// field zeroed). Poly 0x04C11DB7, init 0, no in/out reflection, no final
/// xor.
fn ogg_crc(data: &[u8]) -> u32 {
    let table = crc_table();
    let mut crc: u32 = 0;
    for &b in data {
        let idx = ((crc >> 24) ^ u32::from(b)) & 0xff;
        crc = (crc << 8) ^ table[idx as usize];
    }
    crc
}

/// A fully parsed page plus the byte ranges, within the original input,
/// of every page segment so we can reconstruct packets across pages.
struct ParsedPage {
    page: Page,
    /// Offset of the page's first byte (start) and one-past-its-last byte
    /// (so the whole page is `input[start..end]`).
    end: usize,
    /// The page's lacing values (segment table), owned for convenience.
    lacing: Vec<u8>,
}

/// A single rebuilt or copied page in the output, carrying its own serial
/// so trailing pages can be renumbered per-serial after a rewrite.
struct OutPage {
    serial: u32,
    bytes: Vec<u8>,
    /// When `true` this slot was a surplus original page of a serial whose
    /// stripped comment packet now fits in fewer pages; it is dropped from
    /// the final output.
    removed: bool,
}

/// Strip every comment header from an Ogg stream, in memory.
///
/// Mirrors the native ffmpeg metadata-only strip: codec setup + data
/// packets are preserved byte-for-byte; only the per-stream comment
/// header packet is rewritten (empty vendor, zero comments). The comment
/// packet is reassembled across however many pages it spans, stripped,
/// re-segmented, and the affected pages rebuilt (segment table, body,
/// continuation bits, CRC). If the stripped packet fits in fewer pages
/// than the original, the trailing pages of that serial are renumbered so
/// `page_sequence` stays gapless.
///
/// # Errors
///
/// Returns [`CoreError::ParseError`] for any structurally invalid Ogg
/// input (bad capture pattern, truncated header / segment table / body,
/// malformed comment header, a comment packet that never terminates).
pub(crate) fn strip(input: &[u8]) -> Result<Vec<u8>, CoreError> {
    crate::handlers::check_input_len(input.len())?;
    if input.len() < PAGE_HEADER_FIXED || input.get(..4) != Some(&CAPTURE[..]) {
        return Err(parse_err("not an Ogg stream (missing 'OggS' capture)"));
    }

    // Phase 1: parse every page into an ordered list.
    let mut pages: Vec<ParsedPage> = Vec::new();
    let mut pos = 0usize;
    while pos < input.len() {
        let (page, next) = parse_page(input, pos)?;
        let seg_start = page
            .body_start
            .checked_sub(page.num_segments)
            .ok_or_else(|| parse_err("seg table underflow"))?;
        let lacing = input
            .get(seg_start..page.body_start)
            .ok_or_else(|| parse_err("segment table slice"))?
            .to_vec();
        pages.push(ParsedPage {
            page,
            end: next,
            lacing,
        });
        pos = next;
    }

    // Phase 2: for each serial, find the comment packet (per-serial packet
    // index 1) and the contiguous run of pages it spans, then identify the
    // page indices to rewrite. We process serials independently.
    use std::collections::BTreeMap;
    // serial -> ordered list of page indices (in `pages`) for that serial.
    let mut by_serial: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (idx, pp) in pages.iter().enumerate() {
        by_serial.entry(pp.page.serial).or_default().push(idx);
    }

    // Output pages, in original file order. We build a parallel vector and
    // serialize at the end so renumbering is easy.
    let mut out_pages: Vec<OutPage> = pages
        .iter()
        .map(|pp| {
            Ok(OutPage {
                serial: pp.page.serial,
                bytes: input
                    .get(pp.page.start..pp.end)
                    .ok_or_else(|| parse_err("page slice"))?
                    .to_vec(),
                removed: false,
            })
        })
        .collect::<Result<Vec<_>, CoreError>>()?;

    for page_idxs in by_serial.values() {
        rewrite_comment_for_serial(input, &pages, page_idxs, &mut out_pages)?;
    }

    // Phase 4: renumber page sequences per serial so they stay gapless.
    // (A serial whose pages were never touched keeps its original numbers
    // because the count is unchanged and the originals were gapless; we
    // renumber unconditionally to be safe and spec-exact.)
    let mut seq_counter: BTreeMap<u32, u32> = BTreeMap::new();
    for op in &mut out_pages {
        if op.removed {
            continue;
        }
        let seq = seq_counter.entry(op.serial).or_insert(0);
        let want = *seq;
        *seq = seq
            .checked_add(1)
            .ok_or_else(|| parse_err("page sequence overflow"))?;
        set_page_sequence(&mut op.bytes, want)?;
    }

    // Phase 5: concatenate (dropping removed slots).
    let mut out = Vec::with_capacity(input.len());
    for op in &out_pages {
        if op.removed {
            continue;
        }
        out.extend_from_slice(&op.bytes);
    }
    Ok(out)
}

/// Locate and rewrite the comment packet (per-serial packet index 1) for a
/// single logical stream, replacing the affected entries in `out_pages`.
///
/// `page_idxs` are the indices (into `pages` / `out_pages`) of this
/// serial's pages, in stream order. If no comment packet is present, or it
/// is not a codec we rewrite, `out_pages` is left untouched for this
/// serial.
fn rewrite_comment_for_serial(
    input: &[u8],
    pages: &[ParsedPage],
    page_idxs: &[usize],
    out_pages: &mut [OutPage],
) -> Result<(), CoreError> {
    // Walk this serial's segments in stream order, splitting into packets.
    // We record, per packet, the list of (page_idx, segment_index_in_page,
    // lacing_value) it consumed, so we can later carve up bodies. We only
    // need to fully resolve packet 0 (id header) and packet 1 (comment).
    //
    // A packet terminates at the first lacing value < 255. A packet that
    // is still open at end-of-stream (last lacing == 255 and no more
    // pages) is malformed.
    let mut packet_index: usize = 0;
    // Segments accumulated for the currently-open packet.
    let mut cur_segments: Vec<(usize, usize)> = Vec::new(); // (page_idx, seg_idx)
    // Page indices that the comment packet (index 1) spans, and the
    // page-local segment ranges it consumes within each.
    let mut comment_segments: Option<Vec<(usize, usize)>> = None;

    'walk: for &pi in page_idxs {
        let pp = pages
            .get(pi)
            .ok_or_else(|| parse_err("page index out of range"))?;
        for (si, &lacing) in pp.lacing.iter().enumerate() {
            cur_segments.push((pi, si));
            if lacing < 255 {
                // Packet `packet_index` terminates here.
                if packet_index == 1 {
                    comment_segments = Some(std::mem::take(&mut cur_segments));
                    break 'walk;
                }
                cur_segments.clear();
                packet_index = packet_index
                    .checked_add(1)
                    .ok_or_else(|| parse_err("packet index overflow"))?;
            }
        }
    }

    let Some(comment_segments) = comment_segments else {
        // Either fewer than two packets exist for this serial, or the
        // comment packet never terminated. If `cur_segments` is non-empty
        // and we were mid-comment-packet, that is a malformed (unbounded)
        // packet.
        if packet_index == 1 && !cur_segments.is_empty() {
            return Err(parse_err("comment packet never terminates"));
        }
        return Ok(());
    };

    // Reassemble the comment packet bytes by concatenating its segments'
    // body bytes across pages. We also remember, for the LAST page the
    // comment packet touches, the trailing segments (other packets) so we
    // can re-emit them verbatim, and the FIRST page index where the comment
    // packet starts.
    let first_seg = *comment_segments
        .first()
        .ok_or_else(|| parse_err("empty comment segment list"))?;
    let last_seg = *comment_segments
        .last()
        .ok_or_else(|| parse_err("empty comment segment list"))?;
    let first_page_idx = first_seg.0;
    let last_page_idx = last_seg.0;

    // The comment packet must NOT begin as a continuation: it is a fresh
    // packet on its first page. (If the id header somehow continued into
    // this page we would have a different packet 0 split; the walk above
    // guarantees packet 1 starts a fresh segment run.)

    // Build the original comment packet bytes.
    let mut packet = Vec::new();
    for &(pi, si) in &comment_segments {
        let pp = pages
            .get(pi)
            .ok_or_else(|| parse_err("page index out of range"))?;
        let (off, len) = segment_body_range(pp, si)?;
        let slice = input
            .get(off..off.checked_add(len).ok_or_else(|| parse_err("seg overflow"))?)
            .ok_or_else(|| parse_err("segment body slice"))?;
        packet.extend_from_slice(slice);
    }

    let Some(stripped) = strip_comment_payload(&packet)? else {
        // Not a codec we rewrite (unknown second packet). Leave verbatim.
        return Ok(());
    };

    // Determine the leading segments on the FIRST page that precede the
    // comment packet (i.e. earlier packets sharing that page), and the
    // trailing segments on the LAST page that follow the comment packet.
    let first_pp = pages
        .get(first_page_idx)
        .ok_or_else(|| parse_err("first page index"))?;
    let last_pp = pages
        .get(last_page_idx)
        .ok_or_else(|| parse_err("last page index"))?;

    let lead_segs = first_seg.1; // segments before the comment packet on its first page.
    let trail_seg_start = last_seg
        .1
        .checked_add(1)
        .ok_or_else(|| parse_err("trail seg overflow"))?;

    // Bytes + lacing of the first page's body that precede the comment
    // packet (whole packets that shared the first page, copied verbatim).
    let lead_bytes = page_body_prefix(input, first_pp, lead_segs)?;
    let lead_lacing = first_pp
        .lacing
        .get(..lead_segs)
        .ok_or_else(|| parse_err("lead lacing slice"))?
        .to_vec();
    // header_type of the first page (BOS / continuation bits) preserved.
    let first_header_type = first_pp.page.header_type;
    // Bytes + lacing of the last page's body that follow the comment packet.
    let trail_bytes = page_body_suffix(input, last_pp, trail_seg_start)?;
    let trail_lacing = last_pp
        .lacing
        .get(trail_seg_start..)
        .ok_or_else(|| parse_err("trail lacing slice"))?
        .to_vec();
    let last_header_type = last_pp.page.header_type;

    // Granule of the original last page of the span (the page where the
    // trailing packets, if any, completed).
    let last_granule = read_u64_le(
        input,
        last_pp
            .page
            .start
            .checked_add(6)
            .ok_or_else(|| parse_err("granule off"))?,
    )?;

    // Now build the replacement page(s). The comment packet occupies pages
    // [first_page_idx ..= last_page_idx]. We rebuild it as a (usually
    // smaller) set of pages, reusing the first page's header for the first
    // rebuilt page and the last page's EOS/granule for the last rebuilt
    // page, then mark removed pages for deletion and renumber later.
    let rebuilt = build_comment_pages(
        first_pp.page.serial,
        first_header_type,
        last_header_type,
        last_granule,
        &lead_lacing,
        &lead_bytes,
        &stripped,
        &trail_lacing,
        &trail_bytes,
    )?;

    // Replace the span [first_page_idx ..= last_page_idx] in `out_pages`
    // with `rebuilt`. Since `out_pages` is index-aligned with `pages`, and
    // the span may shrink, we splice: set the first slot to the first
    // rebuilt page, fill following slots, and mark any leftover original
    // pages in the span as removed (empty bytes -> dropped at the end).
    //
    // To keep indices stable for other serials we do NOT actually remove
    // vector elements; we instead replace covered pages with the rebuilt
    // ones in order, and zero out any surplus original pages (marking them
    // removed). Because page_idxs for a serial are contiguous within that
    // serial's stream but interleaved in the file with other serials, the
    // span [first_page_idx..=last_page_idx] could in principle contain
    // pages of OTHER serials. Ogg interleaving allows that, so we must map
    // rebuilt pages onto exactly THIS serial's page slots within the span.
    let serial_span: Vec<usize> = page_idxs
        .iter()
        .copied()
        .filter(|&i| i >= first_page_idx && i <= last_page_idx)
        .collect();
    if rebuilt.len() > serial_span.len() {
        // Should never happen: stripping only shrinks the packet, so it
        // cannot need more pages than the original spanned.
        return Err(parse_err("rewritten comment needs more pages than original"));
    }
    let rebuilt_count = rebuilt.len();
    for (slot, bytes) in serial_span.iter().zip(rebuilt) {
        let op = out_pages
            .get_mut(*slot)
            .ok_or_else(|| parse_err("out page slot"))?;
        op.bytes = bytes;
    }
    // Any surplus original slots of this serial in the span are dropped.
    for &slot in serial_span.iter().skip(rebuilt_count) {
        let op = out_pages
            .get_mut(slot)
            .ok_or_else(|| parse_err("out page slot"))?;
        op.removed = true;
        op.bytes = Vec::new();
    }
    Ok(())
}

/// Compute the (offset, len) within `input` of segment `si`'s body bytes
/// on page `pp`.
fn segment_body_range(pp: &ParsedPage, si: usize) -> Result<(usize, usize), CoreError> {
    let mut off = pp.page.body_start;
    for j in 0..si {
        let l = usize::from(
            *pp.lacing
                .get(j)
                .ok_or_else(|| parse_err("lacing index"))?,
        );
        off = off
            .checked_add(l)
            .ok_or_else(|| parse_err("segment offset overflow"))?;
    }
    let len = usize::from(
        *pp.lacing
            .get(si)
            .ok_or_else(|| parse_err("lacing index"))?,
    );
    Ok((off, len))
}

/// Bytes of `pp`'s body that belong to the first `count` segments.
fn page_body_prefix(input: &[u8], pp: &ParsedPage, count: usize) -> Result<Vec<u8>, CoreError> {
    let mut total = 0usize;
    for j in 0..count {
        total = total
            .checked_add(usize::from(
                *pp.lacing
                    .get(j)
                    .ok_or_else(|| parse_err("lacing index"))?,
            ))
            .ok_or_else(|| parse_err("prefix len overflow"))?;
    }
    let end = pp
        .page
        .body_start
        .checked_add(total)
        .ok_or_else(|| parse_err("prefix end overflow"))?;
    Ok(input
        .get(pp.page.body_start..end)
        .ok_or_else(|| parse_err("prefix slice"))?
        .to_vec())
}

/// Bytes of `pp`'s body that belong to segments `start..`.
fn page_body_suffix(input: &[u8], pp: &ParsedPage, start: usize) -> Result<Vec<u8>, CoreError> {
    let mut before = 0usize;
    for j in 0..start {
        before = before
            .checked_add(usize::from(
                *pp.lacing
                    .get(j)
                    .ok_or_else(|| parse_err("lacing index"))?,
            ))
            .ok_or_else(|| parse_err("suffix prefix overflow"))?;
    }
    let body_off = pp
        .page
        .body_start
        .checked_add(before)
        .ok_or_else(|| parse_err("suffix off overflow"))?;
    let body_end = pp
        .page
        .body_start
        .checked_add(pp.page.body_len)
        .ok_or_else(|| parse_err("suffix end overflow"))?;
    Ok(input
        .get(body_off..body_end)
        .ok_or_else(|| parse_err("suffix slice"))?
        .to_vec())
}

/// Build the replacement page(s) for the comment packet. Layout per page:
/// `lead_bytes` (only on the first page, preceding packets that shared the
/// first page), then the comment packet body, then `trail_bytes` (only on
/// the last page, packets that followed the comment on the last page).
///
/// The comment packet plus its lead are laced together; the result is split
/// across pages of at most 255 segments. The first page keeps the original
/// first page's `header_type` (preserving any continuation bit for a
/// preceding partial packet, plus its BOS); continuation pages get the
/// continuation bit. The final rebuilt page keeps the original last page's
/// EOS bit + granule; intermediate pages carry granule -1 (0xFFFF...) per
/// the Ogg convention that a page completing no packet has no granule.
#[allow(clippy::too_many_arguments)]
fn build_comment_pages(
    serial: u32,
    first_header_type: u8,
    last_header_type: u8,
    last_granule: u64,
    lead_lacing: &[u8],
    lead_bytes: &[u8],
    stripped: &[u8],
    trail_lacing: &[u8],
    trail_bytes: &[u8],
) -> Result<Vec<Vec<u8>>, CoreError> {
    // The comment packet's own lacing (from scratch).
    let comment_lacing = lacing_for_packet(stripped.len());

    // Compose the combined lacing + body for: lead, comment, trail. The
    // lacing values themselves encode every packet boundary, so re-paginating
    // this combined table (greedily, up to 255 segments per page) preserves
    // all packet boundaries exactly while the stripped (smaller) comment
    // packet lets the whole run fit in <= the original page count.
    let mut all_lacing: Vec<u8> = Vec::new();
    all_lacing.extend_from_slice(lead_lacing);
    all_lacing.extend_from_slice(&comment_lacing);
    all_lacing.extend_from_slice(trail_lacing);

    let mut all_body: Vec<u8> = Vec::new();
    all_body.extend_from_slice(lead_bytes);
    all_body.extend_from_slice(stripped);
    all_body.extend_from_slice(trail_bytes);

    // Sanity: total body must equal sum of lacing.
    let lacing_sum: usize = all_lacing
        .iter()
        .try_fold(0usize, |a, &l| a.checked_add(usize::from(l)))
        .ok_or_else(|| parse_err("lacing sum overflow"))?;
    if lacing_sum != all_body.len() {
        return Err(parse_err("internal lacing/body mismatch"));
    }

    // Split `all_lacing` into pages of at most 255 segments. Each page's
    // body is the concatenation of its segments' bytes.
    let total_segs = all_lacing.len();
    let mut pages_out: Vec<Vec<u8>> = Vec::new();
    let mut seg_cursor = 0usize;
    let mut body_cursor = 0usize;
    let mut page_no = 0usize;
    while seg_cursor < total_segs || (total_segs == 0 && pages_out.is_empty()) {
        let remaining = total_segs
            .checked_sub(seg_cursor)
            .ok_or_else(|| parse_err("seg remaining underflow"))?;
        let segs_this = remaining.min(MAX_SEGMENTS_PER_PAGE);
        let seg_end = seg_cursor
            .checked_add(segs_this)
            .ok_or_else(|| parse_err("seg slice overflow"))?;
        let seg_slice = all_lacing
            .get(seg_cursor..seg_end)
            .ok_or_else(|| parse_err("page seg slice"))?;
        let body_this: usize = seg_slice
            .iter()
            .try_fold(0usize, |a, &l| a.checked_add(usize::from(l)))
            .ok_or_else(|| parse_err("page body sum overflow"))?;
        let body_end = body_cursor
            .checked_add(body_this)
            .ok_or_else(|| parse_err("page body end overflow"))?;
        let body_slice = all_body
            .get(body_cursor..body_end)
            .ok_or_else(|| parse_err("page body region"))?;

        let is_first = page_no == 0;
        let is_last = seg_end >= total_segs;

        // header_type: the first rebuilt page keeps the original first
        // page's non-EOS bits (its BOS + any continuation bit it carried for
        // a packet continued from an EARLIER page); subsequent rebuilt pages
        // are continuations of the comment packet (0x01). The EOS bit is
        // carried only onto the last rebuilt page, iff the original last
        // page had it.
        let mut header_type = if is_first {
            first_header_type & !0x04
        } else {
            0x01
        };
        if is_last {
            header_type |= last_header_type & 0x04;
        }

        // granule: the last rebuilt page carries the original last page's
        // granule (the page where the trailing packets complete); any
        // earlier rebuilt page completes no audio packet and uses -1
        // (0xFFFF...), the Ogg "no granule yet" convention. The single-page
        // case (is_first && is_last) therefore also gets last_granule.
        let granule = if is_last { last_granule } else { u64::MAX };

        let page_bytes = assemble_page(
            header_type,
            granule,
            serial,
            0, // sequence set later in the renumber phase
            seg_slice,
            body_slice,
        )?;
        pages_out.push(page_bytes);

        seg_cursor = seg_end;
        body_cursor = body_end;
        page_no = page_no
            .checked_add(1)
            .ok_or_else(|| parse_err("page no overflow"))?;
        if total_segs == 0 {
            break;
        }
    }

    Ok(pages_out)
}

/// Assemble one Ogg page from explicit fields, leaving the CRC computed and
/// the sequence number as given.
fn assemble_page(
    header_type: u8,
    granule: u64,
    serial: u32,
    sequence: u32,
    lacing: &[u8],
    body: &[u8],
) -> Result<Vec<u8>, CoreError> {
    if lacing.len() > MAX_SEGMENTS_PER_PAGE {
        return Err(parse_err("page exceeds 255 segments"));
    }
    let mut out = Vec::with_capacity(PAGE_HEADER_FIXED + lacing.len() + body.len());
    out.extend_from_slice(CAPTURE);
    out.push(0); // version
    out.push(header_type);
    out.extend_from_slice(&granule.to_le_bytes());
    out.extend_from_slice(&serial.to_le_bytes());
    out.extend_from_slice(&sequence.to_le_bytes());
    out.extend_from_slice(&[0u8; 4]); // crc placeholder
    out.push(u8::try_from(lacing.len()).map_err(|_| parse_err("num_segments too large"))?);
    out.extend_from_slice(lacing);
    out.extend_from_slice(body);
    let crc = ogg_crc(&out);
    let crc_bytes = crc.to_le_bytes();
    for (i, b) in crc_bytes.iter().enumerate() {
        let slot = CRC_OFFSET
            .checked_add(i)
            .ok_or_else(|| parse_err("crc write overflow"))?;
        *out.get_mut(slot)
            .ok_or_else(|| parse_err("crc write slot"))? = *b;
    }
    Ok(out)
}

/// Overwrite the page-sequence field of a serialized page and recompute its
/// CRC. No-op-safe: if the sequence is already `seq` the bytes are
/// unchanged but the CRC is still recomputed (cheap, and keeps the field
/// authoritative).
fn set_page_sequence(page: &mut [u8], seq: u32) -> Result<(), CoreError> {
    let seq_bytes = seq.to_le_bytes();
    for (i, b) in seq_bytes.iter().enumerate() {
        let slot = SEQ_OFFSET
            .checked_add(i)
            .ok_or_else(|| parse_err("seq write overflow"))?;
        *page.get_mut(slot).ok_or_else(|| parse_err("seq slot"))? = *b;
    }
    // Recompute CRC over the page with the CRC field zeroed.
    for off in CRC_OFFSET..CRC_OFFSET.checked_add(4).ok_or_else(|| parse_err("crc range"))? {
        *page.get_mut(off).ok_or_else(|| parse_err("crc zero slot"))? = 0;
    }
    let crc = ogg_crc(page);
    let crc_bytes = crc.to_le_bytes();
    for (i, b) in crc_bytes.iter().enumerate() {
        let slot = CRC_OFFSET
            .checked_add(i)
            .ok_or_else(|| parse_err("crc rewrite overflow"))?;
        *page.get_mut(slot).ok_or_else(|| parse_err("crc rewrite slot"))? = *b;
    }
    Ok(())
}

/// Read a little-endian `u64` at `off`, bounds-checked.
fn read_u64_le(input: &[u8], off: usize) -> Result<u64, CoreError> {
    let end = off
        .checked_add(8)
        .ok_or_else(|| parse_err("u64 offset overflow"))?;
    let bytes = input
        .get(off..end)
        .ok_or_else(|| parse_err("u64 read out of range"))?;
    let arr: [u8; 8] = bytes.try_into().map_err(|_| parse_err("u64 slice size"))?;
    Ok(u64::from_le_bytes(arr))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Build one Ogg page from a single self-contained packet, with a
    /// correct CRC so the fixture is a valid page to begin with.
    fn build_test_page(
        header_type: u8,
        granule: u64,
        serial: u32,
        seq: u32,
        packet: &[u8],
    ) -> Vec<u8> {
        let lacing = lacing_for_packet(packet.len());
        let mut page = Vec::new();
        page.extend_from_slice(CAPTURE);
        page.push(0); // version
        page.push(header_type);
        page.extend_from_slice(&granule.to_le_bytes());
        page.extend_from_slice(&serial.to_le_bytes());
        page.extend_from_slice(&seq.to_le_bytes());
        page.extend_from_slice(&[0u8; 4]); // crc placeholder
        page.push(u8::try_from(lacing.len()).unwrap());
        page.extend_from_slice(&lacing);
        page.extend_from_slice(packet);
        let crc = ogg_crc(&page);
        page[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
        page
    }

    /// Build a Vorbis comment-header packet with a given vendor + comments.
    fn vorbis_comment_packet(vendor: &str, comments: &[&str]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(b"\x03vorbis");
        p.extend_from_slice(&u32::try_from(vendor.len()).unwrap().to_le_bytes());
        p.extend_from_slice(vendor.as_bytes());
        p.extend_from_slice(&u32::try_from(comments.len()).unwrap().to_le_bytes());
        for c in comments {
            p.extend_from_slice(&u32::try_from(c.len()).unwrap().to_le_bytes());
            p.extend_from_slice(c.as_bytes());
        }
        p.push(0x01); // framing bit
        p
    }

    /// Build an Opus comment-header packet ("OpusTags").
    fn opus_tags_packet(vendor: &str, comments: &[&str]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(b"OpusTags");
        p.extend_from_slice(&u32::try_from(vendor.len()).unwrap().to_le_bytes());
        p.extend_from_slice(vendor.as_bytes());
        p.extend_from_slice(&u32::try_from(comments.len()).unwrap().to_le_bytes());
        for c in comments {
            p.extend_from_slice(&u32::try_from(c.len()).unwrap().to_le_bytes());
            p.extend_from_slice(c.as_bytes());
        }
        p
    }

    /// Walk every page in an Ogg stream and assert the capture pattern,
    /// the body framing, and the CRC all validate. Returns the per-serial
    /// completed-packet counts.
    fn validate_stream(data: &[u8]) -> std::collections::HashMap<u32, u32> {
        let mut pos = 0usize;
        let mut counts = std::collections::HashMap::new();
        while pos < data.len() {
            let (page, next) = parse_page(data, pos).expect("page must parse");
            // Recompute CRC: copy the page, zero the CRC field, recompute.
            let mut page_bytes = data[pos..next].to_vec();
            let stored =
                u32::from_le_bytes(page_bytes[CRC_OFFSET..CRC_OFFSET + 4].try_into().unwrap());
            page_bytes[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&[0; 4]);
            let computed = ogg_crc(&page_bytes);
            assert_eq!(stored, computed, "page CRC must validate at offset {pos}");

            let seg_start = page.body_start - page.num_segments;
            for &l in &data[seg_start..page.body_start] {
                if l < 255 {
                    *counts.entry(page.serial).or_insert(0u32) += 1;
                }
            }
            pos = next;
        }
        counts
    }

    /// Assert every serial's page-sequence numbers are gapless 0,1,2,...
    /// in file order. Panics on a gap or out-of-order sequence.
    fn assert_gapless_sequences(data: &[u8]) {
        let mut next_seq: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        let mut pos = 0usize;
        while pos < data.len() {
            let (page, next) = parse_page(data, pos).expect("page must parse");
            let seq = u32::from_le_bytes(
                data[pos + SEQ_OFFSET..pos + SEQ_OFFSET + 4]
                    .try_into()
                    .unwrap(),
            );
            let want = next_seq.entry(page.serial).or_insert(0);
            assert_eq!(seq, *want, "page sequence must be gapless for serial {:#x}", page.serial);
            *want += 1;
            pos = next;
        }
    }

    /// Build a stream of Ogg pages carrying, in order, the given packets for
    /// a single logical stream (serial). `packets[k]` is laid out with one
    /// packet per page UNLESS its index appears in `split_after_255`, in
    /// which case the packet is force-spread across multiple pages by
    /// chunking its body into 255-byte page payloads so the packet spans
    /// pages (last lacing 255 -> continued, continuation bit set on the
    /// follow-on page). BOS is set on the first page, EOS on the last.
    fn build_multipage_stream(serial: u32, packets: &[(Vec<u8>, bool)]) -> Vec<u8> {
        // `packets`: (bytes, force_multipage)
        let mut out = Vec::new();
        let mut seq = 0u32;
        let total = packets.len();
        for (pi, (pkt, multipage)) in packets.iter().enumerate() {
            let is_bos = pi == 0;
            let is_eos = pi + 1 == total;
            if *multipage {
                // Spread across pages: each page carries up to 255 *255-byte*
                // segments. To force a true page split we cap each page body
                // at 255 bytes (one full 255 lacing) so the packet continues.
                // Page payload size per page: 255 bytes, last page the remainder.
                let mut offset = 0usize;
                let chunk = 255usize; // one max-length segment per page -> guaranteed continuation
                let mut first_page = true;
                while offset < pkt.len() {
                    let end = (offset + chunk).min(pkt.len());
                    let body = &pkt[offset..end];
                    let last_chunk = end == pkt.len();
                    // lacing for this page's body slice
                    let lacing = lacing_for_packet(body.len());
                    // On a non-final page the body length is a multiple of 255
                    // ending in a 255 lacing (no terminating 0), signalling
                    // "packet continues". `lacing_for_packet` appends a 0 when
                    // len % 255 == 0, which would wrongly terminate; strip it
                    // on continuation pages.
                    let lacing: Vec<u8> = if !last_chunk {
                        lacing.into_iter().filter(|&l| l == 255).collect()
                    } else {
                        lacing
                    };
                    let mut header_type = 0u8;
                    if first_page && is_bos {
                        header_type |= 0x02;
                    }
                    if !first_page {
                        header_type |= 0x01; // continuation
                    }
                    if last_chunk && is_eos {
                        header_type |= 0x04;
                    }
                    let granule = if last_chunk { 100u64 } else { u64::MAX };
                    out.extend_from_slice(&assemble_test_page(
                        header_type,
                        granule,
                        serial,
                        seq,
                        &lacing,
                        body,
                    ));
                    seq += 1;
                    offset = end;
                    first_page = false;
                }
            } else {
                let mut header_type = 0u8;
                if is_bos {
                    header_type |= 0x02;
                }
                if is_eos {
                    header_type |= 0x04;
                }
                out.extend_from_slice(&build_test_page(header_type, 0, serial, seq, pkt));
                seq += 1;
            }
        }
        out
    }

    /// Assemble a page from explicit lacing + body (test mirror of the
    /// production `assemble_page`, kept separate so tests stay independent
    /// of internal helper signatures).
    fn assemble_test_page(
        header_type: u8,
        granule: u64,
        serial: u32,
        seq: u32,
        lacing: &[u8],
        body: &[u8],
    ) -> Vec<u8> {
        let mut page = Vec::new();
        page.extend_from_slice(CAPTURE);
        page.push(0);
        page.push(header_type);
        page.extend_from_slice(&granule.to_le_bytes());
        page.extend_from_slice(&serial.to_le_bytes());
        page.extend_from_slice(&seq.to_le_bytes());
        page.extend_from_slice(&[0u8; 4]);
        page.push(u8::try_from(lacing.len()).unwrap());
        page.extend_from_slice(lacing);
        page.extend_from_slice(body);
        let crc = ogg_crc(&page);
        page[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
        page
    }

    #[test]
    fn strips_vorbis_vendor_and_comments() {
        // A minimal single-stream Ogg Vorbis: id header page (BOS),
        // comment header page, setup-header page (EOS for the test).
        let id = {
            let mut v = Vec::new();
            v.extend_from_slice(b"\x01vorbis");
            v.extend_from_slice(&[0u8; 23]);
            v
        };
        let secret_vendor = "secret-encoder";
        let comment = vorbis_comment_packet(secret_vendor, &["ARTIST=me", "TITLE=private"]);
        let setup = {
            let mut v = Vec::new();
            v.extend_from_slice(b"\x05vorbis");
            v.extend_from_slice(b"fake-setup-codebooks");
            v
        };

        let serial = 0xDEAD_BEEF;
        let mut stream = Vec::new();
        stream.extend_from_slice(&build_test_page(0x02, 0, serial, 0, &id)); // BOS
        stream.extend_from_slice(&build_test_page(0x00, 0, serial, 1, &comment));
        stream.extend_from_slice(&build_test_page(0x04, 100, serial, 2, &setup)); // EOS

        // Sanity: the dirty stream really carries the secrets and validates.
        assert!(stream
            .windows(secret_vendor.len())
            .any(|w| w == secret_vendor.as_bytes()));
        assert!(stream
            .windows(b"ARTIST=me".len())
            .any(|w| w == b"ARTIST=me"));
        let before = validate_stream(&stream);

        let cleaned = strip(&stream).expect("strip must succeed");

        // (a) metadata bytes absent.
        assert!(
            !cleaned
                .windows(secret_vendor.len())
                .any(|w| w == secret_vendor.as_bytes()),
            "vendor string must be gone"
        );
        assert!(
            !cleaned
                .windows(b"ARTIST=me".len())
                .any(|w| w == b"ARTIST=me"),
            "ARTIST comment must be gone"
        );
        assert!(
            !cleaned
                .windows(b"TITLE=private".len())
                .any(|w| w == b"TITLE=private"),
            "TITLE comment must be gone"
        );

        // (b) every page CRC validates + framing parses + same stream
        // structure (same packet count per serial).
        let after = validate_stream(&cleaned);
        assert_eq!(before, after, "stream must keep the same packet structure");
        assert_eq!(after.get(&serial), Some(&3), "three packets in the stream");

        // (c) the comment header is now empty: vendor_len=0, count=0.
        let (id_page, p1) = parse_page(&cleaned, 0).unwrap();
        assert_eq!(id_page.serial, serial);
        let (cmt_page, _p2) = parse_page(&cleaned, p1).unwrap();
        let body = &cleaned[cmt_page.body_start..cmt_page.body_start + cmt_page.body_len];
        assert!(
            body.starts_with(b"\x03vorbis"),
            "still a vorbis comment header"
        );
        assert_eq!(&body[7..11], &0u32.to_le_bytes(), "vendor_len must be 0");
        assert_eq!(
            &body[11..15],
            &0u32.to_le_bytes(),
            "comment count must be 0"
        );
        assert_eq!(body[15], 0x01, "vorbis framing bit must be present");

        // The id header packet must be byte-identical.
        let body_id = &cleaned[id_page.body_start..id_page.body_start + id_page.body_len];
        assert_eq!(body_id, id.as_slice(), "id header must be untouched");

        // The setup header packet must survive.
        assert!(cleaned
            .windows(b"fake-setup-codebooks".len())
            .any(|w| w == b"fake-setup-codebooks"));
    }

    #[test]
    fn strips_opus_tags() {
        let id = {
            let mut v = Vec::new();
            v.extend_from_slice(b"OpusHead");
            v.extend_from_slice(&[1, 2, 0, 0, 0x80, 0xBB, 0, 0, 0, 0, 0]); // 19-byte head
            v
        };
        let tags = opus_tags_packet("libopus-secret", &["ENCODER=evil", "GPS=48.85,2.35"]);
        let serial = 7;
        let mut stream = Vec::new();
        stream.extend_from_slice(&build_test_page(0x02, 0, serial, 0, &id)); // BOS
        stream.extend_from_slice(&build_test_page(0x04, 960, serial, 1, &tags)); // EOS

        assert!(stream
            .windows(b"GPS=48.85,2.35".len())
            .any(|w| w == b"GPS=48.85,2.35"));
        let before = validate_stream(&stream);

        let cleaned = strip(&stream).expect("strip opus");

        assert!(!cleaned
            .windows(b"libopus-secret".len())
            .any(|w| w == b"libopus-secret"));
        assert!(!cleaned
            .windows(b"GPS=48.85,2.35".len())
            .any(|w| w == b"GPS=48.85,2.35"));
        assert!(!cleaned
            .windows(b"ENCODER=evil".len())
            .any(|w| w == b"ENCODER=evil"));

        let after = validate_stream(&cleaned);
        assert_eq!(before, after);

        // OpusHead id header must survive intact.
        assert!(cleaned.windows(b"OpusHead".len()).any(|w| w == b"OpusHead"));

        // The OpusTags packet is now empty.
        let (_id_page, p1) = parse_page(&cleaned, 0).unwrap();
        let (tag_page, _) = parse_page(&cleaned, p1).unwrap();
        let body = &cleaned[tag_page.body_start..tag_page.body_start + tag_page.body_len];
        assert!(body.starts_with(b"OpusTags"));
        assert_eq!(&body[8..12], &0u32.to_le_bytes(), "opus vendor_len 0");
        assert_eq!(&body[12..16], &0u32.to_le_bytes(), "opus count 0");
        assert_eq!(
            body.len(),
            16,
            "stripped OpusTags is sig+vendor_len+count only"
        );
    }

    #[test]
    fn comment_packet_sharing_a_page_keeps_following_packets() {
        // Pathological-but-valid: a single page holding TWO packets, the
        // comment header followed by another packet, both terminating in
        // the same page. The second packet's bytes must survive verbatim.
        let comment = vorbis_comment_packet("v", &["X=1"]);
        let trailer = b"\x05vorbis-setup-data".to_vec();
        let serial = 42;

        let mut body = Vec::new();
        body.extend_from_slice(&comment);
        body.extend_from_slice(&trailer);
        let mut lacing = lacing_for_packet(comment.len());
        lacing.extend_from_slice(&lacing_for_packet(trailer.len()));

        let mut stream = build_test_page(0x02, 0, serial, 0, b"\x01vorbis-id");

        let mut page = Vec::new();
        page.extend_from_slice(CAPTURE);
        page.push(0);
        page.push(0x04); // EOS
        page.extend_from_slice(&0u64.to_le_bytes());
        page.extend_from_slice(&serial.to_le_bytes());
        page.extend_from_slice(&1u32.to_le_bytes());
        page.extend_from_slice(&[0u8; 4]);
        page.push(u8::try_from(lacing.len()).unwrap());
        page.extend_from_slice(&lacing);
        page.extend_from_slice(&body);
        let crc = ogg_crc(&page);
        page[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());

        stream.extend_from_slice(&page);

        let before = validate_stream(&stream);
        let cleaned = strip(&stream).expect("strip multi-packet page");
        let after = validate_stream(&cleaned);
        assert_eq!(before, after, "packet count per serial unchanged");

        assert!(
            cleaned
                .windows(trailer.len())
                .any(|w| w == trailer.as_slice()),
            "the packet after the comment header must be preserved verbatim"
        );
        assert!(!cleaned.windows(b"X=1".len()).any(|w| w == b"X=1"));
    }

    #[test]
    fn rejects_bad_capture_pattern() {
        let bad = b"NOTOGGSxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
        let err = strip(bad).expect_err("must reject non-ogg");
        assert!(matches!(err, CoreError::ParseError { .. }));
    }

    #[test]
    fn rejects_truncated_page() {
        let truncated = b"OggS\x00\x02\x00\x00"; // capture + version + a few bytes
        let err = strip(truncated).expect_err("must reject truncated");
        assert!(matches!(err, CoreError::ParseError { .. }));
    }

    #[test]
    fn rejects_body_overrun() {
        // A page claiming a 255-byte segment but with no body.
        let mut p = Vec::new();
        p.extend_from_slice(CAPTURE);
        p.push(0);
        p.push(0x02);
        p.extend_from_slice(&0u64.to_le_bytes());
        p.extend_from_slice(&1u32.to_le_bytes());
        p.extend_from_slice(&0u32.to_le_bytes());
        p.extend_from_slice(&[0u8; 4]);
        p.push(1); // one segment
        p.push(255); // claims 255 body bytes
                     // no body
        let err = strip(&p).expect_err("must reject body overrun");
        assert!(matches!(err, CoreError::ParseError { .. }));
    }

    #[test]
    fn crc_matches_known_reference() {
        assert_eq!(ogg_crc(&[0, 0, 0, 0]), 0);
        let t = crc_table();
        assert_eq!(ogg_crc(&[0x01]), t[1]);
        assert_ne!(t[1], 0);
    }

    /// REGRESSION (multi-page comment header leak): a Vorbis comment header
    /// packet large enough to span TWO pages (vendor "SECRETVENDORLEAK" + a
    /// 300-byte DESC comment) used to pass through COMPLETELY UNSTRIPPED
    /// because the stripper only rewrote a comment packet that was
    /// self-contained in its first page. This builds that 2-page comment
    /// header, strips it, and asserts the leak is closed: vendor empty, no
    /// comments, none of the secret bytes survive, every page CRC validates,
    /// sequences stay gapless, and the audio (id + setup + data packets)
    /// is intact with the same per-serial packet count.
    #[test]
    fn strips_multipage_vorbis_comment_header() {
        let secret_vendor = "SECRETVENDORLEAK";
        // 300-byte comment value ("DESC=" + 295 'A's) to push the comment
        // header well past one page (>255 bytes).
        let mut desc = String::from("DESC=");
        desc.push_str(&"A".repeat(295));
        assert_eq!(desc.len(), 300);
        let comment = vorbis_comment_packet(secret_vendor, &[desc.as_str()]);
        // Confirm the fixture's comment packet really needs >1 page.
        assert!(
            comment.len() > 255,
            "comment packet must exceed one page to exercise the multi-page path"
        );

        // id header (packet 0), comment header (packet 1, multi-page),
        // setup header (packet 2), one audio data packet (packet 3).
        let id = {
            let mut v = Vec::new();
            v.extend_from_slice(b"\x01vorbis");
            v.extend_from_slice(&[0u8; 23]);
            v
        };
        let setup = {
            let mut v = Vec::new();
            v.extend_from_slice(b"\x05vorbis");
            v.extend_from_slice(b"fake-setup-codebooks-DATA");
            v
        };
        let audio = b"AUDIOPACKETBYTES-keep-verbatim".to_vec();

        let serial = 0x0102_0304u32;
        let packets = vec![
            (id.clone(), false),
            (comment.clone(), true), // force multi-page
            (setup.clone(), false),
            (audio.clone(), false),
        ];
        let stream = build_multipage_stream(serial, &packets);

        // Sanity: the dirty stream carries the secrets and validates. The
        // vendor (16 bytes) lands within the first page; the 300-byte DESC
        // is split across the page boundary, so we verify its presence via
        // reassembly of packet 1 rather than a contiguous window.
        assert!(stream
            .windows(secret_vendor.len())
            .any(|w| w == secret_vendor.as_bytes()));
        let dirty_pkt1 = reassemble_packet(&stream, serial, 1);
        assert!(
            dirty_pkt1
                .windows(desc.len())
                .any(|w| w == desc.as_bytes()),
            "fixture comment packet must contain the 300-byte DESC"
        );
        let before = validate_stream(&stream);
        assert_gapless_sequences(&stream);
        // The comment packet must actually span >1 page in the fixture: with
        // id (1 page) + a 2+ page comment + setup (1) + audio (1), the total
        // page count exceeds the 4 packets.
        let mut page_count = 0usize;
        {
            let mut pos = 0usize;
            while pos < stream.len() {
                let (_p, n) = parse_page(&stream, pos).unwrap();
                page_count += 1;
                pos = n;
            }
        }
        assert!(
            page_count > 4,
            "comment header must span multiple pages (got {page_count} pages for 4 packets)"
        );

        let cleaned = strip(&stream).expect("strip must succeed");

        // (a) NONE of the secret bytes survive anywhere in the output.
        assert!(
            !cleaned
                .windows(secret_vendor.len())
                .any(|w| w == secret_vendor.as_bytes()),
            "vendor string leaked across the multi-page comment header"
        );
        assert!(
            !cleaned.windows(desc.len()).any(|w| w == desc.as_bytes()),
            "300-byte DESC comment leaked across the multi-page comment header"
        );
        // A long run of 'A's (part of the comment) must be gone too.
        let aaa = "A".repeat(60);
        assert!(
            !cleaned.windows(aaa.len()).any(|w| w == aaa.as_bytes()),
            "comment payload bytes leaked"
        );

        // (b) every page CRC validates and sequences stay gapless.
        let after = validate_stream(&cleaned);
        assert_gapless_sequences(&cleaned);

        // (c) same per-serial packet count (4 packets) preserved.
        assert_eq!(
            before.get(&serial),
            Some(&4),
            "fixture must have 4 packets for the serial"
        );
        assert_eq!(
            after.get(&serial),
            Some(&4),
            "stripped stream must keep all 4 packets for the serial"
        );

        // (d) the comment header is now empty: still a vorbis comment header
        // with vendor_len=0, count=0, framing bit present. Reassemble packet
        // 1 from the cleaned stream to inspect it.
        let pkt1 = reassemble_packet(&cleaned, serial, 1);
        assert!(pkt1.starts_with(b"\x03vorbis"), "still a vorbis comment header");
        assert_eq!(&pkt1[7..11], &0u32.to_le_bytes(), "vendor_len must be 0");
        assert_eq!(&pkt1[11..15], &0u32.to_le_bytes(), "comment count must be 0");
        assert_eq!(pkt1[15], 0x01, "vorbis framing bit must be present");
        assert_eq!(pkt1.len(), 16, "stripped vorbis comment is sig+0+0+framing");

        // (e) id, setup, and audio packets survive byte-for-byte.
        assert_eq!(reassemble_packet(&cleaned, serial, 0), id, "id header intact");
        assert_eq!(reassemble_packet(&cleaned, serial, 2), setup, "setup header intact");
        assert_eq!(reassemble_packet(&cleaned, serial, 3), audio, "audio packet intact");
    }

    /// Reassemble the `want`-th packet (0-based) of `serial` from a stream,
    /// concatenating segments across pages (a packet continues while a
    /// page's last lacing value is 255). Test-only helper.
    fn reassemble_packet(data: &[u8], serial: u32, want: usize) -> Vec<u8> {
        let mut pos = 0usize;
        let mut packet_index = 0usize;
        let mut cur = Vec::new();
        while pos < data.len() {
            let (page, next) = parse_page(data, pos).expect("page parses");
            if page.serial == serial {
                let seg_start = page.body_start - page.num_segments;
                let lacing = &data[seg_start..page.body_start];
                let mut boff = page.body_start;
                for &l in lacing {
                    let seg = &data[boff..boff + usize::from(l)];
                    boff += usize::from(l);
                    cur.extend_from_slice(seg);
                    if l < 255 {
                        if packet_index == want {
                            return std::mem::take(&mut cur);
                        }
                        cur.clear();
                        packet_index += 1;
                    }
                }
            }
            pos = next;
        }
        panic!("packet {want} for serial {serial:#x} not found");
    }
}
