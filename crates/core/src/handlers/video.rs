use std::path::Path;

use crate::error::CoreError;
use crate::metadata::{MetadataGroup, MetadataItem, MetadataSet};

use super::FormatHandler;
use super::sandbox;

pub struct VideoHandler;

impl FormatHandler for VideoHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
        sandbox::check_tool_available("ffprobe")?;

        let mut cmd = sandbox::sandboxed_probe_command("ffprobe", path);
        cmd.args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path);
        let output = cmd.output().map_err(|e| CoreError::ToolFailed {
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
            set.groups.push(MetadataGroup { filename, items });
        }
        Ok(set)
    }

    fn clean_metadata(&self, path: &Path, output_path: &Path) -> Result<(), CoreError> {
        sandbox::clean_with_ffmpeg(path, output_path)
    }

    fn supported_mime_types(&self) -> &[&str] {
        &[
            "video/mp4",
            "video/x-matroska",
            "video/webm",
            "video/x-msvideo",
            "video/avi",
            "video/quicktime",
            "video/x-ms-wmv",
            "video/x-flv",
            "video/ogg",
        ]
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // ---------- parse_json_kv ----------

    #[test]
    fn parse_json_kv_basic_pair() {
        let (k, v) = parse_json_kv("\"title\": \"Hello\"").unwrap();
        assert_eq!(k, "title");
        assert_eq!(v, "Hello");
    }

    #[test]
    fn parse_json_kv_trailing_comma() {
        let (k, v) = parse_json_kv("\"author\": \"Alice\",").unwrap();
        assert_eq!(k, "author");
        assert_eq!(v, "Alice");
    }

    #[test]
    fn parse_json_kv_rejects_no_colon() {
        assert!(parse_json_kv("\"no colon here\"").is_none());
    }

    #[test]
    fn parse_json_kv_rejects_empty_key() {
        assert!(parse_json_kv("\"\": \"value\"").is_none());
    }

    #[test]
    fn parse_json_kv_rejects_empty_value() {
        assert!(parse_json_kv("\"key\": \"\"").is_none());
    }

    #[test]
    fn parse_json_kv_splits_on_first_colon_only() {
        // Values containing a colon (e.g. URLs, timestamps) must not
        // be truncated at the second colon. `splitn(2, ':')` enforces
        // this.
        let (k, v) = parse_json_kv("\"url\": \"https://example.com\"").unwrap();
        assert_eq!(k, "url");
        assert_eq!(v, "https://example.com");
    }

    #[test]
    fn parse_json_kv_handles_leading_whitespace() {
        let (k, v) = parse_json_kv("      \"indent\": \"deep\"").unwrap();
        assert_eq!(k, "indent");
        assert_eq!(v, "deep");
    }

    // ---------- parse_ffprobe_json ----------

    #[test]
    fn parse_ffprobe_json_extracts_simple_tags_block() {
        let json = r#"{
  "format": {
    "filename": "foo.mp4",
    "tags": {
      "title": "Hello",
      "author": "Alice"
    }
  }
}"#;
        let items = parse_ffprobe_json(json);
        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|i| i.key == "title" && i.value == "Hello"));
        assert!(
            items
                .iter()
                .any(|i| i.key == "author" && i.value == "Alice")
        );
    }

    #[test]
    fn parse_ffprobe_json_ignores_outer_filename_field() {
        // `filename` appears at the `format` level, not under `tags`.
        // The parser must only collect entries from inside `tags` so
        // the outer `filename` is not surfaced.
        let json = r#"{
  "format": {
    "filename": "leak.mp4",
    "tags": {
      "encoder": "Lavf60"
    }
  }
}"#;
        let items = parse_ffprobe_json(json);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].key, "encoder");
        assert_eq!(items[0].value, "Lavf60");
    }

    #[test]
    fn parse_ffprobe_json_handles_multiple_tags_blocks() {
        // Two streams + one format, each with its own tags block.
        let json = r#"{
  "streams": [
    {
      "codec_name": "h264",
      "tags": {
        "language": "eng"
      }
    },
    {
      "codec_name": "aac",
      "tags": {
        "language": "und"
      }
    }
  ],
  "format": {
    "tags": {
      "title": "My Video"
    }
  }
}"#;
        let items = parse_ffprobe_json(json);
        assert!(
            items
                .iter()
                .any(|i| i.key == "title" && i.value == "My Video")
        );
        assert!(items.iter().filter(|i| i.key == "language").count() == 2);
    }

    #[test]
    fn parse_ffprobe_json_on_empty_input_returns_empty() {
        assert!(parse_ffprobe_json("").is_empty());
    }

    #[test]
    fn parse_ffprobe_json_on_non_json_returns_empty() {
        // Any text that doesn't contain `"tags"` produces no items.
        assert!(parse_ffprobe_json("this is not json at all").is_empty());
    }

    #[test]
    fn parse_ffprobe_json_when_tags_literal_appears_in_value() {
        // `"tags"` inside a string value should not trigger collection
        // unless the same line also contains an opening brace,
        // mimicking how ffprobe actually formats the section header.
        //
        // Note: this is an accepted limitation of the hand-rolled
        // parser. The parser opens a tags block only on a line that
        // contains both `"tags"` and `{`, so a standalone value like
        // `"comment": "some tags here"` does not false-positive.
        let json = r#"{
  "format": {
    "tags": {
      "comment": "has tags in value"
    }
  }
}"#;
        let items = parse_ffprobe_json(json);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].key, "comment");
    }

    // ---------- supported_mime_types ----------

    #[test]
    fn video_handler_supports_every_expected_mime() {
        let mimes: Vec<&&str> = VideoHandler.supported_mime_types().iter().collect();
        for required in [
            "video/mp4",
            "video/x-matroska",
            "video/webm",
            "video/x-msvideo",
            "video/avi",
            "video/quicktime",
            "video/x-ms-wmv",
            "video/x-flv",
            "video/ogg",
        ] {
            assert!(
                mimes.contains(&&required),
                "VideoHandler must claim {required}, got {mimes:?}"
            );
        }
    }
}
