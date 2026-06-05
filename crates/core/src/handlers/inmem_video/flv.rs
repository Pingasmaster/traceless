//! Pure-Rust, fully in-memory metadata strip for the FLV container
//! (`video/x-flv`), behind the `wasm-inmem` feature.
//!
//! FLV is a flat tag stream and needs no transcode to strip metadata, so
//! this is manual tag surgery (no FLV writer crate):
//!
//! ```text
//! [ 9-byte header ]  FLV\x01 <flags:u8> <DataOffset:u32be>
//! [ PreviousTagSize0:u32be == 0 ]
//! ( for each tag: )
//!   [ 11-byte tag header ]  <type:u8> <DataSize:u24be> <Timestamp:u24be>
//!                           <TimestampExt:u8> <StreamID:u24be>
//!   [ DataSize bytes of tag body ]
//!   [ PreviousTagSize:u32be == 11 + DataSize ]
//! ```
//!
//! Tag types: 8 = audio, 9 = video, 18 (0x12) = SCRIPTDATA. The first
//! SCRIPTDATA tag normally carries the `onMetaData` ECMA array, which
//! holds non-structural metadata (creator / encoder / metadatacreator,
//! GPS latitude / longitude, ...) alongside structural hints
//! (width / height / duration). The native ffmpeg path runs
//! `-map_metadata -1 -map_chapters -1`, which drops exactly this
//! non-structural metadata.
//!
//! Matching mat2 / ffmpeg's faithful behaviour, this strips every
//! SCRIPTDATA (type-18) tag in the stream (the `onMetaData` array and any
//! other AMF script payloads are pure metadata; dropping them is what an
//! anonymiser wants), keeping every audio (8) and video (9) tag
//! byte-for-byte. Each surviving tag's leading `PreviousTagSize` is
//! rewritten so the chain stays consistent (the u32 *before* a tag is the
//! size of the tag that *precedes* it, so removing a tag means the next
//! tag's leading `PreviousTagSize` must reflect the new predecessor, or 0
//! when the survivor is now first).
//!
//! Every length / offset read from the (attacker-controlled) input is
//! bounds-checked with checked arithmetic; a malformed file yields
//! `Err(CoreError::ParseError { .. })`, never a panic or OOM.

use crate::error::CoreError;

/// FLV signature: the bytes `F`, `L`, `V`.
const FLV_SIGNATURE: [u8; 3] = *b"FLV";
/// Fixed FLV header length (signature + version + flags + DataOffset).
const FLV_HEADER_LEN: usize = 9;
/// Tag header length (type + DataSize + Timestamp + TimestampExt + StreamID).
const TAG_HEADER_LEN: usize = 11;
/// Length of a `PreviousTagSize` field.
const PREV_TAG_SIZE_LEN: usize = 4;
/// FLV SCRIPTDATA tag type (carries the `onMetaData` AMF array).
const TAG_TYPE_SCRIPTDATA: u8 = 18;

/// Read a big-endian u24 from `buf[off..off+3]`, bounds-checked.
fn read_u24_be(buf: &[u8], off: usize) -> Option<usize> {
    let b0 = *buf.get(off)?;
    let b1 = *buf.get(off.checked_add(1)?)?;
    let b2 = *buf.get(off.checked_add(2)?)?;
    Some(((b0 as usize) << 16) | ((b1 as usize) << 8) | (b2 as usize))
}

/// Build a `ParseError` with the empty path the in-memory API uses (the
/// native wrapper re-paths it via `handlers::repath`).
fn parse_err(detail: &str) -> CoreError {
    CoreError::ParseError {
        path: std::path::PathBuf::new(),
        detail: detail.to_string(),
    }
}

