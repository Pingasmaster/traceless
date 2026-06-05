// In-memory Matroska / WebM (EBML) metadata stripper for the `wasm-inmem`
// build.
//
// Mirrors the native ffmpeg path
// (`ffmpeg -map 0 -c copy -map_metadata -1 -map_chapters -1 -disposition 0
// -fflags +bitexact`): a pure metadata strip + remux that keeps every
// audio / video / subtitle track and every Cluster (the actual media
// frames), while dropping every descriptive tag. Concretely it:
//
// * drops the whole `\Segment\Tags` list (global + per-track tags),
// * drops `\Segment\Attachments` (cover art, fonts, attached files),
// * drops `\Segment\Chapters`,
// * in `\Segment\Info` blanks `MuxingApp` / `WritingApp` to a constant,
//   clears `Title`, and removes `DateUTC`, `SegmentFilename`,
//   `PrevFilename` and `NextFilename`,
// * clears every per-track `Name` and `CodecName` (human-readable
//   descriptive strings),
// * drops the `SeekHead` index and `Cues`: their absolute byte offsets
//   are invalidated by the rewrite, both are optional, and players
//   regenerate them, so re-emitting stale ones would corrupt seeking.
//
// The container is parsed into the `mkv-element` typed element tree and
// re-serialized, so the output is valid EBML with the same tracks and
// clusters even though it is not byte-identical to the input (element
// ordering / size encodings may differ, exactly like the native remux).
//
// Robustness: `mkv-element` bounds every length/offset against the
// available buffer and returns an `Err` rather than panicking on
// malformed input. The one legal construct it refuses outright is an
// unknown-size Segment/Cluster body (live / streamed muxes set all size
// bits to 1 so the element runs to EOF); we handle that here by walking
// the top level ourselves with the public `Header` reader and treating an
// unknown-size Segment body as "the rest of the input", so those files
// clean instead of erroring.

use std::io::{Cursor, Read};

use mkv_element::io::blocking_impl::{ReadFrom, WriteTo};
use mkv_element::prelude::*;

use crate::error::CoreError;

/// Constant the muxing/writing app strings are rewritten to, matching the
/// short, content-free label ffmpeg's bitexact remux leaves behind.
const REMUX_APP: &str = "Lavf";

