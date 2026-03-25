use std::path::Path;
use std::process::Command;

use crate::error::CoreError;
use crate::metadata::{MetadataGroup, MetadataItem, MetadataSet};

use super::FormatHandler;

pub struct VideoHandler;

impl FormatHandler for VideoHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
        check_ffmpeg_available()?;

        let output = Command::new("ffprobe")
            .args([
                "-v", "quiet",
                "-print_format", "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(path)
            .output()
            .map_err(|e| CoreError::ToolFailed {
                tool: "ffprobe".to_string(),
                detail: format!("Failed to run ffprobe: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CoreError::ToolFailed {
                tool: "ffprobe".to_string(),
                detail: format!("ffprobe failed: {stderr}"),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let items = parse_ffprobe_json(&stdout);

        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

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
        lightweight: bool,
    ) -> Result<(), CoreError> {
        check_ffmpeg_available()?;

        let mut args = vec![
            "-y".to_string(),
            "-i".to_string(),
            path.to_string_lossy().into_owned(),
            "-map_metadata".to_string(),
            "-1".to_string(),
        ];

        if lightweight {
            // Keep some structural metadata, just remove user-facing tags
            args.extend([
                "-c".to_string(),
                "copy".to_string(),
                "-map_metadata:s".to_string(),
                "0:s".to_string(), // keep stream metadata
            ]);
        } else {
            // Full strip: copy streams, discard all metadata and chapters
            args.extend([
                "-c".to_string(),
                "copy".to_string(),
                "-map_chapters".to_string(),
                "-1".to_string(),
            ]);
        }

        args.push(output_path.to_string_lossy().into_owned());

        let output = Command::new("ffmpeg")
            .args(&args)
            .output()
            .map_err(|e| CoreError::ToolFailed {
                tool: "ffmpeg".to_string(),
                detail: format!("Failed to run ffmpeg: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CoreError::ToolFailed {
                tool: "ffmpeg".to_string(),
                detail: format!("ffmpeg failed: {stderr}"),
            });
        }

        Ok(())
    }

    fn supported_mime_types(&self) -> &[&str] {
        &[
            "video/mp4",
            "video/x-matroska",
            "video/webm",
            "video/x-msvideo",
            "video/avi",
            "video/quicktime",
        ]
    }
}

fn check_ffmpeg_available() -> Result<(), CoreError> {
    let result = Command::new("ffmpeg").arg("-version").output();
    match result {
        Ok(output) if output.status.success() => Ok(()),
        _ => Err(CoreError::ToolNotFound {
            tool: "ffmpeg".to_string(),
        }),
    }
}

/// Parse ffprobe JSON output to extract metadata tags.
fn parse_ffprobe_json(json_str: &str) -> Vec<MetadataItem> {
    let mut items = Vec::new();

    // Simple JSON parsing without serde: look for "tags" objects
    // Format: "key": "value" within tags blocks
    let mut in_tags = false;
    let mut brace_depth = 0;

    for line in json_str.lines() {
        let trimmed = line.trim();

        if trimmed.contains("\"tags\"") && trimmed.contains('{') {
            in_tags = true;
            brace_depth = 1;
            continue;
        }

        if in_tags {
            if trimmed.contains('{') {
                brace_depth += 1;
            }
            if trimmed.contains('}') {
                brace_depth -= 1;
                if brace_depth == 0 {
                    in_tags = false;
                    continue;
                }
            }

            // Parse "key": "value" pairs
            if let Some((key, value)) = parse_json_kv(trimmed) {
                items.push(MetadataItem { key, value });
            }
        }
    }

    items
}

fn parse_json_kv(line: &str) -> Option<(String, String)> {
    let line = line.trim().trim_end_matches(',');
    let parts: Vec<&str> = line.splitn(2, ':').collect();
    if parts.len() != 2 {
        return None;
    }
    let key = parts[0].trim().trim_matches('"').to_string();
    let value = parts[1].trim().trim_matches('"').to_string();
    if key.is_empty() || value.is_empty() {
        return None;
    }
    Some((key, value))
}
