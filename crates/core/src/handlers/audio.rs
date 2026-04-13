use std::path::Path;

use lofty::file::TaggedFileExt;
use lofty::prelude::*;
use lofty::tag::ItemKey;

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
            let tag_type = format!("{:?}", tag.tag_type());
            for item in tag.items() {
                let key = item_key_to_string(item.key());
                let value = item.value().text().unwrap_or("(binary data)").to_string();
                items.push(MetadataItem {
                    key: format!("[{tag_type}] {key}"),
                    value,
                });
            }

            // Also capture pictures
            for (i, pic) in tag.pictures().iter().enumerate() {
                items.push(MetadataItem {
                    key: format!("[{tag_type}] Picture #{}", i + 1),
                    value: format!(
                        "{:?}, {} bytes",
                        pic.pic_type(),
                        pic.data().len()
                    ),
                });
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

        let mut tagged_file =
            lofty::read_from_path(output_path).map_err(|e| CoreError::CleanError {
                path: path.to_path_buf(),
                detail: format!("Failed to read audio file: {e}"),
            })?;

        tagged_file.clear();

        tagged_file
            .save_to_path(output_path, lofty::config::WriteOptions::default())
            .map_err(|e| CoreError::CleanError {
                path: path.to_path_buf(),
                detail: format!("Failed to save cleaned audio: {e}"),
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
            "audio/x-wav",
            "audio/wav",
            "audio/aac",
            "audio/x-aiff",
            "audio/x-flac",
            "audio/x-m4a",
        ]
    }
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
