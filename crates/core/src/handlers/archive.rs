//! Generic archive cleaner: plain ZIP, TAR, TAR.GZ, TAR.BZ2, TAR.XZ,
//! TAR.ZST.
//!
//! Unlike the office-document handler (which knows the specific layout
//! of DOCX/ODT/EPUB), this one has to assume arbitrary contents. For
//! every member it recognizes (via MIME dispatch) it cleans in place;
//! for members it doesn't, the output file still contains the original
//! data but with normalized archive-level metadata (timestamps,
//! permissions, uid/gid, create_system).
//!
//! TAR needs extra safety: mat2 refuses setuid, symlinks escaping the
//! archive, absolute paths, path-traversal members, device files,
//! hardlinks, and duplicate entries. We mirror that.

#[cfg(feature = "native")]
use std::fs::File;
#[cfg(feature = "native")]
use std::io::BufReader;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use tar::{Archive as TarArchive, Builder as TarBuilder, EntryType, Header as TarHeader};

use crate::config::{UnknownMemberPolicy, archive_unknown_policy};
use crate::error::CoreError;
#[cfg(feature = "native")]
use crate::metadata::{MetadataGroup, MetadataItem, MetadataSet};

#[cfg(feature = "native")]
use super::FormatHandler;
use super::zip_util;

pub struct ArchiveHandler;

// ============================================================
// Compression codec adapters (native C-backed vs. pure-Rust wasm)
// ============================================================

/// Decompress a (possibly compressed) tar stream fully into memory. The
/// gzip / bzip2 codecs are pure-Rust and shared; XZ and zstd swap between
/// the C-backed `xz2` / `zstd` crates (native) and the pure-Rust
/// `lzma-rust2` / `ruzstd` crates (wasm). The returned `Read` is then
/// `.take`-capped by the caller against the decompression-bomb limit.
fn decompress_tar(input: &[u8], fmt: ArchiveFormat) -> Result<Box<dyn Read + '_>, CoreError> {
    let reader: Box<dyn Read + '_> = match fmt {
        ArchiveFormat::Tar => Box::new(Cursor::new(input)),
        ArchiveFormat::TarGz => Box::new(GzDecoder::new(Cursor::new(input))),
        ArchiveFormat::TarBz2 => Box::new(bzip2::read::BzDecoder::new(Cursor::new(input))),
        ArchiveFormat::TarXz => xz_decoder(input),
        ArchiveFormat::TarZst => zst_decoder(input)?,
        ArchiveFormat::Zip => return Err(zip_unreachable()),
    };
    Ok(reader)
}

#[cfg(feature = "native")]
fn xz_decoder(input: &[u8]) -> Box<dyn Read + '_> {
    Box::new(xz2::read::XzDecoder::new(Cursor::new(input)))
}

#[cfg(not(feature = "native"))]
fn xz_decoder(input: &[u8]) -> Box<dyn Read + '_> {
    // `allow_multiple_streams = true` matches xz2's concatenated-stream
    // handling.
    Box::new(lzma_rust2::XzReader::new(Cursor::new(input), true))
}

#[cfg(feature = "native")]
fn zst_decoder(input: &[u8]) -> Result<Box<dyn Read + '_>, CoreError> {
    let dec = zstd::stream::read::Decoder::new(Cursor::new(input)).map_err(|e| {
        CoreError::CleanError {
            path: PathBuf::new(),
            detail: format!("zstd decoder: {e}"),
        }
    })?;
    Ok(Box::new(dec))
}

#[cfg(not(feature = "native"))]
fn zst_decoder(input: &[u8]) -> Result<Box<dyn Read + '_>, CoreError> {
    let dec = ruzstd::decoding::StreamingDecoder::new(Cursor::new(input)).map_err(|e| {
        CoreError::CleanError {
            path: PathBuf::new(),
            detail: format!("zstd decoder: {e}"),
        }
    })?;
    Ok(Box::new(dec))
}

/// Compress a raw tar byte stream with the codec named by `fmt`. The
/// inverse of [`decompress_tar`]. Plain tar passes through unchanged.
fn compress_tar(tar: &[u8], fmt: ArchiveFormat) -> Result<Vec<u8>, CoreError> {
    match fmt {
        ArchiveFormat::Tar => Ok(tar.to_vec()),
        ArchiveFormat::TarGz => {
            let mut enc = GzEncoder::new(Vec::new(), Compression::default());
            enc.write_all(tar).map_err(|e| compress_err(&e))?;
            enc.finish().map_err(|e| compress_err(&e))
        }
        ArchiveFormat::TarBz2 => {
            let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
            enc.write_all(tar).map_err(|e| compress_err(&e))?;
            enc.finish().map_err(|e| compress_err(&e))
        }
        ArchiveFormat::TarXz => xz_encode(tar),
        ArchiveFormat::TarZst => zst_encode(tar),
        ArchiveFormat::Zip => Err(zip_unreachable()),
    }
}

fn compress_err(e: &std::io::Error) -> CoreError {
    CoreError::CleanError {
        path: PathBuf::new(),
        detail: format!("archive recompression: {e}"),
    }
}

#[cfg(feature = "native")]
fn xz_encode(tar: &[u8]) -> Result<Vec<u8>, CoreError> {
    let mut enc = xz2::write::XzEncoder::new(Vec::new(), 6);
    enc.write_all(tar).map_err(|e| compress_err(&e))?;
    enc.finish().map_err(|e| compress_err(&e))
}

#[cfg(not(feature = "native"))]
fn xz_encode(tar: &[u8]) -> Result<Vec<u8>, CoreError> {
    // Preset 6 matches the native xz2 encoder level.
    let options = lzma_rust2::XzOptions::with_preset(6);
    let mut enc = lzma_rust2::XzWriter::new(Vec::new(), options).map_err(|e| compress_err(&e))?;
    enc.write_all(tar).map_err(|e| compress_err(&e))?;
    enc.finish().map_err(|e| compress_err(&e))
}

#[cfg(feature = "native")]
fn zst_encode(tar: &[u8]) -> Result<Vec<u8>, CoreError> {
    let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 3).map_err(|e| compress_err(&e))?;
    enc.write_all(tar).map_err(|e| compress_err(&e))?;
    enc.finish().map_err(|e| compress_err(&e))
}

#[cfg(not(feature = "native"))]
// The `Result` is infallible for the ruzstd encoder, but it must match the
// native `zst_encode` signature (the C-backed zstd encoder can error) since
// both are dispatched from the same `compress_tar` match arm.
#[allow(clippy::unnecessary_wraps)]
fn zst_encode(tar: &[u8]) -> Result<Vec<u8>, CoreError> {
    // ruzstd 0.8 only implements `Uncompressed` and `Fastest`; `Fastest`
    // (zstd level ~1) is the best implemented ratio. The output is a valid
    // zstd frame that round-trips back to the same tar members.
    Ok(ruzstd::encoding::compress_to_vec(
        tar,
        ruzstd::encoding::CompressionLevel::Fastest,
    ))
}

