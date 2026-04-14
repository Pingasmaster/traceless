use std::fs::File;
use std::io::{BufReader, Cursor, Read, Write};
use std::path::Path;

use img_parts::jpeg::Jpeg;
use img_parts::png::Png;
use img_parts::webp::WebP;
use img_parts::{DynImage, ImageEXIF, ImageICC};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use zip::ZipArchive;

use crate::error::CoreError;
use crate::metadata::{MetadataGroup, MetadataItem, MetadataSet};

use super::{epub, odf, ooxml, zip_util, FormatHandler};

pub struct DocumentHandler;

/// Which archive-family this member set belongs to. We decide this once
/// at the start of `clean_metadata` by peeking at the file list and then
/// dispatch each entry accordingly.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum ArchiveKind {
    Ooxml, // DOCX, XLSX, PPTX
    Odf,   // ODT, ODS, ODP, ODG, ODF
    Epub,
    /// Unknown zip-based document — clean conservatively (normalize zip
    /// metadata, strip embedded media EXIF) but don't touch XML contents.
    Generic,
}

impl FormatHandler for DocumentHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
        let file = File::open(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        let mut archive = ZipArchive::new(BufReader::new(file)).map_err(|e| {
            CoreError::ParseError {
                path: path.to_path_buf(),
                detail: format!("Not a valid ZIP archive: {e}"),
            }
        })?;

        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut set = MetadataSet::default();

        // Paths we know carry metadata, probed in order of frequency.
        let meta_paths = [
            "docProps/core.xml",
            "docProps/app.xml",
            "docProps/custom.xml",
            "meta.xml",
            "content.opf",
            "OEBPS/content.opf",
            "OPS/content.opf",
        ];

        for meta_path in meta_paths {
            if let Ok(mut entry) = archive.by_name(meta_path) {
                let mut contents = String::new();
                if entry.read_to_string(&mut contents).is_ok() {
                    let items = parse_xml_metadata(&contents);
                    if !items.is_empty() {
                        set.groups.push(MetadataGroup {
                            filename: format!("{filename}/{meta_path}"),
                            items,
                        });
                    }
                }
            }
        }

        // Flag embedded media (the caller doesn't see the inner EXIF,
        // but we at least tell them it's there).
        let mut embedded_media_count = 0usize;
        let mut revision_hits = 0usize;
        let mut comment_hits = 0usize;
        for i in 0..archive.len() {
            if let Ok(entry) = archive.by_index(i) {
                let name = entry.name();
                if zip_util::is_cleanable_media(name).is_some() {
                    embedded_media_count += 1;
                }
                if name.ends_with("word/document.xml") {
                    // Cheap textual probe for rsid / tracked-changes
                    // markers without a full XML parse.
                    let mut sample = String::new();
                    let _ = entry.take(256 * 1024).read_to_string(&mut sample);
                    if sample.contains(":rsid") || sample.contains(" w:rsid") {
                        revision_hits += 1;
                    }
                    if sample.contains("w:del ") || sample.contains("w:ins ") {
                        revision_hits += 1;
                    }
                    if sample.contains("commentReference") || sample.contains("commentRangeStart") {
                        comment_hits += 1;
                    }
                }
            }
        }

        if embedded_media_count > 0 || revision_hits > 0 || comment_hits > 0 {
            let mut items = Vec::new();
            if embedded_media_count > 0 {
                items.push(MetadataItem {
                    key: "Embedded images".to_string(),
                    value: format!("{embedded_media_count} file(s) may contain EXIF/GPS"),
                });
            }
            if revision_hits > 0 {
                items.push(MetadataItem {
                    key: "Revision fingerprints".to_string(),
                    value: "rsid / tracked changes present".to_string(),
                });
            }
            if comment_hits > 0 {
                items.push(MetadataItem {
                    key: "Comment references".to_string(),
                    value: "comment anchors present".to_string(),
                });
            }
            set.groups.push(MetadataGroup {
                filename: format!("{filename}/[archive]"),
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
        let file = File::open(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        let mut archive = ZipArchive::new(BufReader::new(file)).map_err(|e| {
            CoreError::CleanError {
                path: path.to_path_buf(),
                detail: format!("Not a valid ZIP archive: {e}"),
            }
        })?;

        // --- Detect archive family -----------------------------------------
        let kind = detect_kind(&mut archive);

        // --- EPUB safety check: refuse encrypted archives ------------------
        if kind == ArchiveKind::Epub && archive.by_name("META-INF/encryption.xml").is_ok() {
            return Err(CoreError::CleanError {
                path: path.to_path_buf(),
                detail: "EPUB contains encryption.xml (DRM or encrypted fonts); \
                         refusing to clean, the output would be unreadable"
                    .to_string(),
            });
        }

        // --- Collect members, sorted, mimetype first (ODF/EPUB) ------------
        // Error out on any header-parse failure instead of quietly dropping
        // the entry via `filter_map`; a half-cleaned DOCX / ODT / EPUB that
        // loses a member without telling the user would ship a structurally
        // incomplete document.
        let mut names: Vec<String> = Vec::with_capacity(archive.len());
        for i in 0..archive.len() {
            let entry = archive.by_index(i).map_err(|e| CoreError::CleanError {
                path: path.to_path_buf(),
                detail: format!("bad zip entry at index {i}: {e}"),
            })?;
            names.push(entry.name().to_string());
        }
        // First sort lexicographically to kill any producer-order fingerprint.
        names.sort();
        // Then move the special `mimetype` entry to the front — it must
        // be first in ODF and EPUB archives per their respective specs.
        if let Some(pos) = names.iter().position(|n| n == "mimetype") {
            let m = names.remove(pos);
            names.insert(0, m);
        }

        let out_file = File::create(output_path).map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("Failed to create output: {e}"),
        })?;
        let mut writer = zip::ZipWriter::new(out_file);

        for entry_name in &names {
            // Drop junk members up-front. For OOXML we also call into
            // `is_ooxml_junk` which is path-pattern-based.
            if should_omit(kind, entry_name) {
                continue;
            }

            let (raw_bytes, compression) = {
                let mut e = archive.by_name(entry_name).map_err(|e| CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: format!("Failed to read ZIP entry {entry_name}: {e}"),
                })?;
                // Don't re-pack directory entries - they confuse ODF readers.
                if e.is_dir() {
                    continue;
                }
                let compression = e.compression();
                let mut buf = Vec::with_capacity(zip_util::safe_capacity_hint(e.size()));
                // Cap the decompressed member body so a DOCX / ODT /
                // EPUB with an embedded zip bomb can't OOM the cleaner.
                // See `archive::MAX_ENTRY_DECOMPRESSED_BYTES` for the
                // cap value and rationale.
                (&mut e)
                    .take(super::archive::MAX_ENTRY_DECOMPRESSED_BYTES + 1)
                    .read_to_end(&mut buf)
                    .map_err(|e| CoreError::CleanError {
                        path: path.to_path_buf(),
                        detail: format!("Failed to read entry {entry_name}: {e}"),
                    })?;
                if buf.len() as u64 > super::archive::MAX_ENTRY_DECOMPRESSED_BYTES {
                    return Err(CoreError::CleanError {
                        path: path.to_path_buf(),
                        detail: format!(
                            "document member '{entry_name}' exceeds the \
                             {}-byte decompression cap; refusing to clean \
                             (likely a zip bomb)",
                            super::archive::MAX_ENTRY_DECOMPRESSED_BYTES
                        ),
                    });
                }
                (buf, compression)
            };

            let cleaned_bytes = clean_entry(kind, entry_name, raw_bytes).map_err(|e| {
                CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: format!("Failed to clean entry {entry_name}: {e}"),
                }
            })?;

            let options = zip_util::normalized_options(compression);
            writer
                .start_file(entry_name, options)
                .map_err(|e| CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: format!("Failed to start ZIP entry {entry_name}: {e}"),
                })?;
            writer
                .write_all(&cleaned_bytes)
                .map_err(|e| CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: format!("Failed to write ZIP entry {entry_name}: {e}"),
                })?;
        }

        writer.finish().map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("Failed to finalize ZIP: {e}"),
        })?;

        Ok(())
    }

    fn supported_mime_types(&self) -> &[&str] {
        &[
            "application/vnd.oasis.opendocument.text",
            "application/vnd.oasis.opendocument.spreadsheet",
            "application/vnd.oasis.opendocument.presentation",
            "application/vnd.oasis.opendocument.graphics",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            "application/epub+zip",
        ]
    }
}