/// Strip all container metadata from a Matroska / WebM byte stream,
/// keeping every track and cluster.
///
/// # Errors
///
/// Returns [`CoreError::ParseError`] if the input is not parseable as
/// Matroska/EBML (bad magic, truncated, malformed element framing), and
/// [`CoreError::CleanError`] if the cleaned tree cannot be re-serialized.
pub(crate) fn strip(input: &[u8]) -> Result<Vec<u8>, CoreError> {
    // The first top-level element of any Matroska/WebM file MUST be the
    // EBML header (ID 0x1A45DFA3). Reject anything else up front so we
    // give a clean 422 rather than mis-parsing arbitrary bytes.
    if input.len() < 4
        || input[0] != 0x1A
        || input[1] != 0x45
        || input[2] != 0xDF
        || input[3] != 0xA3
    {
        return Err(parse_err(
            "not a Matroska/WebM stream (missing EBML header)".to_string(),
        ));
    }

    let mut cursor = Cursor::new(input);
    let mut out: Vec<u8> = Vec::with_capacity(input.len());

    // Walk the top-level element sequence: EBML header, then one or more
    // Segments (with possible Void / CRC-32 padding between them). We read
    // each element's Header explicitly so we can special-case an
    // unknown-size Segment (legal: body runs to EOF) instead of letting
    // the crate's read_body reject it.
    let total = input.len() as u64;
    loop {
        let pos = cursor.position();
        if pos >= total {
            break;
        }
        let header = match Header::read_from(&mut cursor) {
            Ok(h) => h,
            // A clean EOF between elements is the normal loop terminator;
            // any other read error means the framing is malformed.
            Err(mkv_element::Error::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(e) => return Err(parse_err(format!("malformed EBML element header: {e}"))),
        };
        let body_start = cursor.position();

        // Determine the body byte range. Known size: exactly `*size`
        // bytes. Unknown size (Segment/Cluster only, per spec): the body
        // extends to the end of the input.
        let body_len: u64 = if header.size.is_unknown {
            total
                .checked_sub(body_start)
                .ok_or_else(|| parse_err("element body starts past end of input".to_string()))?
        } else {
            *header.size
        };
        let body_end = body_start
            .checked_add(body_len)
            .ok_or_else(|| parse_err("element body length overflows".to_string()))?;
        if body_end > total {
            return Err(parse_err(format!(
                "element body ({body_len}B) runs past end of input"
            )));
        }

        // Pull the body bytes out of the cursor.
        let mut body = vec![0u8; body_len_usize(body_len)?];
        cursor
            .read_exact(&mut body)
            .map_err(|e| parse_err(format!("short read of element body: {e}")))?;

        if header.id == Segment::ID {
            // Decode, clean, re-serialize this Segment.
            let mut segment = Segment::decode_body(&mut &body[..])
                .map_err(|e| parse_err(format!("failed to parse Segment: {e}")))?;
            clean_segment(&mut segment);
            segment
                .write_to(&mut out)
                .map_err(|e| clean_err(format!("failed to re-serialize cleaned Segment: {e}")))?;
        } else if header.id == Ebml::ID {
            // Re-emit the EBML header (it carries no user metadata: just
            // versions + DocType, which must be preserved so the output
            // keeps its matroska/webm identity). Round-trip through the
            // typed element so a malformed header is rejected, not copied.
            let ebml = Ebml::decode_body(&mut &body[..])
                .map_err(|e| parse_err(format!("failed to parse EBML header: {e}")))?;
            ebml.write_to(&mut out)
                .map_err(|e| clean_err(format!("failed to re-serialize EBML header: {e}")))?;
        }
        // Any other top-level element (a stray Void / CRC-32 between
        // segments) carries no metadata and no media: drop it.
    }

    Ok(out)
}

/// Build a [`CoreError::ParseError`] with an empty path (the native layer
/// rewrites it via `handlers::repath`); the HTTP layer maps it to 422.
const fn parse_err(detail: String) -> CoreError {
    CoreError::ParseError {
        path: std::path::PathBuf::new(),
        detail,
    }
}

/// Build a [`CoreError::CleanError`] (maps to 500: a genuine internal
/// re-serialization failure, not bad input).
const fn clean_err(detail: String) -> CoreError {
    CoreError::CleanError {
        path: std::path::PathBuf::new(),
        detail,
    }
}

/// Convert a validated `u64` body length to `usize`, guarding the wasip2
/// 32-bit `usize`. Lengths this large cannot be backed by the in-memory
/// buffer we already hold, so this only trips on absurd (corrupt) size
/// fields.
fn body_len_usize(len: u64) -> Result<usize, CoreError> {
    usize::try_from(len).map_err(|_| {
        parse_err(format!(
            "element body length {len} exceeds addressable memory"
        ))
    })
}

/// Apply the `-map_metadata -1 -map_chapters -1` strip to a decoded
/// Segment in place.
fn clean_segment(segment: &mut Segment) {
    // Global + track-scoped descriptive tags.
    segment.tags.clear();
    // Attached files (cover art, fonts, ...).
    segment.attachments = None;
    // Menu / chapter data.
    segment.chapters = None;
    // The seek index and cue points reference byte offsets that our
    // rewrite invalidates; both are optional and players regenerate them,
    // so re-emitting stale ones would break seeking. Drop them.
    segment.seek_head.clear();
    segment.cues = None;

    // Segment\Info: blank the descriptive / identifying string fields and
    // drop the creation timestamp, but keep the structural fields
    // (TimestampScale, Duration, the *UUIDs that link segments together).
    let info = &mut segment.info;
    info.muxing_app = MuxingApp(REMUX_APP.to_string());
    info.writing_app = WritingApp(REMUX_APP.to_string());
    info.title = None;
    info.date_utc = None;
    info.segment_filename = None;
    info.prev_filename = None;
    info.next_filename = None;

    // Per-track human-readable strings.
    if let Some(tracks) = segment.tracks.as_mut() {
        for track in &mut tracks.track_entry {
            track.name = None;
            track.codec_name = None;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Build a tiny but structurally complete MKV in memory carrying known
    /// metadata: an EBML header, a Segment with Info (Title / WritingApp /
    /// MuxingApp / DateUTC / SegmentFilename), two tracks (each with a
    /// Name), one Cluster (the "media"), an Attachments element, and a
    /// Tags element.
    fn dirty_mkv() -> Vec<u8> {
        let ebml = Ebml {
            ebml_max_id_length: EbmlMaxIdLength(4),
            ebml_max_size_length: EbmlMaxSizeLength(8),
            doc_type: Some(DocType("matroska".to_string())),
            doc_type_version: Some(DocTypeVersion(4)),
            doc_type_read_version: Some(DocTypeReadVersion(2)),
            ..Default::default()
        };

        let segment = Segment {
            crc32: None,
            void: None,
            seek_head: vec![],
            info: Info {
                timestamp_scale: TimestampScale(1_000_000),
                muxing_app: MuxingApp("libebml v1.4.4 + libmatroska v1.7.1".to_string()),
                writing_app: WritingApp("mkvmerge v75.0.0 ('SECRET-TOOL')".to_string()),
                duration: Some(Duration(120_000.0)),
                title: Some(Title("My Secret Holiday Video".to_string())),
                date_utc: Some(DateUtc(123_456_789)),
                segment_filename: Some(SegmentFilename("/home/alice/secret.mkv".to_string())),
                ..Default::default()
            },
            cluster: vec![Cluster {
                timestamp: Timestamp(0),
                ..Default::default()
            }],
            tracks: Some(Tracks {
                track_entry: vec![
                    TrackEntry {
                        track_number: TrackNumber(1),
                        track_uid: TrackUid(0xDEAD_BEEF),
                        track_type: TrackType(1), // video
                        codec_id: CodecId("V_VP9".to_string()),
                        name: Some(Name("Main Video Track (Alice's cam)".to_string())),
                        codec_name: Some(CodecName("VP9 by alice".to_string())),
                        ..Default::default()
                    },
                    TrackEntry {
                        track_number: TrackNumber(2),
                        track_uid: TrackUid(0xCAFE_F00D),
                        track_type: TrackType(2), // audio
                        codec_id: CodecId("A_OPUS".to_string()),
                        name: Some(Name("Director Commentary".to_string())),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }),
            cues: None,
            attachments: Some(Attachments {
                crc32: None,
                void: None,
                attached_file: vec![AttachedFile {
                    crc32: None,
                    void: None,
                    file_description: Some(FileDescription("cover art".to_string())),
                    file_name: FileName("cover.jpg".to_string()),
                    file_media_type: FileMediaType("image/jpeg".to_string()),
                    file_data: FileData(bytes::Bytes::from_static(b"SECRET-JPEG-BYTES")),
                    file_uid: FileUid(42),
                }],
            }),
            chapters: None,
            tags: vec![Tags {
                crc32: None,
                void: None,
                tag: vec![Tag {
                    crc32: None,
                    void: None,
                    targets: Targets::default(),
                    simple_tag: vec![SimpleTag {
                        crc32: None,
                        void: None,
                        tag_name: TagName("ARTIST".to_string()),
                        tag_language: TagLanguage("und".to_string()),
                        tag_default: TagDefault(1),
                        tag_string: Some(TagString("Alice Smith".to_string())),
                        ..Default::default()
                    }],
                }],
            }],
        };

        let mut buf = Vec::new();
        ebml.write_to(&mut buf).unwrap();
        segment.write_to(&mut buf).unwrap();
        buf
    }

    #[test]
    fn strip_removes_metadata_keeps_tracks_and_clusters() {
        let dirty = dirty_mkv();

        // Sanity: the dirty bytes really do carry the secrets.
        assert!(
            contains(&dirty, b"My Secret Holiday Video"),
            "fixture must carry a Title"
        );
        assert!(
            contains(&dirty, b"mkvmerge v75.0.0 ('SECRET-TOOL')"),
            "fixture must carry a WritingApp"
        );
        assert!(
            contains(&dirty, b"Director Commentary"),
            "fixture must carry a track Name"
        );
        assert!(
            contains(&dirty, b"SECRET-JPEG-BYTES"),
            "fixture must carry an attachment"
        );
        assert!(
            contains(&dirty, b"Alice Smith"),
            "fixture must carry a tag value"
        );

        let cleaned = strip(&dirty).unwrap();

        // (c) The metadata bytes must be ABSENT from the output.
        assert!(
            !contains(&cleaned, b"My Secret Holiday Video"),
            "Title must be gone"
        );
        assert!(
            !contains(&cleaned, b"mkvmerge v75.0.0 ('SECRET-TOOL')"),
            "WritingApp must be blanked"
        );
        assert!(
            !contains(&cleaned, b"libebml v1.4.4 + libmatroska v1.7.1"),
            "MuxingApp must be blanked"
        );
        assert!(
            !contains(&cleaned, b"Main Video Track (Alice's cam)"),
            "track Name must be gone"
        );
        assert!(
            !contains(&cleaned, b"Director Commentary"),
            "track Name must be gone"
        );
        assert!(
            !contains(&cleaned, b"VP9 by alice"),
            "CodecName must be gone"
        );
        assert!(
            !contains(&cleaned, b"SECRET-JPEG-BYTES"),
            "attachment data must be gone"
        );
        assert!(
            !contains(&cleaned, b"cover.jpg"),
            "attachment filename must be gone"
        );
        assert!(
            !contains(&cleaned, b"Alice Smith"),
            "tag value must be gone"
        );
        assert!(
            !contains(&cleaned, b"/home/alice/secret.mkv"),
            "SegmentFilename must be gone"
        );

        // (d) The output must still parse as the SAME container with the
        // same tracks / clusters and the metadata structure normalized
        // (tags / attachments / chapters / date all gone, apps blanked).
        let mut c = Cursor::new(&cleaned[..]);
        let ebml = Ebml::read_from(&mut c).unwrap();
        assert_eq!(
            ebml.doc_type.as_ref().unwrap().0,
            "matroska",
            "DocType must survive"
        );
        let seg = Segment::read_from(&mut c).unwrap();

        // Same number of tracks, same UIDs / codecs (media untouched).
        let tracks = seg.tracks.as_ref().unwrap();
        assert_eq!(tracks.track_entry.len(), 2, "both tracks must survive");
        assert_eq!(*tracks.track_entry[0].track_uid, 0xDEAD_BEEF);
        assert_eq!(tracks.track_entry[0].codec_id.0, "V_VP9");
        assert_eq!(*tracks.track_entry[1].track_uid, 0xCAFE_F00D);
        assert_eq!(tracks.track_entry[1].codec_id.0, "A_OPUS");
        // Names cleared.
        assert!(tracks.track_entry[0].name.is_none());
        assert!(tracks.track_entry[1].name.is_none());
        assert!(tracks.track_entry[0].codec_name.is_none());

        // Same number of clusters (the media frames).
        assert_eq!(seg.cluster.len(), 1, "the cluster (media) must survive");

        // Metadata structure normalized.
        assert!(seg.tags.is_empty(), "Tags must be dropped");
        assert!(seg.attachments.is_none(), "Attachments must be dropped");
        assert!(seg.chapters.is_none(), "Chapters must be dropped");
        assert!(seg.info.date_utc.is_none(), "DateUTC must be dropped");
        assert!(seg.info.title.is_none(), "Title must be dropped");
        assert!(
            seg.info.segment_filename.is_none(),
            "SegmentFilename must be dropped"
        );
        assert_eq!(seg.info.writing_app.0, REMUX_APP, "WritingApp blanked");
        assert_eq!(seg.info.muxing_app.0, REMUX_APP, "MuxingApp blanked");
        // Structural Info preserved.
        assert_eq!(*seg.info.timestamp_scale, 1_000_000);
        assert_eq!(seg.info.duration.as_ref().unwrap().0, 120_000.0);
    }

    #[test]
    fn round_trips_webm_doctype() {
        // A WebM-flavoured EBML header (DocType "webm") must survive intact.
        let ebml = Ebml {
            ebml_max_id_length: EbmlMaxIdLength(4),
            ebml_max_size_length: EbmlMaxSizeLength(8),
            doc_type: Some(DocType("webm".to_string())),
            doc_type_version: Some(DocTypeVersion(2)),
            doc_type_read_version: Some(DocTypeReadVersion(2)),
            ..Default::default()
        };
        let segment = Segment {
            crc32: None,
            void: None,
            seek_head: vec![],
            info: Info {
                timestamp_scale: TimestampScale(1_000_000),
                muxing_app: MuxingApp("whatever".to_string()),
                writing_app: WritingApp("whatever".to_string()),
                ..Default::default()
            },
            cluster: vec![Cluster {
                timestamp: Timestamp(0),
                ..Default::default()
            }],
            tracks: Some(Tracks {
                track_entry: vec![TrackEntry {
                    track_number: TrackNumber(1),
                    track_uid: TrackUid(7),
                    track_type: TrackType(1),
                    codec_id: CodecId("V_VP9".to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            cues: None,
            attachments: None,
            chapters: None,
            tags: vec![],
        };
        let mut dirty = Vec::new();
        ebml.write_to(&mut dirty).unwrap();
        segment.write_to(&mut dirty).unwrap();

        let cleaned = strip(&dirty).unwrap();
        let mut c = Cursor::new(&cleaned[..]);
        let out_ebml = Ebml::read_from(&mut c).unwrap();
        assert_eq!(out_ebml.doc_type.as_ref().unwrap().0, "webm");
        let seg = Segment::read_from(&mut c).unwrap();
        assert_eq!(seg.tracks.as_ref().unwrap().track_entry.len(), 1);
        assert_eq!(seg.cluster.len(), 1);
    }

    #[test]
    fn rejects_non_matroska_input() {
        let err = strip(b"not an mkv file at all").unwrap_err();
        assert!(
            matches!(err, CoreError::ParseError { .. }),
            "non-MKV input must yield ParseError, got {err:?}"
        );
    }

    #[test]
    fn rejects_truncated_segment() {
        // Valid EBML header + a Segment header claiming more body than is
        // present must be a ParseError, never a panic.
        let mut dirty = dirty_mkv();
        // Lop off the tail (inside the Segment body).
        dirty.truncate(dirty.len() - 8);
        let err = strip(&dirty).unwrap_err();
        assert!(
            matches!(err, CoreError::ParseError { .. }),
            "truncated input must yield ParseError, got {err:?}"
        );
    }

    /// Naive substring scan over the raw container bytes.
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        if needle.is_empty() || needle.len() > haystack.len() {
            return false;
        }
        haystack.windows(needle.len()).any(|w| w == needle)
    }
}