fn zip_unreachable() -> CoreError {
    CoreError::CleanError {
        path: PathBuf::new(),
        detail: "internal error: ZIP routed through the tar codec path".to_string(),
    }
}

/// Classify an archive by its filename extension chain. Called from
/// both `read_metadata` and `clean_metadata` to pick the decoder.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum ArchiveFormat {
    Zip,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
    TarZst,
}

/// Result of probing an archive filename for a supported format.
enum FormatProbe {
    /// Filename matched one of the archive forms the handler can decode.
    Matched(ArchiveFormat),
    /// Filename ends in a compression suffix (`.gz` / `.bz2` / `.xz`) but
    /// the body is not a tar archive. The handler intentionally does not
    /// decompress-clean-recompress plain compressed blobs, so the caller
    /// must return a clear error instead of the generic
    /// "unknown extension" fallthrough.
    PlainCompressed(&'static str),
    /// Filename has no recognised archive extension at all.
    Unknown,
}

impl ArchiveFormat {
    #[cfg(feature = "native")]
    fn probe(path: &Path) -> FormatProbe {
        let Some(name) = path.file_name() else {
            return FormatProbe::Unknown;
        };
        Self::probe_name(&name.to_string_lossy())
    }

    /// Filename-string variant of [`probe`], usable from the in-memory
    /// path where there is no `Path`. The caller passes the original
    /// filename (or just its compound extension, e.g. `archive.tar.zst`).
    fn probe_name(name: &str) -> FormatProbe {
        // Table-driven so clippy's `case_sensitive_file_extension_comparisons`
        // lint (which only flags literal `.ends_with("...")`) stays quiet,
        // and extending the table with a new archive type is a one-line
        // change. Order matters: longer suffixes must come before their
        // shorter siblings so `.tar.gz` wins over `.gz`.
        const MATCHED_SUFFIXES: &[(&str, ArchiveFormat)] = &[
            (".tar.gz", ArchiveFormat::TarGz),
            (".tgz", ArchiveFormat::TarGz),
            (".tar.bz2", ArchiveFormat::TarBz2),
            (".tbz2", ArchiveFormat::TarBz2),
            (".tbz", ArchiveFormat::TarBz2),
            (".tar.xz", ArchiveFormat::TarXz),
            (".txz", ArchiveFormat::TarXz),
            (".tar.zst", ArchiveFormat::TarZst),
            (".tzst", ArchiveFormat::TarZst),
            (".tar", ArchiveFormat::Tar),
            (".zip", ArchiveFormat::Zip),
        ];
        const PLAIN_SUFFIXES: &[(&str, &str)] = &[
            (".gz", "gzip"),
            (".bz2", "bzip2"),
            (".xz", "xz"),
            (".zst", "zstd"),
        ];

        let name = name.to_ascii_lowercase();

        for (suffix, fmt) in MATCHED_SUFFIXES {
            if name.ends_with(suffix) {
                return FormatProbe::Matched(*fmt);
            }
        }
        for (suffix, kind) in PLAIN_SUFFIXES {
            if name.ends_with(suffix) {
                return FormatProbe::PlainCompressed(kind);
            }
        }
        FormatProbe::Unknown
    }

    /// Resolve a filename into an `ArchiveFormat`, or build a specific
    /// `CoreError` that distinguishes "no archive extension at all" from
    /// "plain compressed stream, which we deliberately do not support".
    #[cfg(feature = "native")]
    fn resolve(path: &Path) -> Result<Self, CoreError> {
        Self::resolve_probe(&Self::probe(path))
    }

    /// Filename-string variant of [`resolve`].
    fn resolve_name(name: &str) -> Result<Self, CoreError> {
        Self::resolve_probe(&Self::probe_name(name))
    }

    fn resolve_probe(probe: &FormatProbe) -> Result<Self, CoreError> {
        match *probe {
            FormatProbe::Matched(fmt) => Ok(fmt),
            FormatProbe::PlainCompressed(kind) => Err(CoreError::UnsupportedFormat {
                mime_type: format!(
                    "plain {kind}-compressed files are not supported; \
                     only tar-bundled variants (.tar.{kind}) and .zip / .tar are handled"
                ),
            }),
            FormatProbe::Unknown => Err(CoreError::UnsupportedFormat {
                mime_type: "archive: unknown extension".to_string(),
            }),
        }
    }
}

/// Strip archive-level + per-member metadata in memory. `name` is the
/// original filename (or compound extension, e.g. `bundle.tar.zst`); it
/// selects the container codec. Shared by the native `clean_metadata`
/// wrapper and the wasm `inmem` path.
///
/// # Errors
///
/// Returns [`CoreError::UnsupportedFormat`] for an unrecognised /
/// plain-compressed name, or [`CoreError::CleanError`] /
/// [`CoreError::ParseError`] if the archive cannot be decoded, a member
/// trips a safety/decompression cap, or the output cannot be rebuilt.
pub(crate) fn clean_bytes(input: &[u8], name: &str) -> Result<Vec<u8>, CoreError> {
    super::check_input_len(input.len())?;
    let fmt = ArchiveFormat::resolve_name(name)?;
    match fmt {
        ArchiveFormat::Zip => clean_zip_bytes(input),
        _ => clean_tar_bytes(input, fmt),
    }
}

#[cfg(feature = "native")]
impl FormatHandler for ArchiveHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
        super::check_input_size(path)?;
        let fmt = ArchiveFormat::resolve(path)?;

        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut set = MetadataSet::default();
        let mut summary_items: Vec<MetadataItem> = Vec::new();

        if fmt == ArchiveFormat::Zip {
            let f = File::open(path).map_err(|e| CoreError::ReadError {
                path: path.to_path_buf(),
                source: e,
            })?;
            let mut archive =
                zip::ZipArchive::new(BufReader::new(f)).map_err(|e| CoreError::ParseError {
                    path: path.to_path_buf(),
                    detail: format!("bad zip: {e}"),
                })?;
            for i in 0..archive.len() {
                let entry = archive.by_index(i).map_err(|e| CoreError::ParseError {
                    path: path.to_path_buf(),
                    detail: format!("bad zip entry: {e}"),
                })?;
                let name = entry.name().to_string();
                if entry.comment().is_empty() && !is_suspicious_zip(&entry) {
                    continue;
                }
                summary_items.push(MetadataItem {
                    key: format!("zip member: {name}"),
                    value: describe_zip_meta(&entry),
                });
            }
            // Recursive per-member read
            recurse_read_zip(path, &mut archive, &mut set)?;
        } else {
            let entries = read_tar_entries(path, fmt)?;
            for (name, header) in entries {
                let meta = describe_tar_meta(&header);
                if !meta.is_empty() {
                    summary_items.push(MetadataItem {
                        key: format!("tar member: {name}"),
                        value: meta,
                    });
                }
            }
        }