/// Best-effort detection of the archive family based on the list of
/// top-level members. Called once per clean.
fn detect_kind<R: Read + std::io::Seek>(archive: &mut ZipArchive<R>) -> ArchiveKind {
    let mut has_content_types = false;
    let mut has_mimetype_epub = false;
    let mut has_mimetype_odf = false;
    let mut has_content_xml = false;

    for i in 0..archive.len() {
        if let Ok(mut entry) = archive.by_index(i) {
            let name = entry.name().to_string();
            if name == "[Content_Types].xml" {
                has_content_types = true;
            } else if name == "content.xml" {
                has_content_xml = true;
            } else if name == "mimetype" {
                let mut buf = String::new();
                let _ = entry.read_to_string(&mut buf);
                if buf.trim() == "application/epub+zip" {
                    has_mimetype_epub = true;
                } else if buf.starts_with("application/vnd.oasis.opendocument") {
                    has_mimetype_odf = true;
                }
            }
        }
    }

    if has_mimetype_epub {
        ArchiveKind::Epub
    } else if has_mimetype_odf || has_content_xml {
        ArchiveKind::Odf
    } else if has_content_types {
        ArchiveKind::Ooxml
    } else {
        ArchiveKind::Generic
    }
}

/// True if the given entry should be skipped entirely. Combines the
/// shared `zip_util::is_office_junk_path` with a couple of archive-kind
/// specific special cases.
fn should_omit(kind: ArchiveKind, name: &str) -> bool {
    if zip_util::is_office_junk_path(name) {
        return true;
    }
    // ODF specifically: meta.xml is already in the shared junk list but
    // keep `mimetype` regardless of junk rules.
    if name == "mimetype" {
        return false;
    }
    match kind {
        ArchiveKind::Ooxml | ArchiveKind::Odf | ArchiveKind::Epub | ArchiveKind::Generic => false,
    }
}

