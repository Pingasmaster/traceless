use std::io::Cursor;
#[cfg(feature = "native")]
use std::path::Path;

use lofty::file::TaggedFileExt;
#[cfg(feature = "native")]
use lofty::tag::ItemKey;
use lofty::tag::TagType;

use crate::error::CoreError;
#[cfg(feature = "native")]
use crate::metadata::{MetadataGroup, MetadataItem, MetadataSet};

#[cfg(feature = "native")]
use super::FormatHandler;
#[cfg(feature = "native")]
use super::sandbox;

pub struct AudioHandler;

/// Map a lowercase audio extension to the MIME the cleaner branches on.
/// Returns `None` for an extension this handler does not own.
fn ext_to_mime(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "ogg" => "audio/ogg",
        "opus" => "audio/opus",
        "wav" => "audio/wav",
        "aiff" => "audio/aiff",
        "m4a" => "audio/m4a",
        "mp4" => "audio/mp4",
        "aac" => "audio/aac",
        _ => return None,
    })
}

/// True for the MP4-family containers (MP4 / M4A / AAC) whose full
/// metadata strip requires ffmpeg (non-ilst atoms, freeform GPS, chapter
/// tracks). The native build routes these through bubblewrap+ffmpeg; the
/// wasm build returns [`CoreError::NotImplementedInWasm`] from
/// `clean_bytes` because there is no in-memory MP4 atom rewriter yet.
fn is_ffmpeg_routed(mime: &str) -> bool {
    matches!(
        mime,
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "audio/aac"
    )
}

/// Strip every tag from a lofty-supported audio file, in memory. Handles
/// MP3 / FLAC / OGG / Opus / WAV / AIFF (the formats lofty can rewrite
/// without a subprocess); for FLAC it also blanks the VorbisComments
/// vendor string and drops APPLICATION / PICTURE metadata blocks. The
/// MP4-family containers return [`CoreError::NotImplementedInWasm`]
/// because they need ffmpeg (the native build handles them in
/// `clean_metadata` before reaching here). Shared by the native wrapper
/// and the wasm `inmem` path. `ext` is the lowercase extension (no dot).
///
/// # Errors
///
/// Returns [`CoreError::ParseError`] if the audio file cannot be parsed,
/// [`CoreError::CleanError`] if a tag cannot be removed, [`CoreError::UnsupportedFormat`] for an
/// unknown extension, or [`CoreError::NotImplementedInWasm`] for the
/// MP4-family containers.
pub(crate) fn clean_bytes(input: &[u8], ext: &str) -> Result<Vec<u8>, CoreError> {
    super::check_input_len(input.len())?;
    let empty = std::path::PathBuf::new;
    let Some(mime) = ext_to_mime(ext) else {
        return Err(CoreError::UnsupportedFormat {
            mime_type: format!("audio handler: unknown extension '{ext}'"),
        });
    };

    if is_ffmpeg_routed(mime) {
        // MP4-family audio (m4a/aac/audio-mp4) carries metadata in the same
        // ISO-BMFF user-data atoms as MP4 video (moov/udta, meta/keys/ilst,
        // freeform ©xyz GPS), so it strips through the same pure-Rust
        // box-tree stripper the video path uses. The native build still
        // routes these through ffmpeg in `clean_metadata`.
        #[cfg(feature = "wasm-inmem")]
        {
            return super::inmem_video::isobmff::strip(input);
        }
        #[cfg(not(feature = "wasm-inmem"))]
        {
            return Err(CoreError::NotImplementedInWasm {
                format: format!("audio container '{mime}'"),
            });
        }
    }

    // Enumerate every tag type present, then strip each from the buffer.
    // `TaggedFile::clear()` + save does NOT work (lofty's `save_to`
    // returns early on an empty tag vec, leaving the on-disk tags intact),
    // so we mirror the native per-tag `remove_from` approach over a
    // `Cursor` instead of a `File`.
    let tag_types: Vec<TagType> = {
        // Input does not probe/read as a supported audio file -> ParseError
        // (HTTP 422), not CleanError (500): a malformed/wrong-format body is a
        // client error. The per-tag remove + save failures below stay CleanError.
        let probe = lofty::probe::Probe::new(Cursor::new(input))
            .guess_file_type()
            .map_err(|e| CoreError::ParseError {
                path: empty(),
                detail: format!("Failed to probe audio file: {e}"),
            })?;
        let tagged = probe.read().map_err(|e| CoreError::ParseError {
            path: empty(),
            detail: format!("Failed to read audio file: {e}"),
        })?;
        tagged
            .tags()
            .iter()
            .map(lofty::tag::Tag::tag_type)
            .collect()
    };

    let mut cursor = Cursor::new(input.to_vec());
    for tag_type in tag_types {
        tag_type
            .remove_from(&mut cursor)
            .map_err(|e| CoreError::CleanError {
                path: empty(),
                detail: format!("Failed to remove {tag_type:?} tag: {e}"),
            })?;
    }
    let mut data = cursor.into_inner();

    // FLAC extras: blank the VorbisComments vendor and drop leaky
    // APPLICATION / PICTURE blocks. See the native helpers' rationale.
    if matches!(mime, "audio/flac" | "audio/x-flac") {
        data = blank_flac_vendor_inmem(data)?;
        if let Some(rewritten) = rewrite_flac_without_blocks(&data, FLAC_LEAKY_BLOCK_TYPES) {
            data = rewritten;
        }
    }

    Ok(data)
}