        if !summary_items.is_empty() {
            set.groups.push(MetadataGroup {
                filename,
                items: summary_items,
            });
        }
        Ok(set)
    }

    fn clean_metadata(&self, path: &Path, output_path: &Path) -> Result<(), CoreError> {
        super::check_input_size(path)?;
        // Resolve the format from the real filename first so a bad
        // extension surfaces the same specific error as before, then run
        // the shared in-memory cleaner.
        let _ = ArchiveFormat::resolve(path)?;
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let data = std::fs::read(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;
        let cleaned = clean_bytes(&data, &filename).map_err(|e| super::repath(e, path))?;
        std::fs::write(output_path, cleaned).map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("create output: {e}"),
        })?;
        Ok(())
    }

    fn supported_mime_types(&self) -> &[&str] {
        &[
            "application/zip",
            "application/x-tar",
            "application/gzip",
            "application/x-bzip2",
            "application/x-xz",
        ]
    }
}

// ============================================================
// ZIP path
// ============================================================

#[cfg(feature = "native")]
fn is_suspicious_zip(entry: &zip::read::ZipFile<'_, BufReader<File>>) -> bool {
    // non-Unix creator (mat2 test checks: create_system == 3 means Linux)
    entry.unix_mode().is_none() || entry.last_modified().is_some_and(|dt| dt.year() != 1980)
}

#[cfg(feature = "native")]
fn describe_zip_meta(entry: &zip::read::ZipFile<'_, BufReader<File>>) -> String {
    let mut bits = Vec::new();
    if let Some(mode) = entry.unix_mode()
        && mode & 0o7000 != 0
    {
        bits.push(format!("special bits 0o{:o}", mode & 0o7000));
    }
    if let Some(dt) = entry.last_modified()
        && dt.year() != 1980
    {
        bits.push(format!(
            "mtime {}-{:02}-{:02}",
            dt.year(),
            dt.month(),
            dt.day()
        ));
    }
    if !entry.comment().is_empty() {
        bits.push(format!("comment {:?}", entry.comment().to_string()));
    }
    if bits.is_empty() {
        "normalized".to_string()
    } else {
        bits.join(", ")
    }
}

#[cfg(feature = "native")]
fn recurse_read_zip(
    _archive_path: &Path,
    archive: &mut zip::ZipArchive<BufReader<File>>,
    out: &mut MetadataSet,
) -> Result<(), CoreError> {
    let tmpdir = tempfile::tempdir().map_err(|e| CoreError::CleanError {
        path: PathBuf::new(),
        detail: format!("tempdir: {e}"),
    })?;

    let mut total_decompressed: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| CoreError::ParseError {
            path: PathBuf::new(),
            detail: format!("bad zip entry: {e}"),
        })?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        // Path-traversal safety
        if is_path_traversal(&name) {
            return Err(CoreError::ParseError {
                path: PathBuf::new(),
                detail: format!("zip member path traversal: {name}"),
            });
        }
        let safe_name = name.replace(['/', '\\'], "_");
        let probe_path = tmpdir.path().join(safe_name);
        let mut buf = Vec::with_capacity(zip_util::safe_capacity_hint(entry.size()));
        // Cap the decompressed entry size to defeat zip bombs. ZIP has
        // no outer wrapper to cap, so each member is an independent
        // compression bomb vector. Read one extra byte past the cap
        // so we can tell "exactly at the cap" from "would have
        // exceeded it".
        (&mut entry)
            .take(effective_take(MAX_ENTRY_DECOMPRESSED_BYTES))
            .read_to_end(&mut buf)
            .map_err(|e| CoreError::ParseError {
                path: PathBuf::new(),
                detail: format!("read zip entry {name}: {e}"),
            })?;
        if over_cap(buf.len() as u64, MAX_ENTRY_DECOMPRESSED_BYTES) {
            return Err(CoreError::ParseError {
                path: PathBuf::new(),
                detail: format!(
                    "zip member '{name}' exceeds the \
                     {MAX_ENTRY_DECOMPRESSED_BYTES}-byte decompression \
                     cap; refusing to probe (likely a zip bomb)"
                ),
            });
        }
        total_decompressed = total_decompressed.saturating_add(buf.len() as u64);
        if over_cap(total_decompressed, MAX_ARCHIVE_TOTAL_DECOMPRESSED_BYTES) {
            return Err(CoreError::ParseError {
                path: PathBuf::new(),
                detail: format!(
                    "zip archive exceeds the \
                     {MAX_ARCHIVE_TOTAL_DECOMPRESSED_BYTES}-byte cumulative \
                     decompression cap; refusing to probe (likely a multi-\
                     member zip bomb)"
                ),
            });
        }
        std::fs::write(&probe_path, &buf).map_err(|e| CoreError::ParseError {
            path: PathBuf::new(),
            detail: format!("stage zip entry {name} for probe: {e}"),
        })?;

        let mime = crate::format_support::detect_mime(&probe_path);
        if let Some(handler) = crate::format_support::get_handler_for_mime(&mime) {
            // Avoid unbounded recursion: don't dispatch back into the
            // archive handler from within itself.
            if mime == "application/zip"
                || mime == "application/x-tar"
                || mime == "application/gzip"
                || mime == "application/x-bzip2"
                || mime == "application/x-xz"
            {
                continue;
            }
            if let Ok(meta) = handler.read_metadata(&probe_path)
                && !meta.is_empty()
            {
                for mut group in meta.groups {
                    group.filename = format!("{name}/{}", group.filename);
                    out.groups.push(group);
                }
            }
        }
    }
    Ok(())
}