/// Strip all SCRIPTDATA (metadata) tags from an FLV byte stream, keeping
/// every audio / video tag verbatim and remuxing a consistent
/// `PreviousTagSize` chain. Fully in-memory, no transcode.
///
/// # Errors
///
/// Returns [`CoreError::ParseError`] if the input is not a structurally
/// valid FLV (bad signature, truncated header, a tag header/body/trailer
/// that runs past the end of the buffer, or an inconsistent
/// `PreviousTagSize`).
// `pub(crate)` is the module convention this workflow mandates; the
// `redundant_pub_crate` nursery lint fires only because the integrator has
// not yet promoted the module's visibility.
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn strip(input: &[u8]) -> Result<Vec<u8>, CoreError> {
    // ---- Header ----
    if input.len() < FLV_HEADER_LEN {
        return Err(parse_err("FLV too short for a 9-byte header"));
    }
    let sig = input
        .get(0..3)
        .ok_or_else(|| parse_err("missing FLV signature"))?;
    if sig != FLV_SIGNATURE {
        return Err(parse_err("not an FLV (bad signature)"));
    }
    // DataOffset (u32be at byte 5) is the size of the header; spec says 9,
    // but honour the declared value so we copy any header padding verbatim.
    let b5 = *input.get(5).ok_or_else(|| parse_err("truncated header"))?;
    let b6 = *input.get(6).ok_or_else(|| parse_err("truncated header"))?;
    let b7 = *input.get(7).ok_or_else(|| parse_err("truncated header"))?;
    let b8 = *input.get(8).ok_or_else(|| parse_err("truncated header"))?;
    let data_offset =
        ((b5 as usize) << 24) | ((b6 as usize) << 16) | ((b7 as usize) << 8) | (b8 as usize);
    if data_offset < FLV_HEADER_LEN || data_offset > input.len() {
        return Err(parse_err("FLV DataOffset out of range"));
    }

    // Output starts as a verbatim copy of the header (through DataOffset).
    let header = input
        .get(0..data_offset)
        .ok_or_else(|| parse_err("header slice out of range"))?;
    let mut out: Vec<u8> = Vec::with_capacity(input.len());
    out.extend_from_slice(header);

    // After the header comes PreviousTagSize0 (always 0). We emit our own
    // chain from scratch, so consume it here and seed the output with 0.
    let mut pos = data_offset
        .checked_add(PREV_TAG_SIZE_LEN)
        .ok_or_else(|| parse_err("offset overflow after header"))?;
    if pos > input.len() {
        // A header with no tag stream at all: just the header + a zero
        // PreviousTagSize0 is a valid (empty) FLV. Emit that.
        out.extend_from_slice(&0u32.to_be_bytes());
        return Ok(out);
    }
    out.extend_from_slice(&0u32.to_be_bytes());

    // ---- Tag loop ----
    // Each iteration consumes: [11-byte header][DataSize body][u32 trailer].
    while pos < input.len() {
        // Tag header.
        let header_end = pos
            .checked_add(TAG_HEADER_LEN)
            .ok_or_else(|| parse_err("offset overflow at tag header"))?;
        if header_end > input.len() {
            return Err(parse_err("truncated tag header"));
        }
        let tag_type = *input
            .get(pos)
            .ok_or_else(|| parse_err("missing tag type"))?;
        let data_size = read_u24_be(
            input,
            pos.checked_add(1)
                .ok_or_else(|| parse_err("offset overflow"))?,
        )
        .ok_or_else(|| parse_err("truncated tag DataSize"))?;

        // Full on-wire tag span: header + body.
        let body_end = header_end
            .checked_add(data_size)
            .ok_or_else(|| parse_err("offset overflow at tag body"))?;
        if body_end > input.len() {
            return Err(parse_err("tag body runs past end of file"));
        }
        // The PreviousTagSize trailer that follows the body.
        let trailer_end = body_end
            .checked_add(PREV_TAG_SIZE_LEN)
            .ok_or_else(|| parse_err("offset overflow at tag trailer"))?;
        if trailer_end > input.len() {
            return Err(parse_err("missing PreviousTagSize trailer"));
        }
        // Validate the trailer matches this tag's on-wire size (defends
        // against a corrupt stream; ffmpeg would reject it too).
        let on_wire_size = TAG_HEADER_LEN
            .checked_add(data_size)
            .ok_or_else(|| parse_err("tag on-wire size overflow"))?;
        let tb0 = *input
            .get(body_end)
            .ok_or_else(|| parse_err("trailer read"))?;
        let tb1 = *input
            .get(
                body_end
                    .checked_add(1)
                    .ok_or_else(|| parse_err("overflow"))?,
            )
            .ok_or_else(|| parse_err("trailer read"))?;
        let tb2 = *input
            .get(
                body_end
                    .checked_add(2)
                    .ok_or_else(|| parse_err("overflow"))?,
            )
            .ok_or_else(|| parse_err("trailer read"))?;
        let tb3 = *input
            .get(
                body_end
                    .checked_add(3)
                    .ok_or_else(|| parse_err("overflow"))?,
            )
            .ok_or_else(|| parse_err("trailer read"))?;
        let declared_trailer =
            ((tb0 as u32) << 24) | ((tb1 as u32) << 16) | ((tb2 as u32) << 8) | (tb3 as u32);
        let on_wire_u32 =
            u32::try_from(on_wire_size).map_err(|_| parse_err("tag on-wire size exceeds u32"))?;
        if declared_trailer != on_wire_u32 {
            return Err(parse_err("PreviousTagSize does not match tag size"));
        }

        if tag_type == TAG_TYPE_SCRIPTDATA {
            // Drop the entire SCRIPTDATA tag (header + body + trailer).
            // We do not emit a leading PreviousTagSize before a kept tag;
            // instead each kept tag emits its OWN trailer, and the loop
            // body re-derives the chain. So removing this tag simply
            // shrinks the gap before the next survivor, which is exactly
            // the ffmpeg-style remux fix-up.
            pos = trailer_end;
            continue;
        }

        // Keep this tag. The leading PreviousTagSize that precedes it in
        // `out` is, by construction, the trailer the previous kept tag
        // emitted (or PreviousTagSize0 == 0 for the first survivor), so the
        // chain is consistent regardless of how many SCRIPTDATA tags we
        // skipped between them.
        //
        // Copy the tag header + body verbatim, then emit this tag's own
        // trailer (its on-wire size); that trailer is the leading
        // PreviousTagSize the NEXT kept tag will read.
        let tag_bytes = input
            .get(pos..body_end)
            .ok_or_else(|| parse_err("tag slice out of range"))?;
        out.extend_from_slice(tag_bytes);
        out.extend_from_slice(&on_wire_u32.to_be_bytes());

        pos = trailer_end;
    }

    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Push an 11-byte tag header + body + the matching PreviousTagSize
    /// trailer onto `buf`.
    fn push_tag(buf: &mut Vec<u8>, tag_type: u8, body: &[u8]) {
        let data_size = body.len();
        buf.push(tag_type);
        buf.push(((data_size >> 16) & 0xFF) as u8);
        buf.push(((data_size >> 8) & 0xFF) as u8);
        buf.push((data_size & 0xFF) as u8);
        // Timestamp (u24) + TimestampExt (u8) + StreamID (u24) = 7 zero bytes.
        buf.extend_from_slice(&[0u8; 7]);
        buf.extend_from_slice(body);
        let on_wire = (TAG_HEADER_LEN + data_size) as u32;
        buf.extend_from_slice(&on_wire.to_be_bytes());
    }

    /// Build a minimal FLV: header + PreviousTagSize0 + a SCRIPTDATA
    /// `onMetaData` tag carrying "encoder"/"latitude" + one video tag.
    fn dirty_flv() -> Vec<u8> {
        let mut v = Vec::new();
        // Header: "FLV", version 1, flags 0x05 (audio+video), DataOffset 9.
        v.extend_from_slice(&FLV_SIGNATURE);
        v.push(0x01);
        v.push(0x05);
        v.extend_from_slice(&9u32.to_be_bytes());
        // PreviousTagSize0.
        v.extend_from_slice(&0u32.to_be_bytes());

        // SCRIPTDATA onMetaData body: not a fully valid AMF object, but it
        // carries the recognisable metadata key bytes we assert are gone.
        let mut script = Vec::new();
        script.extend_from_slice(b"\x02\x00\x0aonMetaData");
        script.extend_from_slice(b"encoder");
        script.extend_from_slice(b"Lavf-secret-encoder");
        script.extend_from_slice(b"latitude");
        script.extend_from_slice(&48.85f64.to_be_bytes());
        script.extend_from_slice(b"longitude");
        script.extend_from_slice(&2.35f64.to_be_bytes());
        push_tag(&mut v, TAG_TYPE_SCRIPTDATA, &script);

        // One video tag with a distinctive body.
        push_tag(&mut v, 9, b"\x17\x00VIDEO-FRAME-PAYLOAD");

        v
    }

    /// Walk an FLV tag stream and return (type, body) for each tag, while
    /// asserting the PreviousTagSize chain is internally consistent.
    fn parse_tags(flv: &[u8]) -> Vec<(u8, Vec<u8>)> {
        assert_eq!(&flv[0..3], &FLV_SIGNATURE);
        let data_offset = ((flv[5] as usize) << 24)
            | ((flv[6] as usize) << 16)
            | ((flv[7] as usize) << 8)
            | (flv[8] as usize);
        // PreviousTagSize0 must be 0.
        let mut pos = data_offset;
        let pts0 = u32::from_be_bytes([flv[pos], flv[pos + 1], flv[pos + 2], flv[pos + 3]]);
        assert_eq!(pts0, 0, "PreviousTagSize0 must be 0");
        pos += 4;

        let mut prev_size: u32 = 0;
        let mut tags = Vec::new();
        while pos < flv.len() {
            let tag_type = flv[pos];
            let data_size = ((flv[pos + 1] as usize) << 16)
                | ((flv[pos + 2] as usize) << 8)
                | (flv[pos + 3] as usize);
            // The leading PreviousTagSize for this tag was the previous
            // trailer; verify the chain.
            let body_start = pos + TAG_HEADER_LEN;
            let body_end = body_start + data_size;
            let body = flv[body_start..body_end].to_vec();
            let trailer = u32::from_be_bytes([
                flv[body_end],
                flv[body_end + 1],
                flv[body_end + 2],
                flv[body_end + 3],
            ]);
            let on_wire = (TAG_HEADER_LEN + data_size) as u32;
            assert_eq!(trailer, on_wire, "tag trailer must equal its on-wire size");
            // prev_size is what the *previous* trailer wrote; the chain is
            // consistent if each trailer equals the next tag's predecessor
            // size. We track it implicitly via `on_wire` below.
            let _ = prev_size;
            prev_size = on_wire;
            tags.push((tag_type, body));
            pos = body_end + 4;
        }
        tags
    }

    #[test]
    fn strip_removes_scriptdata_keeps_video_and_fixes_chain() {
        let dirty = dirty_flv();

        // Sanity: the dirty fixture really carries the metadata + parses.
        let before = parse_tags(&dirty);
        assert_eq!(before.len(), 2, "fixture must have script + video tag");
        assert!(
            before.iter().any(|(t, _)| *t == TAG_TYPE_SCRIPTDATA),
            "fixture must contain a SCRIPTDATA tag"
        );
        assert!(
            dirty
                .windows(b"Lavf-secret-encoder".len())
                .any(|w| w == b"Lavf-secret-encoder"),
            "fixture must contain the encoder string"
        );
        assert!(
            dirty.windows(b"latitude".len()).any(|w| w == b"latitude"),
            "fixture must contain the latitude key"
        );

        let cleaned = strip(&dirty).unwrap();

        // (a/c) The SCRIPTDATA metadata bytes must be gone.
        assert!(
            !cleaned
                .windows(b"onMetaData".len())
                .any(|w| w == b"onMetaData"),
            "cleaned FLV must not contain onMetaData"
        );
        assert!(
            !cleaned
                .windows(b"Lavf-secret-encoder".len())
                .any(|w| w == b"Lavf-secret-encoder"),
            "cleaned FLV must not retain the encoder string"
        );
        assert!(
            !cleaned.windows(b"latitude".len()).any(|w| w == b"latitude"),
            "cleaned FLV must not retain the latitude key"
        );

        // (b) The video tag must survive byte-for-byte.
        assert!(
            cleaned
                .windows(b"VIDEO-FRAME-PAYLOAD".len())
                .any(|w| w == b"VIDEO-FRAME-PAYLOAD"),
            "cleaned FLV must keep the video tag body verbatim"
        );

        // (d) Structural integrity: still a valid FLV with the SAME header,
        // exactly one tag (the video tag, type 9), and a consistent
        // PreviousTagSize chain (parse_tags asserts the chain).
        assert_eq!(
            &cleaned[0..FLV_HEADER_LEN],
            &dirty[0..FLV_HEADER_LEN],
            "header preserved"
        );
        let after = parse_tags(&cleaned);
        assert_eq!(after.len(), 1, "exactly the one video tag must remain");
        assert_eq!(after[0].0, 9, "the surviving tag must be the video tag");
        assert_eq!(
            after[0].1, b"\x17\x00VIDEO-FRAME-PAYLOAD",
            "video body intact"
        );
    }

    #[test]
    fn rejects_bad_signature() {
        let mut bad = dirty_flv();
        bad[0] = b'X';
        assert!(matches!(strip(&bad), Err(CoreError::ParseError { .. })));
    }

    #[test]
    fn rejects_truncated_tag_body() {
        let mut v = Vec::new();
        v.extend_from_slice(&FLV_SIGNATURE);
        v.push(0x01);
        v.push(0x05);
        v.extend_from_slice(&9u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        // A tag header claiming a huge body but no body bytes.
        v.push(9);
        v.extend_from_slice(&[0xFF, 0xFF, 0xFF]); // DataSize = 16777215
        v.extend_from_slice(&[0u8; 7]);
        assert!(matches!(strip(&v), Err(CoreError::ParseError { .. })));
    }

    #[test]
    fn rejects_too_short() {
        assert!(matches!(strip(b"FL"), Err(CoreError::ParseError { .. })));
    }

    #[test]
    fn header_only_flv_is_valid_empty() {
        let mut v = Vec::new();
        v.extend_from_slice(&FLV_SIGNATURE);
        v.push(0x01);
        v.push(0x00);
        v.extend_from_slice(&9u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        let cleaned = strip(&v).unwrap();
        assert_eq!(cleaned, v, "header-only FLV must round-trip unchanged");
    }
}
