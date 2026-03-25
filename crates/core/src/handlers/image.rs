use std::fs;
use std::io::{BufWriter, Cursor};
use std::path::Path;

use img_parts::jpeg::Jpeg;
use img_parts::{DynImage, ImageEXIF, ImageICC};
use little_exif::metadata::Metadata as ExifMetadata;

use crate::error::CoreError;
use crate::metadata::{MetadataGroup, MetadataItem, MetadataSet};

use super::FormatHandler;

pub struct ImageHandler;

impl FormatHandler for ImageHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut items = Vec::new();

        // Read EXIF tags via little_exif (iterate the Metadata struct)
        match ExifMetadata::new_from_path(path) {
            Ok(exif) => {
                for tag in &exif {
                    let tag_str = format!("{tag:?}");
                    // Debug output includes the value, extract tag name and value
                    if let Some((name, value)) = split_debug_tag(&tag_str) {
                        items.push(MetadataItem {
                            key: name,
                            value,
                        });
                    } else {
                        items.push(MetadataItem {
                            key: tag_str.clone(),
                            value: String::new(),
                        });
                    }
                }
            }
            Err(e) => {
                log::debug!("No EXIF data or parse error for {}: {e}", path.display());
            }
        }

        // Check for additional metadata segments via img-parts
        let data = fs::read(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        match DynImage::from_bytes(data.clone().into()) {
            Ok(Some(img)) => {
                if img.icc_profile().is_some() {
                    items.push(MetadataItem {
                        key: "ICC Profile".to_string(),
                        value: "present".to_string(),
                    });
                }
                if img.exif().is_some() && items.is_empty() {
                    items.push(MetadataItem {
                        key: "EXIF data".to_string(),
                        value: "present (could not parse individual tags)".to_string(),
                    });
                }
            }
            Ok(None) => {}
            Err(e) => {
                log::debug!("img-parts parse error for {}: {e}", path.display());
            }
        }

        // Check for XMP/IPTC in JPEG
        if let Ok(jpeg) = Jpeg::from_bytes(data.into()) {
            for segment in jpeg.segments() {
                let marker = segment.marker();
                let seg_data = segment.contents();
                if marker == 0xE1 && seg_data.starts_with(b"http://ns.adobe.com/xap/1.0/\0") {
                    items.push(MetadataItem {
                        key: "XMP data".to_string(),
                        value: "present".to_string(),
                    });
                }
                if marker == 0xED {
                    items.push(MetadataItem {
                        key: "IPTC/Photoshop data".to_string(),
                        value: "present".to_string(),
                    });
                }
            }
        }

        let mut set = MetadataSet::default();
        if !items.is_empty() {
            set.groups.push(MetadataGroup { filename, items });
        }
        Ok(set)
    }

    fn clean_metadata(
        &self,
        path: &Path,
        output_path: &Path,
        lightweight: bool,
    ) -> Result<(), CoreError> {
        if lightweight {
            // Lightweight: copy file, then clear EXIF metadata via little_exif
            fs::copy(path, output_path).map_err(|e| CoreError::CleanError {
                path: path.to_path_buf(),
                detail: format!("Failed to copy file: {e}"),
            })?;
            ExifMetadata::file_clear_metadata(output_path).map_err(|e| CoreError::CleanError {
                path: path.to_path_buf(),
                detail: format!("Failed to clear EXIF: {e}"),
            })?;
            return Ok(());
        }

        // Full clean: strip all metadata segments via img-parts
        let data = fs::read(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        match DynImage::from_bytes(data.into()) {
            Ok(Some(mut img)) => {
                img.set_exif(None);
                img.set_icc_profile(None);

                // For JPEG, also strip APP13 (IPTC), XMP, COM segments
                let mut buf = Vec::new();
                img.encoder()
                    .write_to(&mut BufWriter::new(Cursor::new(&mut buf)))
                    .map_err(|e| CoreError::CleanError {
                        path: path.to_path_buf(),
                        detail: format!("Failed to encode cleaned image: {e}"),
                    })?;

                // If JPEG, do an additional pass to strip remaining APP markers
                let mime = mime_guess::from_path(path).first_or_octet_stream();
                let final_data = if mime == "image/jpeg" {
                    strip_jpeg_extra_segments(&buf).unwrap_or(buf)
                } else {
                    buf
                };

                fs::write(output_path, final_data).map_err(|e| CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: format!("Failed to write output: {e}"),
                })?;
            }
            Ok(None) => {
                return Err(CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: "Could not parse image".to_string(),
                });
            }
            Err(e) => {
                return Err(CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: format!("Image parse error: {e}"),
                });
            }
        }

        Ok(())
    }

    fn supported_mime_types(&self) -> &[&str] {
        &["image/jpeg", "image/png", "image/webp"]
    }
}

/// Strip APP1-APP15 and COM segments from JPEG data using img-parts.
fn strip_jpeg_extra_segments(data: &[u8]) -> Option<Vec<u8>> {
    let mut jpeg = Jpeg::from_bytes(data.to_vec().into()).ok()?;

    // Remove APP1-APP15 markers (0xE1-0xEF) and COM (0xFE)
    for marker in 0xE1u8..=0xEF {
        jpeg.remove_segments_by_marker(marker);
    }
    jpeg.remove_segments_by_marker(0xFE); // COM

    let mut buf = Vec::new();
    jpeg.encoder()
        .write_to(&mut BufWriter::new(Cursor::new(&mut buf)))
        .ok()?;
    Some(buf)
}

/// Split a Debug-formatted ExifTag string like `ImageDescription("Hello")`
/// into (name, value).
fn split_debug_tag(debug: &str) -> Option<(String, String)> {
    let paren = debug.find('(')?;
    let name = debug[..paren].to_string();
    let inner = debug[paren + 1..].trim_end_matches(')');
    // Remove surrounding quotes if present
    let value = inner.trim_matches('"').to_string();
    Some((name, value))
}