/// In-memory equivalent of `blank_flac_vendor`: rewrite the FLAC
/// VorbisComments block with an empty vendor string over a `Cursor`.
fn blank_flac_vendor_inmem(data: Vec<u8>) -> Result<Vec<u8>, CoreError> {
    use lofty::ogg::VorbisComments;
    use lofty::tag::TagExt;

    let mut cursor = Cursor::new(data);
    let mut vc = VorbisComments::default();
    vc.set_vendor(String::new());
    vc.save_to(&mut cursor, lofty::config::WriteOptions::default())
        .map_err(|e| CoreError::CleanError {
            path: std::path::PathBuf::new(),
            detail: format!("Failed to blank FLAC vendor: {e}"),
        })?;
    Ok(cursor.into_inner())
}

#[cfg(feature = "native")]
impl FormatHandler for AudioHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
        super::check_input_size(path)?;
        let tagged_file = lofty::read_from_path(path).map_err(|e| CoreError::ParseError {
            path: path.to_path_buf(),
            detail: format!("Failed to read audio file: {e}"),
        })?;

        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut items = Vec::new();

        for tag in tagged_file.tags() {
            let tag_type = tag.tag_type();
            let tag_label = format!("{tag_type:?}");

            // Vorbis-based formats (OGG/FLAC/Opus) carry a mandatory
            // vendor string in the comment header. Lofty exposes it as
            // a synthetic `EncoderSoftware` item on the generic Tag,
            // so every file ends up "having" an Encoder metadata entry
            // even after a clean. This matches a quirk of lofty's
            // conversion layer, not a real metadata leak - mat2 (via
            // mutagen) intentionally hides it from its reader output.
            // We do the same here.
            let skip_encoder_software = tag_type == TagType::VorbisComments;

            for item in tag.items() {
                if skip_encoder_software && item.key() == ItemKey::EncoderSoftware {
                    continue;
                }
                let key = item_key_to_string(item.key());
                let value = item.value().text().unwrap_or("(binary data)").to_string();
                items.push(MetadataItem {
                    key: format!("[{tag_label}] {key}"),
                    value,
                });
            }

            // Also capture pictures, including any metadata leaks
            // inside them (an embedded JPEG can carry EXIF/GPS). mat2
            // does this in libmat2/audio.py::FLACParser.get_meta.
            for (i, pic) in tag.pictures().iter().enumerate() {
                items.push(MetadataItem {
                    key: format!("[{tag_label}] Picture #{}", i + 1),
                    value: format!("{:?}, {} bytes", pic.pic_type(), pic.data().len()),
                });
                // Recursively scan the cover art for its own metadata.
                if let Some(inner) = probe_picture_metadata(pic.mime_type(), pic.data()) {
                    for item in inner {
                        items.push(MetadataItem {
                            key: format!("[{tag_label}] Picture #{} → {}", i + 1, item.key),
                            value: item.value,
                        });
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
        // M4A / MP4-audio containers carry metadata in two separate
        // places: the iTunes `ilst` atom tree (which lofty handles
        // cleanly) and user-data atoms outside `ilst` like
        // `moov/udta/meta/keys`, freeform `©xyz` GPS, chapter tracks,
        // and MP4 brands. Lofty only strips `ilst` tags via
        // `TagType::remove_from_path`, so a GPS tag injected via
        // ffmpeg's `-metadata location=` survives the lofty-only
        // clean. Route these formats through the same ffmpeg
        // incantation the video handler uses so every non-codec
        // atom is discarded. Lofty still owns the reader path for
        // these formats because its picture-scanning surface is
        // richer than ffprobe's.
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        if is_ffmpeg_routed(mime.as_ref()) {
            return sandbox::clean_with_ffmpeg(path, output_path);
        }

        // Everything else (MP3 / FLAC / OGG / Opus / WAV / AIFF) is
        // cleaned via lofty over an in-memory buffer; `clean_bytes` is the
        // single source of truth shared with the wasm build (tag-type
        // enumeration + per-tag `remove_from`, plus the FLAC vendor /
        // leaky-block sweeps).
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let data = std::fs::read(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;
        let cleaned = clean_bytes(&data, &ext).map_err(|e| super::repath(e, path))?;
        std::fs::write(output_path, cleaned).map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("Failed to write cleaned audio: {e}"),
        })?;
        Ok(())
    }

    fn supported_mime_types(&self) -> &[&str] {
        &[
            "audio/mpeg",
            "audio/flac",
            "audio/ogg",
            "audio/vorbis",
            "audio/mp4",
            "audio/m4a",
            "audio/x-wav",
            "audio/wav",
            "audio/aac",
            "audio/x-aiff",
            "audio/x-flac",
            "audio/x-m4a",
            "audio/aiff",
            "audio/opus",
        ]
    }
}

/// Take a picture's MIME type and raw bytes and, if we have a matching
/// handler, return its metadata. Returns None on any parse error so
/// the caller can silently skip it. Native-only: it stages the picture
/// to a temp file and runs the path-based reader, which only exists in
/// the native build.
#[cfg(feature = "native")]
fn probe_picture_metadata(
    mime: Option<&lofty::picture::MimeType>,
    data: &[u8],
) -> Option<Vec<MetadataItem>> {
    let (mime_str, ext) = match mime? {
        lofty::picture::MimeType::Jpeg => ("image/jpeg", "jpg"),
        lofty::picture::MimeType::Png => ("image/png", "png"),
        lofty::picture::MimeType::Tiff => ("image/tiff", "tiff"),
        lofty::picture::MimeType::Bmp => ("image/bmp", "bmp"),
        lofty::picture::MimeType::Gif => ("image/gif", "gif"),
        _ => return None,
    };

    // Handlers use the on-disk path to detect MIME, so write to a temp
    // file with the correct extension.
    let tmp = tempfile::Builder::new()
        .prefix("traceless-flac-pic-")
        .suffix(&format!(".{ext}"))
        .tempfile()
        .ok()?;
    std::fs::write(tmp.path(), data).ok()?;

    let handler = crate::format_support::get_handler_for_mime(mime_str)?;
    let set = handler.read_metadata(tmp.path()).ok()?;
    let mut out = Vec::new();
    for group in set.groups {
        for item in group.items {
            out.push(item);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// FLAC metadata block type constants. See the FLAC format spec
/// <https://xiph.org/flac/format.html#metadata_block_header> -
/// the 7-bit block type field uses these values.
const FLAC_BLOCK_APPLICATION: u8 = 2;
const FLAC_BLOCK_PICTURE: u8 = 6;

/// Block types that carry fingerprinting payloads we always want
/// dropped from a cleaned FLAC.
///
/// - `APPLICATION` (type 2): free-form producer slot where mastering
///   tools (shntool, cuetools, Melodyne, ReplayGain scanners) stuff
///   arbitrary identifying payloads. mat2 doesn't explicitly strip
///   these, but they are a real fingerprinting channel.
/// - `PICTURE` (type 6): embedded cover art. In FLAC these live as
///   standalone metadata blocks *outside* the VorbisComments block, so
///   `TagType::VorbisComments::remove_from_path` does not touch them,
///   and the old APPLICATION-only sweep used to leave them in place.
///   A cover JPEG can ship its own EXIF `Artist`/`GPSLatitude` leak
///   (exercised by `flac_reader_recurses_into_embedded_cover` in the
///   parity suite) so the cleaner must drop the whole block, not just
///   strip the tag surface around it.
const FLAC_LEAKY_BLOCK_TYPES: &[u8] = &[FLAC_BLOCK_APPLICATION, FLAC_BLOCK_PICTURE];

/// Pure function: parse `data` as a FLAC stream and return a rewritten
/// copy with every metadata block whose 7-bit type is in `drop_types`
/// removed. Returns `None` when the input is not a valid FLAC stream,
/// or when there are no matching blocks to drop (so the caller can
/// skip the write).
fn rewrite_flac_without_blocks(data: &[u8], drop_types: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 4 || &data[..4] != b"fLaC" {
        return None;
    }

    // First pass: walk the block list, noting where each block body
    // starts and ends. Stop when the last-metadata-block flag is set
    // or the buffer is exhausted. Returns None on any structural
    // damage so we never emit a truncated rewrite.
    let mut blocks: Vec<BlockSlice> = Vec::new();
    let mut pos = 4usize;
    loop {
        if pos + 4 > data.len() {
            return None;
        }
        let header = data[pos];
        let is_last = header & 0x80 != 0;
        let block_type = header & 0x7f;
        let len = (u32::from(data[pos + 1]) << 16)
            | (u32::from(data[pos + 2]) << 8)
            | u32::from(data[pos + 3]);
        let body_start = pos + 4;
        let body_end = body_start.checked_add(len as usize)?;
        if body_end > data.len() {
            return None;
        }
        blocks.push(BlockSlice {
            block_type,
            body_start,
            body_end,
        });
        pos = body_end;
        if is_last {
            break;
        }
    }

    if !blocks.iter().any(|b| drop_types.contains(&b.block_type)) {
        return None;
    }

    // Filter the leaky blocks out. STREAMINFO (type 0) must always be
    // present as the first block per the FLAC spec; if a crafted
    // input has marked STREAMINFO itself with one of the dropped
    // types the filtered list will be empty and the output would be
    // invalid. Bail rather than corrupt.
    let kept: Vec<&BlockSlice> = blocks
        .iter()
        .filter(|b| !drop_types.contains(&b.block_type))
        .collect();
    if kept.is_empty() || kept[0].block_type != 0 {
        return None;
    }

    let audio_frames_start = pos;
    let mut out: Vec<u8> = Vec::with_capacity(data.len());
    out.extend_from_slice(b"fLaC");

    let kept_count = kept.len();
    for (i, block) in kept.iter().enumerate() {
        let is_last = i + 1 == kept_count;
        let body_len = block.body_end - block.body_start;
        // 24-bit big-endian length field.
        let len_bytes = [
            ((body_len >> 16) & 0xff) as u8,
            ((body_len >> 8) & 0xff) as u8,
            (body_len & 0xff) as u8,
        ];
        let header_byte = if is_last {
            block.block_type | 0x80
        } else {
            block.block_type & 0x7f
        };
        out.push(header_byte);
        out.extend_from_slice(&len_bytes);
        out.extend_from_slice(&data[block.body_start..block.body_end]);
    }

    // Copy the audio frames unchanged.
    out.extend_from_slice(&data[audio_frames_start..]);
    Some(out)
}

#[derive(Clone, Copy)]
struct BlockSlice {
    block_type: u8,
    body_start: usize,
    body_end: usize,
}

#[cfg(feature = "native")]
fn item_key_to_string(key: ItemKey) -> String {
    match key {
        ItemKey::TrackTitle => "Title".to_string(),
        ItemKey::TrackArtist => "Artist".to_string(),
        ItemKey::AlbumTitle => "Album".to_string(),
        ItemKey::AlbumArtist => "Album Artist".to_string(),
        ItemKey::TrackNumber => "Track Number".to_string(),
        ItemKey::Year => "Year".to_string(),
        ItemKey::RecordingDate => "Recording Date".to_string(),
        ItemKey::Genre => "Genre".to_string(),
        ItemKey::Comment => "Comment".to_string(),
        ItemKey::Composer => "Composer".to_string(),
        ItemKey::Conductor => "Conductor".to_string(),
        ItemKey::EncoderSoftware => "Encoder".to_string(),
        ItemKey::EncoderSettings => "Encoder Settings".to_string(),
        ItemKey::CopyrightMessage => "Copyright".to_string(),
        ItemKey::Lyrics => "Lyrics".to_string(),
        ItemKey::Publisher => "Publisher".to_string(),
        ItemKey::Remixer => "Remixer".to_string(),
        ItemKey::DiscNumber => "Disc Number".to_string(),
        ItemKey::Bpm => "BPM".to_string(),
        other => format!("{other:?}"),
    }
}

#[cfg(all(test, feature = "native"))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // ---------- item_key_to_string ----------

    #[test]
    fn item_key_to_string_translates_known_keys() {
        assert_eq!(item_key_to_string(ItemKey::TrackTitle), "Title");
        assert_eq!(item_key_to_string(ItemKey::TrackArtist), "Artist");
        assert_eq!(item_key_to_string(ItemKey::AlbumTitle), "Album");
        assert_eq!(item_key_to_string(ItemKey::TrackNumber), "Track Number");
        assert_eq!(item_key_to_string(ItemKey::Year), "Year");
        assert_eq!(item_key_to_string(ItemKey::Genre), "Genre");
        assert_eq!(item_key_to_string(ItemKey::Composer), "Composer");
        assert_eq!(item_key_to_string(ItemKey::EncoderSoftware), "Encoder");
        assert_eq!(
            item_key_to_string(ItemKey::EncoderSettings),
            "Encoder Settings"
        );
        assert_eq!(item_key_to_string(ItemKey::CopyrightMessage), "Copyright");
        assert_eq!(item_key_to_string(ItemKey::Lyrics), "Lyrics");
        assert_eq!(item_key_to_string(ItemKey::Publisher), "Publisher");
        assert_eq!(item_key_to_string(ItemKey::DiscNumber), "Disc Number");
        assert_eq!(item_key_to_string(ItemKey::Bpm), "BPM");
    }

    #[test]
    fn item_key_to_string_falls_back_to_debug_for_unknown() {
        // ItemKey::Description is not in the explicit match but must
        // still produce some non-empty string via the Debug fallback.
        let s = item_key_to_string(ItemKey::Description);
        assert!(!s.is_empty());
    }

    // ---------- probe_picture_metadata: MIME variant coverage ----------

    #[test]
    fn probe_picture_metadata_ignores_missing_mime() {
        assert!(probe_picture_metadata(None, b"anything").is_none());
    }

    #[test]
    fn probe_picture_metadata_ignores_unsupported_mime() {
        // Vorbis tags can carry arbitrary MIME types; anything outside
        // the 5-variant allowlist (JPEG, PNG, TIFF, BMP, GIF) is
        // dropped silently.
        let other = lofty::picture::MimeType::Unknown("image/avif".to_string());
        assert!(probe_picture_metadata(Some(&other), b"junk").is_none());
    }

    #[test]
    fn probe_picture_metadata_handles_garbage_bytes_for_known_mime() {
        // A PNG MIME with a single invalid byte must not panic; the
        // underlying image handler returns an error and the function
        // propagates `None`.
        let mime = lofty::picture::MimeType::Png;
        assert!(probe_picture_metadata(Some(&mime), &[0xff]).is_none());
    }

    // ---------- AudioHandler supported_mime_types ----------

    #[test]
    fn audio_handler_lists_every_routed_mime() {
        let claimed: Vec<&&str> = AudioHandler.supported_mime_types().iter().collect();
        for required in [
            "audio/mpeg",
            "audio/flac",
            "audio/x-flac",
            "audio/ogg",
            "audio/vorbis",
            "audio/mp4",
            "audio/m4a",
            "audio/x-m4a",
            "audio/wav",
            "audio/x-wav",
            "audio/aac",
            "audio/aiff",
            "audio/x-aiff",
            "audio/opus",
        ] {
            assert!(
                claimed.contains(&&required),
                "AudioHandler must claim {required}, got {claimed:?}"
            );
        }
    }

    // ---------- blank_flac_vendor_inmem: smoke test on non-flac bytes ------

    #[test]
    fn blank_flac_vendor_inmem_returns_err_for_non_flac() {
        // Feeding plain bytes that aren't a FLAC must return Err rather
        // than panic. The exact error message depends on lofty.
        let result = blank_flac_vendor_inmem(b"this is not flac bytes".to_vec());
        assert!(result.is_err());
    }

    // ---------- read_metadata on a non-audio file ----------

    #[test]
    fn read_metadata_on_non_audio_returns_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake.mp3");
        std::fs::write(&path, b"definitely not an mp3 file").unwrap();
        let err = AudioHandler
            .read_metadata(&path)
            .expect_err("non-audio must not parse");
        matches!(err, CoreError::ParseError { .. });
    }

    #[test]
    fn read_metadata_on_empty_file_returns_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.mp3");
        std::fs::write(&path, b"").unwrap();
        let err = AudioHandler
            .read_metadata(&path)
            .expect_err("empty file must not parse");
        matches!(err, CoreError::ParseError { .. });
    }

    // ---------- FLAC APPLICATION block walker ----------

    /// Build the smallest possible synthetic FLAC stream with a
    /// STREAMINFO block, zero or more intermediate blocks, and a
    /// trailing 8-byte "audio frames" payload. Used by the walker tests
    /// so we don't depend on ffmpeg for pure structural coverage.
    fn synth_flac(extra_blocks: &[(u8, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"fLaC");

        // STREAMINFO block: type 0, 34-byte body filled with zeros.
        // The body is structurally bogus but the walker only looks at
        // block types + lengths, so zeros suffice.
        let streaminfo_body = vec![0u8; 34];
        let is_last = extra_blocks.is_empty();
        push_block(&mut out, 0, is_last, &streaminfo_body);

        for (i, (block_type, body)) in extra_blocks.iter().enumerate() {
            let is_last = i + 1 == extra_blocks.len();
            push_block(&mut out, *block_type, is_last, body);
        }

        // Fake audio frames; any bytes work since the walker never
        // dereferences them as codec data.
        out.extend_from_slice(b"AUDIO456");
        out
    }

    fn push_block(out: &mut Vec<u8>, block_type: u8, is_last: bool, body: &[u8]) {
        let header = if is_last {
            block_type | 0x80
        } else {
            block_type
        };
        out.push(header);
        let len = u32::try_from(body.len()).unwrap();
        out.push(((len >> 16) & 0xff) as u8);
        out.push(((len >> 8) & 0xff) as u8);
        out.push((len & 0xff) as u8);
        out.extend_from_slice(body);
    }

    #[test]
    fn rewrite_flac_returns_none_on_non_flac_magic() {
        assert!(rewrite_flac_without_blocks(b"oggS...", FLAC_LEAKY_BLOCK_TYPES).is_none());
        assert!(rewrite_flac_without_blocks(b"", FLAC_LEAKY_BLOCK_TYPES).is_none());
    }

    #[test]
    fn rewrite_flac_returns_none_when_no_leaky_blocks() {
        // STREAMINFO + PADDING + audio frames, no APPLICATION or
        // PICTURE anywhere.
        let flac = synth_flac(&[(1, &[0u8; 16])]);
        assert!(
            rewrite_flac_without_blocks(&flac, FLAC_LEAKY_BLOCK_TYPES).is_none(),
            "no-op case must skip the write"
        );
    }

    #[test]
    fn rewrite_flac_drops_application_block_and_rewires_last_flag() {
        // Layout: STREAMINFO | APPLICATION("XXXX" + payload) | audio.
        // The APPLICATION block is currently the last metadata block
        // (is_last=1 on its header). After removal, STREAMINFO must
        // inherit the last-flag so decoders stop parsing metadata
        // before walking into the audio frames.
        let app_body = b"XXXXsecret-producer-data";
        let flac = synth_flac(&[(FLAC_BLOCK_APPLICATION, app_body)]);
        let rewritten =
            rewrite_flac_without_blocks(&flac, FLAC_LEAKY_BLOCK_TYPES).expect("should rewrite");

        // Magic preserved.
        assert_eq!(&rewritten[..4], b"fLaC");

        // STREAMINFO header byte = type 0 | last-flag 0x80 = 0x80.
        assert_eq!(
            rewritten[4], 0x80,
            "STREAMINFO must become the new last metadata block"
        );

        // APPLICATION body must not appear anywhere in the rewrite.
        assert!(
            !rewritten.windows(app_body.len()).any(|w| w == app_body),
            "APPLICATION body survived the rewrite"
        );

        // Audio frames preserved at the tail.
        assert!(rewritten.ends_with(b"AUDIO456"));
    }

    #[test]
    fn rewrite_flac_drops_application_in_the_middle() {
        // STREAMINFO | APPLICATION | VORBIS_COMMENT | audio.
        // Dropping the middle block must leave STREAMINFO as a
        // non-last block and VORBIS_COMMENT as the last.
        let vc_body = b"junk-vorbis-body";
        let flac = synth_flac(&[(FLAC_BLOCK_APPLICATION, b"APP1payload"), (4, vc_body)]);
        let rewritten =
            rewrite_flac_without_blocks(&flac, FLAC_LEAKY_BLOCK_TYPES).expect("should rewrite");

        // First kept block (STREAMINFO, type 0) has last-flag clear.
        assert_eq!(rewritten[4] & 0x80, 0);
        // Walk to the next header: 4 magic + 4 streaminfo header + 34 body = 42.
        let second_header_idx = 4 + 4 + 34;
        assert_eq!(
            rewritten[second_header_idx] & 0x7f,
            4,
            "second kept block must be VORBIS_COMMENT"
        );
        assert_eq!(
            rewritten[second_header_idx] & 0x80,
            0x80,
            "VORBIS_COMMENT must now be the last metadata block"
        );
    }

    #[test]
    fn rewrite_flac_returns_none_on_truncated_block_body() {
        // Header claims a 100-byte APPLICATION body but we only ship 4 bytes.
        let mut flac = Vec::new();
        flac.extend_from_slice(b"fLaC");
        flac.push(FLAC_BLOCK_APPLICATION | 0x80);
        flac.extend_from_slice(&[0x00, 0x00, 0x64]); // 100 bytes
        flac.extend_from_slice(b"abcd");
        assert!(
            rewrite_flac_without_blocks(&flac, FLAC_LEAKY_BLOCK_TYPES).is_none(),
            "truncated body must not corrupt the file"
        );
    }

    #[test]
    fn rewrite_flac_returns_none_when_streaminfo_missing_after_filter() {
        // A pathological fixture where the *first* block is typed as
        // APPLICATION. Dropping it would leave the FLAC stream without
        // a STREAMINFO, which is invalid per the spec. The walker must
        // bail rather than emit that.
        let mut flac = Vec::new();
        flac.extend_from_slice(b"fLaC");
        // Single APPLICATION block with is_last=1.
        flac.push(FLAC_BLOCK_APPLICATION | 0x80);
        flac.extend_from_slice(&[0x00, 0x00, 0x04]);
        flac.extend_from_slice(b"APP1");
        flac.extend_from_slice(b"AUDIO");
        assert!(
            rewrite_flac_without_blocks(&flac, FLAC_LEAKY_BLOCK_TYPES).is_none(),
            "rewriting must not emit a FLAC without STREAMINFO"
        );
    }

    #[test]
    fn rewrite_flac_drops_picture_block() {
        // Regression for HIGH-4: a FLAC PICTURE block (type 6) lives
        // as a standalone metadata block outside the VorbisComments
        // block, so `TagType::VorbisComments::remove_from_path`
        // doesn't touch it. Before the fix, a cleaned FLAC still
        // carried its embedded cover art (which can itself ship EXIF
        // Artist / GPS leaks). After the fix, the generalised block
        // sweep drops PICTURE alongside APPLICATION.
        //
        // Layout: STREAMINFO | PICTURE("secret-cover-bytes") | audio.
        let pic_body = b"secret-cover-bytes-with-EXIF-and-GPS";
        let flac = synth_flac(&[(FLAC_BLOCK_PICTURE, pic_body)]);
        let rewritten = rewrite_flac_without_blocks(&flac, FLAC_LEAKY_BLOCK_TYPES)
            .expect("picture-carrying FLAC should be rewritten");

        // Magic preserved, STREAMINFO is now the last metadata block.
        assert_eq!(&rewritten[..4], b"fLaC");
        assert_eq!(
            rewritten[4], 0x80,
            "STREAMINFO must become the new last metadata block"
        );
        // The PICTURE body must not survive anywhere in the output.
        assert!(
            !rewritten.windows(pic_body.len()).any(|w| w == pic_body),
            "PICTURE body survived the rewrite"
        );
        // Audio frames preserved at the tail.
        assert!(rewritten.ends_with(b"AUDIO456"));
    }

    #[test]
    fn rewrite_flac_drops_picture_and_application_together() {
        // STREAMINFO | PICTURE | APPLICATION | VORBIS_COMMENT | audio.
        // Both leaky types must be dropped in one pass, leaving only
        // STREAMINFO + VORBIS_COMMENT, with VORBIS_COMMENT correctly
        // marked as the last metadata block.
        let pic = b"picture-bytes";
        let app = b"application-bytes";
        let vc = b"vorbis-bytes";
        let flac = synth_flac(&[
            (FLAC_BLOCK_PICTURE, pic),
            (FLAC_BLOCK_APPLICATION, app),
            (4, vc),
        ]);
        let rewritten =
            rewrite_flac_without_blocks(&flac, FLAC_LEAKY_BLOCK_TYPES).expect("should rewrite");

        assert!(
            !rewritten.windows(pic.len()).any(|w| w == pic),
            "picture body leaked"
        );
        assert!(
            !rewritten.windows(app.len()).any(|w| w == app),
            "application body leaked"
        );

        // Second kept block (VORBIS_COMMENT, type 4) must now be
        // marked as the last metadata block.
        let second_header_idx = 4 + 4 + 34;
        assert_eq!(rewritten[second_header_idx] & 0x7f, 4);
        assert_eq!(
            rewritten[second_header_idx] & 0x80,
            0x80,
            "VORBIS_COMMENT must now be the last metadata block"
        );
    }
}