fn clean_zip_bytes(input: &[u8]) -> Result<Vec<u8>, CoreError> {
    let empty = PathBuf::new;
    let mut archive =
        zip::ZipArchive::new(Cursor::new(input)).map_err(|e| CoreError::CleanError {
            path: empty(),
            detail: format!("bad zip: {e}"),
        })?;

    // Gather entry names and sort lexicographically (kills member-
    // order fingerprinting). Surface any header-parse failure as a
    // `CleanError` rather than silently dropping the entry - a
    // `filter_map` over `by_index(...).ok()` would otherwise ship a
    // structurally incomplete cleaned archive without telling the user.
    let mut names: Vec<String> = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| CoreError::CleanError {
            path: empty(),
            detail: format!("bad zip entry at index {i}: {e}"),
        })?;
        names.push(entry.name().to_string());
    }
    names.sort();
    // Duplicate-named zip entries would otherwise route every lookup to
    // the first occurrence, so the cleaned output would write that member
    // twice and silently drop its twin.
    names.dedup();

    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));

    let mut total_decompressed: u64 = 0;
    for name in &names {
        if is_path_traversal(name) {
            return Err(CoreError::CleanError {
                path: empty(),
                detail: format!("path traversal: {name}"),
            });
        }

        let (bytes, compression) = {
            let mut entry = archive.by_name(name).map_err(|e| CoreError::CleanError {
                path: empty(),
                detail: format!("read entry {name}: {e}"),
            })?;
            if entry.is_dir() {
                continue;
            }
            let compression = entry.compression();
            let mut buf = Vec::with_capacity(zip_util::safe_capacity_hint(entry.size()));
            // Cap the decompressed entry body so a single-member zip
            // bomb can't OOM the cleaner. See
            // `MAX_ENTRY_DECOMPRESSED_BYTES` at the top of this file.
            (&mut entry)
                .take(effective_take(MAX_ENTRY_DECOMPRESSED_BYTES))
                .read_to_end(&mut buf)
                .map_err(|e| CoreError::CleanError {
                    path: empty(),
                    detail: format!("read entry body {name}: {e}"),
                })?;
            if over_cap(buf.len() as u64, MAX_ENTRY_DECOMPRESSED_BYTES) {
                return Err(CoreError::CleanError {
                    path: empty(),
                    detail: format!(
                        "zip member '{name}' exceeds the \
                         {MAX_ENTRY_DECOMPRESSED_BYTES}-byte decompression \
                         cap; refusing to clean (likely a zip bomb)"
                    ),
                });
            }
            (buf, compression)
        };

        total_decompressed = total_decompressed.saturating_add(bytes.len() as u64);
        if over_cap(total_decompressed, MAX_ARCHIVE_TOTAL_DECOMPRESSED_BYTES) {
            return Err(CoreError::CleanError {
                path: empty(),
                detail: format!(
                    "zip archive exceeds the \
                     {MAX_ARCHIVE_TOTAL_DECOMPRESSED_BYTES}-byte cumulative \
                     decompression cap; refusing to clean (likely a multi-\
                     member zip bomb)"
                ),
            });
        }

        // Try to clean the member via format dispatch; honor the
        // process-wide UnknownMemberPolicy when no handler applies.
        let action = dispatch_member(name, bytes)?;
        let cleaned = match action {
            ArchiveAction::Write(b) => b,
            ArchiveAction::Drop => continue,
        };

        let options = zip_util::normalized_options(compression);
        writer
            .start_file(name, options)
            .map_err(|e| CoreError::CleanError {
                path: empty(),
                detail: format!("start entry {name}: {e}"),
            })?;
        writer
            .write_all(&cleaned)
            .map_err(|e| CoreError::CleanError {
                path: empty(),
                detail: format!("write entry {name}: {e}"),
            })?;
    }

    let out = writer.finish().map_err(|e| CoreError::CleanError {
        path: empty(),
        detail: format!("finalize zip: {e}"),
    })?;
    Ok(out.into_inner())
}

// ============================================================
// TAR path
// ============================================================

/// Open the archive once, enforce safety invariants, return each
/// header for the reader summary.
///
/// The decompressor is wrapped in `.take(MAX_TAR_DECOMPRESSED_BYTES + 1)`
/// so a crafted `.tar.gz` / `.tar.xz` / `.tar.zst` bomb can't pin a
/// reader worker by decompressing forever. `clean_tar_bytes` already
/// applies the same cap on its side; the reader path was missing it,
/// which turned `ArchiveHandler::read_metadata` into an unbounded CPU
/// DoS.
#[cfg(feature = "native")]
fn read_tar_entries(
    path: &Path,
    fmt: ArchiveFormat,
) -> Result<Vec<(String, TarHeader)>, CoreError> {
    let raw = std::fs::read(path).map_err(|e| CoreError::ReadError {
        path: path.to_path_buf(),
        source: e,
    })?;
    let reader = decompress_tar(&raw, fmt)?.take(effective_take(MAX_TAR_DECOMPRESSED_BYTES));
    let mut archive = TarArchive::new(reader);
    let mut out = Vec::new();
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    for entry in archive.entries().map_err(|e| CoreError::ParseError {
        path: path.to_path_buf(),
        detail: format!("tar: {e}"),
    })? {
        let entry = entry.map_err(|e| CoreError::ParseError {
            path: path.to_path_buf(),
            detail: format!("tar entry: {e}"),
        })?;
        let header = entry.header().clone();
        let name = entry
            .path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        check_tar_safety(&entry, &name, &mut seen_names)?;
        out.push((name, header));
    }
    Ok(out)
}

#[cfg(feature = "native")]
fn describe_tar_meta(header: &TarHeader) -> String {
    let mut bits = Vec::new();
    if header.mtime().unwrap_or(0) != 0 {
        bits.push(format!("mtime={}", header.mtime().unwrap_or(0)));
    }
    if header.uid().unwrap_or(0) != 0 {
        bits.push(format!("uid={}", header.uid().unwrap_or(0)));
    }
    if header.gid().unwrap_or(0) != 0 {
        bits.push(format!("gid={}", header.gid().unwrap_or(0)));
    }
    if let Ok(Some(u)) = header.username()
        && !u.is_empty()
    {
        bits.push(format!("uname={u}"));
    }
    if let Ok(Some(g)) = header.groupname()
        && !g.is_empty()
    {
        bits.push(format!("gname={g}"));
    }
    bits.join(", ")
}

/// mat2's __check_tarfile_safety, ported. Returns Err on anything
/// sketchy. Mutates `seen` with the member name for duplicate detection.
fn check_tar_safety<R: Read>(
    entry: &tar::Entry<'_, R>,
    name: &str,
    seen: &mut std::collections::HashSet<String>,
) -> Result<(), CoreError> {
    if Path::new(name).is_absolute() {
        return Err(CoreError::ParseError {
            path: PathBuf::new(),
            detail: format!("tar member has absolute path: {name}"),
        });
    }
    if is_path_traversal(name) {
        return Err(CoreError::ParseError {
            path: PathBuf::new(),
            detail: format!("tar member has path traversal: {name}"),
        });
    }
    if !seen.insert(name.to_string()) {
        return Err(CoreError::ParseError {
            path: PathBuf::new(),
            detail: format!("tar duplicate member: {name}"),
        });
    }
    let header = entry.header();
    let mode = header.mode().unwrap_or(0);
    if mode & 0o4000 != 0 {
        return Err(CoreError::ParseError {
            path: PathBuf::new(),
            detail: format!("tar setuid member: {name}"),
        });
    }
    if mode & 0o2000 != 0 {
        return Err(CoreError::ParseError {
            path: PathBuf::new(),
            detail: format!("tar setgid member: {name}"),
        });
    }
    let ty = header.entry_type();
    if ty == EntryType::Char || ty == EntryType::Block || ty == EntryType::Fifo {
        return Err(CoreError::ParseError {
            path: PathBuf::new(),
            detail: format!("tar non-regular member: {name}"),
        });
    }
    if ty == EntryType::Link {
        return Err(CoreError::ParseError {
            path: PathBuf::new(),
            detail: format!("tar hardlink: {name}"),
        });
    }
    if ty == EntryType::Symlink
        && let Ok(Some(linkname)) = header.link_name().map(|p| p.map(|p| p.to_path_buf()))
    {
        let link_str = linkname.to_string_lossy().to_string();
        if Path::new(&link_str).is_absolute() || is_path_traversal(&link_str) {
            return Err(CoreError::ParseError {
                path: PathBuf::new(),
                detail: format!("tar symlink escape: {name} -> {link_str}"),
            });
        }
    }
    Ok(())
}