/// Reason an embedded archive member could not be cleaned. Kept as a
/// plain struct with a `Display` impl so the calling `CleanError` gets a
/// specific message instead of a generic "clean failed".
#[derive(Debug)]
pub struct CleanEntryError(String);

impl std::fmt::Display for CleanEntryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Clean a single archive member. Dispatches on file extension and
/// archive kind to decide whether to replace the bytes entirely, deep-
/// clean the XML, strip EXIF from an embedded image, or leave as-is.
fn clean_entry(
    kind: ArchiveKind,
    entry_name: &str,
    raw: Vec<u8>,
) -> Result<Vec<u8>, CleanEntryError> {
    // 1. Embedded raster images: strip EXIF/XMP/ICC unconditionally,
    //    regardless of archive family. If the image can't be parsed we
    //    must *fail* rather than copy the dirty bytes through, otherwise
    //    the cleaned document silently ships the original metadata.
    if let Some(mime) = zip_util::is_cleanable_media(entry_name) {
        return strip_embedded_image(&raw, mime).ok_or_else(|| {
            CleanEntryError(format!(
                "embedded {mime} '{entry_name}' could not be parsed; \
                 refusing to ship dirty bytes into the cleaned archive"
            ))
        });
    }

    // 2. Binary entries: nothing to do.
    if !is_xml_like(entry_name) {
        return Ok(raw);
    }

    // 3. OOXML stub paths (docProps/core.xml, app.xml, custom.xml) get
    //    replaced with a compile-time constant regardless of what the
    //    original bytes are. Doing this before the UTF-8 decode means a
    //    crafted document with a non-UTF-8 core.xml (UTF-16 BOM, Latin-1,
    //    etc.) still gets its metadata stripped instead of slipping
    //    through the fallback below.
    if kind == ArchiveKind::Ooxml
        && let Some(stub) = ooxml::stub_for_path(entry_name)
    {
        return Ok(stub.as_bytes().to_vec());
    }

    // 4. XML entries: decode once, dispatch, re-encode. OOXML / ODF / EPUB
    //    all mandate UTF-8 (ODF additionally allows UTF-16) for XML parts,
    //    so a non-UTF-8 payload here is either corrupt or hostile. Refuse
    //    to ship it rather than silently re-emitting unprocessed bytes -
    //    the caller surfaces the error to the user.
    let Ok(xml) = std::str::from_utf8(&raw) else {
        return Err(CleanEntryError(format!(
            "XML member '{entry_name}' is not valid UTF-8; \
             refusing to ship unprocessed bytes into the cleaned archive"
        )));
    };

    let cleaned: String = match kind {
        ArchiveKind::Ooxml => ooxml::clean_xml_member(entry_name, xml),
        ArchiveKind::Odf => odf::clean_xml_member(entry_name, xml),
        ArchiveKind::Epub => {
            if epub::is_opf_path(entry_name) {
                epub::clean_opf(xml)
            } else if epub::is_ncx_path(entry_name) || epub::is_ops_xml_path(entry_name) {
                epub::clean_head_only(xml)
            } else {
                xml.to_string()
            }
        }
        ArchiveKind::Generic => xml.to_string(),
    };

    Ok(cleaned.into_bytes())
}

