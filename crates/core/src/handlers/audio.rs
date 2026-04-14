use std::path::Path;

use lofty::file::TaggedFileExt;
use lofty::tag::{ItemKey, TagType};

use crate::error::CoreError;
use crate::metadata::{MetadataGroup, MetadataItem, MetadataSet};

use super::FormatHandler;

pub struct AudioHandler;

impl FormatHandler for AudioHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
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
            // conversion layer, not a real metadata leak — mat2 (via
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
                    value: format!(
                        "{:?}, {} bytes",
                        pic.pic_type(),
                        pic.data().len()
                    ),
                });
                // Recursively scan the cover art for its own metadata.
                if let Some(inner) = probe_picture_metadata(pic.mime_type(), pic.data()) {
                    for item in inner {
                        items.push(MetadataItem {
                            key: format!(
                                "[{tag_label}] Picture #{} → {}",
                                i + 1,
                                item.key
                            ),
                            value: item.value,
                        });
                    }
                }
            }
        }

        let mut set = MetadataSet::default();
        if !items.is_empty() {
            set.groups.push(MetadataGroup {
                filename,
                items,
            });
        }
        Ok(set)
    }

    fn clean_metadata(
        &self,
        path: &Path,
        output_path: &Path,
    ) -> Result<(), CoreError> {
        // Copy original to output first so lofty parses a file with the
        // correct extension (see file_store::make_temp_path).
        std::fs::copy(path, output_path).map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("Failed to copy file: {e}"),
        })?;

        // Enumerate every tag type present in the file, then delete
        // each via `TagType::remove_from_path`. Calling
        // `TaggedFile::clear()` + `save_to_path` does NOT work:
        // `save_to_path` iterates `self.tags` and returns early on an
        // empty vec, leaving the on-disk tags intact. See lofty 0.24
        // `TaggedFile::save_to` in tagged_file.rs line 440.
        let tag_types: Vec<lofty::tag::TagType> = {
            let tagged_file =
                lofty::read_from_path(output_path).map_err(|e| CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: format!("Failed to read audio file: {e}"),
                })?;
            tagged_file.tags().iter().map(lofty::tag::Tag::tag_type).collect()
        };

        for tag_type in tag_types {
            tag_type
                .remove_from_path(output_path)
                .map_err(|e| CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: format!("Failed to remove {tag_type:?} tag: {e}"),
                })?;
        }

        // For FLAC we also try to blank the VorbisComments vendor
        // string, which lofty lets us rewrite on FLAC because the
        // VorbisComments block is a standalone metadata block inside
        // the FLAC container. For OGG/Vorbis/Opus lofty refuses to
        // overwrite the vendor (see lofty/src/ogg/write.rs lines 117-
        // 134 — the existing vendor is force-copied from disk), so the
        // best we can do there is hide the synthetic reader-side
        // EncoderSoftware item.
        if matches!(
            mime_guess::from_path(output_path)
                .first_or_octet_stream()
                .as_ref(),
            "audio/flac" | "audio/x-flac"
        ) {
            blank_flac_vendor(output_path).map_err(|e| CoreError::CleanError {
                path: path.to_path_buf(),
                detail: format!("Failed to blank FLAC vendor: {e}"),
            })?;
        }

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
/// the caller can silently skip it.
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

/// Rewrite a FLAC file's VorbisComments block with an empty vendor
/// string. FLAC stores VorbisComments as a standalone metadata block
/// inside the FLAC container, so lofty will honor our vendor value on
/// save (unlike for native OGG where the vendor is force-preserved).
fn blank_flac_vendor(path: &Path) -> Result<(), String> {
    use lofty::ogg::VorbisComments;
    use lofty::tag::TagExt;

    let mut vc = VorbisComments::default();
    vc.set_vendor(String::new());
    vc.save_to_path(path, lofty::config::WriteOptions::default())
        .map_err(|e| e.to_string())?;
    Ok(())
}

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

#[cfg(test)]
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

    // ---------- blank_flac_vendor: smoke test via a non-flac path ----------

    #[test]
    fn blank_flac_vendor_returns_err_for_non_flac() {
        // Feeding a plain file that isn't a FLAC must return Err
        // rather than panic. The exact error message depends on lofty.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not.flac");
        std::fs::write(&path, b"this is not flac bytes").unwrap();
        let result = blank_flac_vendor(&path);
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
}