/// An entry ready to be written into the cleaned tar. Distinguishing
/// regular files from symlinks here is load-bearing: before the split
/// every member was written as `EntryType::Regular` with an empty body,
/// which silently transmuted symlinks into empty files.
enum CleanedTarEntry {
    Regular { name: String, data: Vec<u8> },
    Symlink { name: String, target: PathBuf },
}

impl CleanedTarEntry {
    fn name(&self) -> &str {
        match self {
            Self::Regular { name, .. } | Self::Symlink { name, .. } => name,
        }
    }
}

/// Upper bound on the size of a decompressed tar archive the cleaner
/// will accept. A crafted `.tar.gz` / `.tar.bz2` / `.tar.xz` compression
/// bomb could balloon from a few KB into many GiB and OOM the process,
/// so cap the eager `read_to_end` at a value that is still comfortable
/// for legitimate archives (CI artefacts, source tarballs) but refuses
/// gibibyte-scale payloads. If a real use case needs larger, the
/// streaming TAR pipeline noted in `CLAUDE.md` is the real fix.
pub const MAX_TAR_DECOMPRESSED_BYTES: u64 = 1024 * 1024 * 1024;

/// Upper bound on the decompressed size of a single archive entry
/// (ZIP or TAR) the reader and cleaner will accept. Unlike the outer
/// tar cap above, this protects the ZIP paths too - ZIP has no
/// outer wrapper to cap, so each individual member can be a
/// compression bomb all on its own. Set to 1 GiB to match the tar
/// cap's spirit: a single-member tar.bz2 with a legitimate 1 GiB
/// payload still fits under both caps.
///
/// Tests override this to a much smaller value (4 MiB) so they can
/// exercise the cap-hit error path with a manageable fixture.
#[cfg(not(test))]
pub const MAX_ENTRY_DECOMPRESSED_BYTES: u64 = 1024 * 1024 * 1024;
#[cfg(test)]
pub const MAX_ENTRY_DECOMPRESSED_BYTES: u64 = 4 * 1024 * 1024;

/// Upper bound on the *cumulative* decompressed size of all members in a
/// single archive. `MAX_ENTRY_DECOMPRESSED_BYTES` already defuses the
/// single-member bomb, but a zip with 10,000 near-maximum members still
/// amplifies a 10 GiB input into 10 TiB of cleaner output. For a public
/// API this is a disk-exhaustion DoS; cap the running total so the
/// aggregate amplification is bounded.
///
/// Tests override this to 16 MiB so the aggregate cap can be exercised
/// without generating gigabyte fixtures.
#[cfg(not(test))]
pub const MAX_ARCHIVE_TOTAL_DECOMPRESSED_BYTES: u64 = 10 * 1024 * 1024 * 1024;
#[cfg(test)]
pub const MAX_ARCHIVE_TOTAL_DECOMPRESSED_BYTES: u64 = 16 * 1024 * 1024;

/// Effective `take` length for a bounded-read wrapper. Returns
/// `u64::MAX` (effectively no cap) when the process-wide
/// `limits_disabled` flag is on, otherwise `max + 1` so the caller can
/// still distinguish "at the cap" from "over the cap" in the legal
/// path.
fn effective_take(max: u64) -> u64 {
    if crate::config::limits_disabled() {
        u64::MAX
    } else {
        max.saturating_add(1)
    }
}

/// Returns `true` if the caller should reject because `len` exceeds
/// `max`. Always `false` while the "limits disabled" knob is on.
fn over_cap(len: u64, max: u64) -> bool {
    !crate::config::limits_disabled() && len > max
}