fn is_xml_like(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".xml")
        || lower.ends_with(".rels")
        || lower.ends_with(".opf")
        || lower.ends_with(".ncx")
        || lower.ends_with(".xhtml")
}

/// Parse an embedded image out of `data`, strip every metadata segment
/// (EXIF/ICC/XMP/IPTC/text-chunks/time-chunks), re-encode, return the
/// new bytes. Returns `None` if the image can't be parsed so the caller
/// can fall back to the original bytes.
fn strip_embedded_image(data: &[u8], mime: &str) -> Option<Vec<u8>> {
    // Fast path: img-parts handles JPEG/PNG/WEBP via DynImage.
    let Ok(Some(mut img)) = DynImage::from_bytes(data.to_vec().into()) else {
        return None;
    };
    img.set_exif(None);
    img.set_icc_profile(None);

    let mut buf = Vec::new();
    let mut cursor = Cursor::new(&mut buf);
    img.encoder().write_to(&mut cursor).ok()?;

    // Format-specific post-pass to remove what img-parts doesn't expose.
    // If the post-pass parser fails (our own img-parts output did not
    // re-parse cleanly), return None so `clean_entry` propagates a
    // specific error rather than silently shipping partially-stripped
    // bytes that may still carry XMP / IPTC / COM / text chunks.
    match mime {
        "image/jpeg" => strip_jpeg_extra_segments(&buf),
        "image/png" => strip_png_text_chunks(&buf),
        "image/webp" => strip_webp_extra_chunks(&buf),
        _ => Some(buf),
    }
}

fn strip_jpeg_extra_segments(data: &[u8]) -> Option<Vec<u8>> {
    let mut jpeg = Jpeg::from_bytes(data.to_vec().into()).ok()?;
    for marker in 0xE1u8..=0xEF {
        jpeg.remove_segments_by_marker(marker);
    }
    jpeg.remove_segments_by_marker(0xFE); // COM
    let mut buf = Vec::new();
    let mut cursor = Cursor::new(&mut buf);
    jpeg.encoder().write_to(&mut cursor).ok()?;
    Some(buf)
}

fn strip_png_text_chunks(data: &[u8]) -> Option<Vec<u8>> {
    const CHUNK_TEXT: [u8; 4] = *b"tEXt";
    const CHUNK_ITXT: [u8; 4] = *b"iTXt";
    const CHUNK_ZTXT: [u8; 4] = *b"zTXt";
    const CHUNK_TIME: [u8; 4] = *b"tIME";

    let mut png = Png::from_bytes(data.to_vec().into()).ok()?;
    png.remove_chunks_by_type(CHUNK_TEXT);
    png.remove_chunks_by_type(CHUNK_ITXT);
    png.remove_chunks_by_type(CHUNK_ZTXT);
    png.remove_chunks_by_type(CHUNK_TIME);

    let mut buf = Vec::new();
    let mut cursor = Cursor::new(&mut buf);
    png.encoder().write_to(&mut cursor).ok()?;
    Some(buf)
}