// F1 preserves symlinks as their own enum variant; the writer loop has to
// branch on regular-vs-symlink to emit the correct `EntryType`, which pushes
// the top-level body past clippy's 100-line ceiling. Splitting it further
// just fragments one linear pipeline across four helpers for no real gain.
#[allow(clippy::too_many_lines)]
fn clean_tar_bytes(input: &[u8], fmt: ArchiveFormat) -> Result<Vec<u8>, CoreError> {
    let empty = PathBuf::new;
    // 1. Decompress into memory, bounded by `MAX_TAR_DECOMPRESSED_BYTES`
    //    to defeat compression bombs. We read one extra byte so we can
    //    tell "exactly at the cap" from "would have exceeded it".
    let mut decompressed = Vec::new();
    decompress_tar(input, fmt)?
        .take(effective_take(MAX_TAR_DECOMPRESSED_BYTES))
        .read_to_end(&mut decompressed)
        .map_err(|e| CoreError::CleanError {
            path: empty(),
            detail: format!("decompress tar: {e}"),
        })?;
    if over_cap(decompressed.len() as u64, MAX_TAR_DECOMPRESSED_BYTES) {
        return Err(CoreError::CleanError {
            path: empty(),
            detail: format!(
                "tar archive exceeds the {MAX_TAR_DECOMPRESSED_BYTES}-byte \
                 decompression cap; refusing to clean (likely a compression bomb)"
            ),
        });
    }

    // 2. Enumerate and clean each entry in-memory.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut cleaned_members: Vec<CleanedTarEntry> = Vec::new();
    let mut total_decompressed: u64 = 0;

    {
        let mut archive = TarArchive::new(&decompressed[..]);
        for entry in archive.entries().map_err(|e| CoreError::CleanError {
            path: empty(),
            detail: format!("tar entries: {e}"),
        })? {
            let mut entry = entry.map_err(|e| CoreError::CleanError {
                path: empty(),
                detail: format!("tar entry: {e}"),
            })?;
            let name = entry
                .path()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            check_tar_safety(&entry, &name, &mut seen)?;
            let ty = entry.header().entry_type();
            if ty != EntryType::Regular && ty != EntryType::Continuous && ty != EntryType::Symlink {
                continue;
            }
            if ty == EntryType::Symlink {
                // `check_tar_safety` has already rejected absolute /
                // traversing targets. Carry the link target through so
                // the writer can emit a real symlink entry.
                let target = entry
                    .header()
                    .link_name()
                    .map_err(|e| CoreError::CleanError {
                        path: empty(),
                        detail: format!("read symlink target for {name}: {e}"),
                    })?
                    .map(std::borrow::Cow::into_owned)
                    .ok_or_else(|| CoreError::CleanError {
                        path: empty(),
                        detail: format!("symlink {name} has no target"),
                    })?;
                cleaned_members.push(CleanedTarEntry::Symlink { name, target });
                continue;
            }
            // The outer tar stream is already capped at
            // `MAX_TAR_DECOMPRESSED_BYTES` above, so individual
            // members are implicitly bounded too. We still cap each
            // member explicitly so a future refactor that switches
            // to a streaming outer pipeline does not silently re-open
            // the per-entry zip-bomb hole.
            let mut buf = Vec::new();
            (&mut entry)
                .take(effective_take(MAX_ENTRY_DECOMPRESSED_BYTES))
                .read_to_end(&mut buf)
                .map_err(|e| CoreError::CleanError {
                    path: empty(),
                    detail: format!("read entry {name}: {e}"),
                })?;
            if over_cap(buf.len() as u64, MAX_ENTRY_DECOMPRESSED_BYTES) {
                return Err(CoreError::CleanError {
                    path: empty(),
                    detail: format!(
                        "tar member '{name}' exceeds the \
                         {MAX_ENTRY_DECOMPRESSED_BYTES}-byte decompression \
                         cap; refusing to clean (likely a zip bomb)"
                    ),
                });
            }
            total_decompressed = total_decompressed.saturating_add(buf.len() as u64);
            if over_cap(total_decompressed, MAX_ARCHIVE_TOTAL_DECOMPRESSED_BYTES) {
                return Err(CoreError::CleanError {
                    path: empty(),
                    detail: format!(
                        "tar archive exceeds the \
                         {MAX_ARCHIVE_TOTAL_DECOMPRESSED_BYTES}-byte cumulative \
                         decompression cap; refusing to clean (likely a \
                         multi-member zip bomb)"
                    ),
                });
            }
            let action = dispatch_member(&name, buf)?;
            match action {
                ArchiveAction::Write(cleaned) => {
                    cleaned_members.push(CleanedTarEntry::Regular {
                        name,
                        data: cleaned,
                    });
                }
                ArchiveAction::Drop => {}
            }
        }
    }

    // 3. Build the output tar (uncompressed) in memory. Sort by name for
    //    determinism, then recompress with the container codec.
    cleaned_members.sort_by(|a, b| a.name().cmp(b.name()));

    let mut builder = TarBuilder::new(Cursor::new(Vec::new()));
    for entry in cleaned_members {
        match entry {
            CleanedTarEntry::Regular { name, data } => {
                let mut header = TarHeader::new_gnu();
                header.set_path(&name).map_err(|e| CoreError::CleanError {
                    path: empty(),
                    detail: format!("set_path {name}: {e}"),
                })?;
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_uid(0);
                header.set_gid(0);
                header.set_mtime(0);
                header.set_entry_type(EntryType::Regular);
                header.set_username("").ok();
                header.set_groupname("").ok();
                header.set_cksum();
                builder
                    .append(&header, Cursor::new(&data))
                    .map_err(|e| CoreError::CleanError {
                        path: empty(),
                        detail: format!("append {name}: {e}"),
                    })?;
            }
            CleanedTarEntry::Symlink { name, target } => {
                let mut header = TarHeader::new_gnu();
                header.set_path(&name).map_err(|e| CoreError::CleanError {
                    path: empty(),
                    detail: format!("set_path {name}: {e}"),
                })?;
                header.set_size(0);
                header.set_mode(0o777);
                header.set_uid(0);
                header.set_gid(0);
                header.set_mtime(0);
                header.set_entry_type(EntryType::Symlink);
                header
                    .set_link_name(&target)
                    .map_err(|e| CoreError::CleanError {
                        path: empty(),
                        detail: format!("set_link_name {name}: {e}"),
                    })?;
                header.set_username("").ok();
                header.set_groupname("").ok();
                header.set_cksum();
                builder
                    .append(&header, std::io::empty())
                    .map_err(|e| CoreError::CleanError {
                        path: empty(),
                        detail: format!("append symlink {name}: {e}"),
                    })?;
            }
        }
    }
    let tar_bytes = builder
        .into_inner()
        .map_err(|e| CoreError::CleanError {
            path: empty(),
            detail: format!("finish tar: {e}"),
        })?
        .into_inner();

    compress_tar(&tar_bytes, fmt)
}

// ============================================================
// Shared helpers
// ============================================================

fn is_path_traversal(name: &str) -> bool {
    if name.contains("..") {
        // "..file" is fine, only `..` components are a problem.
        for comp in name.split(['/', '\\']) {
            if comp == ".." {
                return true;
            }
        }
    }
    false
}

/// Decision a recursive-clean step can make for a single archive member.
enum ArchiveAction {
    /// Write these bytes out as the member body.
    Write(Vec<u8>),
    /// Drop the member entirely (used by `UnknownMemberPolicy::Omit`).
    Drop,
}

/// Dispatch an archive member through the handler table, honoring the
/// process-wide `UnknownMemberPolicy`.
///
/// Returns:
/// - `Ok(ArchiveAction::Write(cleaned))` when the member was recognized
///   and cleaned, or when the policy says to keep unknown members.
/// - `Ok(ArchiveAction::Drop)` when the policy is `Omit` and the member
///   has no registered handler.
/// - `Err(...)` when the policy is `Abort` and an unknown member was
///   found, or when the member's own handler failed to clean.
fn dispatch_member(entry_name: &str, bytes: Vec<u8>) -> Result<ArchiveAction, CoreError> {
    let mime = crate::inmem::mime_for_name(entry_name);

    // Don't recurse into archive handlers - this module IS the archive
    // handler, and we don't want unbounded nesting. Treat nested
    // archives as opaque members: the user can clean them individually.
    let is_nested_archive = matches!(
        mime.as_str(),
        "application/zip"
            | "application/x-tar"
            | "application/gzip"
            | "application/x-gzip"
            | "application/x-compressed"
            | "application/x-bzip2"
            | "application/x-bzip-compressed-tar"
            | "application/x-gtar"
            | "application/x-xz"
            | "application/zstd"
            | "application/x-zstd"
    );

    if is_nested_archive || !crate::inmem::has_handler(&mime) {
        return apply_unknown_policy(entry_name, bytes);
    }

    // Dispatch through the in-memory cleaner by MIME. A *known* handler
    // failing is surfaced as an error regardless of the unknown-member
    // policy: the member was recognized and the caller asked us to
    // process it, so silently shipping the dirty original would defeat
    // the whole point.
    match crate::inmem::clean_bytes_for_mime(&mime, entry_name, &bytes) {
        Ok(cleaned) => Ok(ArchiveAction::Write(cleaned)),
        Err(e) => Err(CoreError::CleanError {
            path: PathBuf::new(),
            detail: format!("failed to clean archive member {entry_name} ({mime}): {e}"),
        }),
    }
}

/// Apply `UnknownMemberPolicy` to a member with no registered handler.
fn apply_unknown_policy(entry_name: &str, bytes: Vec<u8>) -> Result<ArchiveAction, CoreError> {
    match archive_unknown_policy() {
        UnknownMemberPolicy::Keep => Ok(ArchiveAction::Write(bytes)),
        UnknownMemberPolicy::Omit => {
            log::info!("omitting unknown archive member '{entry_name}'");
            Ok(ArchiveAction::Drop)
        }
        UnknownMemberPolicy::Abort => Err(CoreError::CleanError {
            path: PathBuf::new(),
            detail: format!("unknown archive member '{entry_name}': aborting per policy"),
        }),
    }
}

#[cfg(all(test, feature = "native"))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::BufWriter;
    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;

    #[test]
    fn zip_clean_normalizes_members() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("dirty.zip");
        let dst = dir.path().join("clean.zip");

        // Build a dirty zip with a suspicious member timestamp and a
        // member comment.
        {
            let file = File::create(&src).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let options = SimpleFileOptions::default().last_modified_time(
                zip::DateTime::from_date_and_time(2024, 6, 1, 12, 0, 0).unwrap(),
            );
            writer.start_file("a.txt", options).unwrap();
            writer.write_all(b"hello").unwrap();
            let _ = writer.set_raw_comment(Box::from(b"zip-archive comment".to_vec()));
            writer.finish().unwrap();
        }

        let h = ArchiveHandler;
        h.clean_metadata(&src, &dst).unwrap();

        // Verify normalization
        let f = File::open(&dst).unwrap();
        let mut archive = zip::ZipArchive::new(BufReader::new(f)).unwrap();
        let entry = archive.by_index(0).unwrap();
        let dt = entry.last_modified().unwrap();
        assert_eq!(dt.year(), 1980);
    }

    #[test]
    fn tar_roundtrip_preserves_content() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("dirty.tar");
        let dst = dir.path().join("clean.tar");

        {
            let file = File::create(&src).unwrap();
            let mut builder = TarBuilder::new(BufWriter::new(file));
            let mut header = TarHeader::new_gnu();
            header.set_path("hello.txt").unwrap();
            header.set_size(5);
            header.set_mode(0o644);
            header.set_mtime(1_700_000_000);
            header.set_uid(1000);
            header.set_gid(1000);
            header.set_username("alice").unwrap();
            header.set_groupname("alice").unwrap();
            header.set_entry_type(EntryType::Regular);
            header.set_cksum();
            builder.append(&header, &b"hello"[..]).unwrap();
            builder.into_inner().unwrap();
        }

        let h = ArchiveHandler;
        h.clean_metadata(&src, &dst).unwrap();

        // Verify: iterate the output archive and assert the sole entry
        // has zeroed ownership/time and the original body content.
        // tar::Entry is a streaming reader tied to the Archive cursor,
        // so we must read each body BEFORE advancing to the next entry.
        let f = File::open(&dst).unwrap();
        let mut archive = TarArchive::new(BufReader::new(f));
        let mut count = 0usize;
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let header = entry.header();
            assert_eq!(header.uid().unwrap(), 0);
            assert_eq!(header.gid().unwrap(), 0);
            assert_eq!(header.mtime().unwrap(), 0);
            let mut body = String::new();
            entry.read_to_string(&mut body).unwrap();
            assert_eq!(body, "hello");
            count += 1;
        }
        assert_eq!(count, 1);
    }

    #[test]
    fn tar_preserves_symlink_entry() {
        // Regression test: before this was fixed, every entry in
        // `cleaned_members` was written as `EntryType::Regular` with a
        // zero-length body, which silently transmuted symlinks into
        // empty files. The output tar must still carry a symlink entry
        // whose link target matches the input.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("dirty.tar");
        let dst = dir.path().join("clean.tar");

        {
            let file = File::create(&src).unwrap();
            let mut builder = TarBuilder::new(BufWriter::new(file));

            // Regular payload the symlink will point at.
            let mut reg = TarHeader::new_gnu();
            reg.set_path("target.txt").unwrap();
            reg.set_size(5);
            reg.set_mode(0o644);
            reg.set_mtime(1_700_000_000);
            reg.set_entry_type(EntryType::Regular);
            reg.set_cksum();
            builder.append(&reg, &b"hello"[..]).unwrap();

            // The symlink itself.
            let mut sym = TarHeader::new_gnu();
            sym.set_path("link.txt").unwrap();
            sym.set_size(0);
            sym.set_mode(0o777);
            sym.set_mtime(1_700_000_000);
            sym.set_entry_type(EntryType::Symlink);
            sym.set_link_name("target.txt").unwrap();
            sym.set_cksum();
            builder.append(&sym, std::io::empty()).unwrap();

            builder.into_inner().unwrap();
        }

        let h = ArchiveHandler;
        h.clean_metadata(&src, &dst).unwrap();

        let f = File::open(&dst).unwrap();
        let mut archive = TarArchive::new(BufReader::new(f));
        let mut saw_symlink = false;
        let mut saw_regular = false;
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            let header = entry.header();
            let name = entry.path().unwrap().to_string_lossy().into_owned();
            match header.entry_type() {
                EntryType::Symlink => {
                    assert_eq!(name, "link.txt");
                    let target = header
                        .link_name()
                        .unwrap()
                        .expect("symlink must carry a target")
                        .to_string_lossy()
                        .into_owned();
                    assert_eq!(target, "target.txt");
                    saw_symlink = true;
                }
                EntryType::Regular => {
                    assert_eq!(name, "target.txt");
                    saw_regular = true;
                }
                other => panic!("unexpected entry type {other:?} in cleaned tar"),
            }
        }
        assert!(
            saw_symlink,
            "cleaned tar must still contain the symlink entry"
        );
        assert!(
            saw_regular,
            "cleaned tar must still contain the regular entry"
        );
    }

    #[test]
    fn tar_rejects_path_traversal_via_raw_bytes() {
        // `tar::Builder::append` refuses to write `../escape` itself,
        // so we handcraft a tar header to inject a malicious name.
        // The tar ustar header is 512 bytes; first 100 bytes are the
        // path. We fill the rest with zeros and let the checksum be
        // recalculated on the fly.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("bad.tar");

        let mut block = [0u8; 512];
        // Name: `../escape.txt`
        let name = b"../escape.txt";
        block[..name.len()].copy_from_slice(name);
        // Mode: octal 0000644 as ASCII
        block[100..107].copy_from_slice(b"0000644");
        // UID / GID
        block[108..115].copy_from_slice(b"0000000");
        block[116..123].copy_from_slice(b"0000000");
        // Size: 0 in octal (11 bytes)
        block[124..135].copy_from_slice(b"00000000000");
        // Mtime
        block[136..147].copy_from_slice(b"00000000000");
        // Typeflag: regular file
        block[156] = b'0';
        // Ustar magic
        block[257..263].copy_from_slice(b"ustar\0");
        // Version
        block[263..265].copy_from_slice(b"00");

        // Compute checksum: sum of all bytes, with the checksum field
        // treated as 8 spaces, written as 6 octal digits + NUL + space
        // at offset 148.
        for b in &mut block[148..156] {
            *b = b' ';
        }
        let sum: u32 = block.iter().map(|&b| u32::from(b)).sum();
        let chksum = format!("{sum:06o}\0 ");
        block[148..156].copy_from_slice(chksum.as_bytes());

        // Archive = header block + two zero blocks (tar EOF marker)
        let mut buf = Vec::with_capacity(512 * 3);
        buf.extend_from_slice(&block);
        buf.extend_from_slice(&[0u8; 1024]);
        std::fs::write(&src, &buf).unwrap();

        let h = ArchiveHandler;
        let dst = dir.path().join("out.tar");
        let result = h.clean_metadata(&src, &dst);
        assert!(result.is_err(), "path-traversal tar should be rejected");
    }

    #[test]
    fn zip_path_traversal_is_rejected() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("bad.zip");

        {
            let file = File::create(&src).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let options = SimpleFileOptions::default();
            writer.start_file("../escape.txt", options).unwrap();
            writer.write_all(b"nope").unwrap();
            writer.finish().unwrap();
        }

        let h = ArchiveHandler;
        let dst = dir.path().join("out.zip");
        let result = h.clean_metadata(&src, &dst);
        assert!(result.is_err(), "path-traversal zip should be rejected");
    }

    #[test]
    fn path_traversal_detector() {
        assert!(is_path_traversal("../etc/passwd"));
        assert!(is_path_traversal("foo/../bar"));
        assert!(is_path_traversal(".."));
        assert!(!is_path_traversal("..foo"));
        assert!(!is_path_traversal("foo/bar/baz"));
        assert!(!is_path_traversal("foo.bar.baz"));
    }

    #[test]
    fn zip_per_entry_cap_rejects_oversized_member() {
        // The `#[cfg(test)]` override lowers MAX_ENTRY_DECOMPRESSED_BYTES
        // to 4 MiB; a 5 MiB member therefore lands over the cap. Every
        // test in this suite that depends on the caps firing takes the
        // shared `limits_test_lock` and pins the flag off through a
        // `LimitsGuard`, so a parallel test that toggled the global
        // flag can't race this one.
        use zip::write::SimpleFileOptions;
        let _lock = crate::config::limits_test_lock();
        let _guard = crate::config::LimitsGuard::new(false);
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("bomb.zip");
        let dst = dir.path().join("out.zip");
        {
            let file = File::create(&src).unwrap();
            let mut w = zip::ZipWriter::new(file);
            w.start_file("payload.bin", SimpleFileOptions::default())
                .unwrap();
            let payload = vec![0u8; 5 * 1024 * 1024];
            w.write_all(&payload).unwrap();
            w.finish().unwrap();
        }
        let h = ArchiveHandler;
        let result = h.clean_metadata(&src, &dst);
        assert!(
            result.is_err(),
            "per-entry cap should reject a 5 MiB member when the test cap is 4 MiB"
        );
    }

    #[test]
    fn tar_per_entry_cap_rejects_oversized_member() {
        let _lock = crate::config::limits_test_lock();
        let _guard = crate::config::LimitsGuard::new(false);
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("bomb.tar");
        let dst = dir.path().join("out.tar");
        {
            let file = File::create(&src).unwrap();
            let mut builder = TarBuilder::new(file);
            let mut header = TarHeader::new_gnu();
            header.set_path("payload.bin").unwrap();
            header.set_size(5 * 1024 * 1024);
            header.set_mode(0o644);
            header.set_entry_type(EntryType::Regular);
            header.set_cksum();
            let body = vec![0u8; 5 * 1024 * 1024];
            builder.append(&header, body.as_slice()).unwrap();
            builder.into_inner().unwrap();
        }
        let h = ArchiveHandler;
        let result = h.clean_metadata(&src, &dst);
        assert!(
            result.is_err(),
            "per-entry cap should reject a 5 MiB tar member when the test cap is 4 MiB"
        );
    }

    #[test]
    fn zip_aggregate_cap_rejects_multi_member_bomb() {
        // Per-member cap is 4 MiB under #[cfg(test)], aggregate cap is
        // 16 MiB. Eight 3 MiB members individually pass the per-entry
        // cap but their cumulative decompressed size (24 MiB) exceeds
        // the aggregate cap. Without the cap the cleaner would happily
        // write them all into the output zip; with it, the 6th member
        // trips the aggregate guard and surfaces a specific `CleanError`.
        let _lock = crate::config::limits_test_lock();
        let _guard = crate::config::LimitsGuard::new(false);
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("aggregate-bomb.zip");
        let dst = dir.path().join("out.zip");
        {
            let file = File::create(&src).unwrap();
            let mut w = zip::ZipWriter::new(file);
            for i in 0..8 {
                w.start_file(format!("part{i}.bin"), SimpleFileOptions::default())
                    .unwrap();
                let payload = vec![0u8; 3 * 1024 * 1024];
                w.write_all(&payload).unwrap();
            }
            w.finish().unwrap();
        }
        let h = ArchiveHandler;
        let result = h.clean_metadata(&src, &dst);
        let Err(CoreError::CleanError { detail, .. }) = result else {
            panic!("expected aggregate-cap CleanError, got {result:?}");
        };
        assert!(
            detail.contains("cumulative decompression cap"),
            "aggregate cap error not surfaced: {detail}"
        );
    }

    #[test]
    fn is_path_traversal_handles_backslash_and_dotdot_patterns() {
        // Supplementary cases that complement `path_traversal_detector`.
        // The detector is intentionally lexical: anything containing
        // a `..` path component is rejected, but attribute-style
        // fields (`..foo`, `foo..`) are fine.
        assert!(is_path_traversal("a/../b"));
        assert!(is_path_traversal("../a"));
        assert!(!is_path_traversal("a/b/c"));
        assert!(!is_path_traversal("a/..bar"));
    }
}