/// Strip the WebP `XMP ` RIFF chunk that `DynImage` can't clear
/// (img-parts 0.4 has no WebP XMP setter). Without this, a document
/// that embeds a WebP exported from Lightroom / Photoshop / Affinity
/// still carries the XMP packet - `dc:creator`, `xmpMM:InstanceID`,
/// GPS, etc. - into the cleaned archive.
fn strip_webp_extra_chunks(data: &[u8]) -> Option<Vec<u8>> {
    const CHUNK_XMP: [u8; 4] = *b"XMP ";
    let mut webp = WebP::from_bytes(data.to_vec().into()).ok()?;
    webp.remove_chunks_by_id(CHUNK_XMP);

    let mut buf = Vec::new();
    let mut cursor = Cursor::new(&mut buf);
    webp.encoder().write_to(&mut cursor).ok()?;
    Some(buf)
}

/// Parse XML content and extract metadata key-value pairs for the
/// read_metadata display path.
fn parse_xml_metadata(xml: &str) -> Vec<MetadataItem> {
    let mut items = Vec::new();
    let mut reader = Reader::from_str(xml);
    let mut current_tag: Option<String> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let local = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                current_tag = Some(local);
            }
            Ok(Event::Text(ref e)) => {
                if let Some(ref tag) = current_tag {
                    let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                    if !text.is_empty() && is_metadata_tag(tag) {
                        items.push(MetadataItem {
                            key: tag.clone(),
                            value: text,
                        });
                    }
                }
            }
            Ok(Event::End(_)) => {
                current_tag = None;
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    items
}

fn is_metadata_tag(tag: &str) -> bool {
    matches!(
        tag,
        "creator"
            | "title"
            | "subject"
            | "description"
            | "keywords"
            | "lastModifiedBy"
            | "created"
            | "modified"
            | "revision"
            | "category"
            | "Application"
            | "AppVersion"
            | "Company"
            | "Manager"
            | "TotalTime"
            | "Pages"
            | "Words"
            | "Characters"
            | "Paragraphs"
            | "Lines"
            | "initial-creator"
            | "creation-date"
            | "date"
            | "editing-cycles"
            | "editing-duration"
            | "generator"
            | "language"
            | "print-date"
            | "printed-by"
            | "identifier"
            | "rights"
            | "publisher"
            | "contributor"
    )
}

// The old test helper was the lightweight-cleanup function; it no
// longer exists because the trait dropped the lightweight mode. The
// ooxml / odf / epub submodules each carry their own unit tests.
#[cfg(test)]
pub(crate) fn clean_xml_metadata_lightweight_for_tests(xml: &str) -> String {
    // Kept for backwards compatibility with existing tests in tests.rs;
    // thin wrapper around the OOXML cleaner which applies the same set
    // of "remove creator/Application/Manager/…" operations plus attribute
    // sorting. The old implementation stripped by tag name only.
    let mut reader = Reader::from_str(xml);
    let mut writer = quick_xml::Writer::new(Cursor::new(Vec::new()));
    let mut skip_depth: usize = 0;
    let remove = [
        "creator",
        "initial-creator",
        "lastModifiedBy",
        "Company",
        "Manager",
        "Application",
        "AppVersion",
        "generator",
        "printed-by",
    ];

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let ln = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if skip_depth > 0 || remove.contains(&ln.as_str()) {
                    skip_depth += 1;
                } else {
                    let _ = writer.write_event(Event::Start(e.clone()));
                }
            }
            Ok(Event::End(ref e)) => {
                if skip_depth > 0 {
                    skip_depth -= 1;
                } else {
                    let _ = writer.write_event(Event::End(e.clone()));
                }
            }
            Ok(Event::Empty(ref e)) => {
                if skip_depth > 0 {
                    continue;
                }
                let ln = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if !remove.contains(&ln.as_str()) {
                    let _ = writer.write_event(Event::Empty(e.clone()));
                }
            }
            Ok(Event::Text(ref t)) => {
                if skip_depth == 0 {
                    let _ = writer.write_event(Event::Text(t.clone()));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(other) => {
                if skip_depth == 0 {
                    let _ = writer.write_event(other);
                }
            }
        }
    }

    String::from_utf8(writer.into_inner().into_inner()).unwrap_or_else(|_| xml.to_string())
}
