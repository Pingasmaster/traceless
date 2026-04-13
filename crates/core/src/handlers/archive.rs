//! Generic archive cleaner: plain ZIP, TAR, TAR.GZ, TAR.BZ2, TAR.XZ.
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

use std::fs::File;
use std::io::{BufReader, BufWriter, Cursor, Read, Write};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use tar::{Builder as TarBuilder, Archive as TarArchive, EntryType, Header as TarHeader};

use crate::config::{archive_unknown_policy, UnknownMemberPolicy};
use crate::error::CoreError;
use crate::metadata::{MetadataGroup, MetadataItem, MetadataSet};

use super::{zip_util, FormatHandler};

pub struct ArchiveHandler;

/// Classify an archive by its filename extension chain. Called from
/// both `read_metadata` and `clean_metadata` to pick the decoder.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum ArchiveFormat {
    Zip,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
}

impl ArchiveFormat {
    fn detect(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
        if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            return Some(Self::TarGz);
        }
        if name.ends_with(".tar.bz2") || name.ends_with(".tbz2") || name.ends_with(".tbz") {
            return Some(Self::TarBz2);
        }
        if name.ends_with(".tar.xz") || name.ends_with(".txz") {
            return Some(Self::TarXz);
        }
        if name.ends_with(".tar") {
            return Some(Self::Tar);
        }
        if name.ends_with(".zip") {
            return Some(Self::Zip);
        }
        None
    }
}

impl FormatHandler for ArchiveHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
        let fmt = ArchiveFormat::detect(path).ok_or_else(|| CoreError::UnsupportedFormat {
            mime_type: "archive: unknown extension".to_string(),
        })?;

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
        let fmt = ArchiveFormat::detect(path).ok_or_else(|| CoreError::UnsupportedFormat {
            mime_type: "archive: unknown extension".to_string(),
        })?;
        match fmt {
            ArchiveFormat::Zip => clean_zip(path, output_path),
            _ => clean_tar(path, output_path, fmt),
        }
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

fn is_suspicious_zip(entry: &zip::read::ZipFile<'_, BufReader<File>>) -> bool {
    // non-Unix creator (mat2 test checks: create_system == 3 means Linux)
    entry.unix_mode().is_none() || entry.last_modified().is_some_and(|dt| dt.year() != 1980)
}

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
        bits.push(format!(
            "comment {:?}",
            entry.comment().to_string()
        ));
    }
    if bits.is_empty() {
        "normalized".to_string()
    } else {
        bits.join(", ")
    }
}

fn recurse_read_zip(
    _archive_path: &Path,
    archive: &mut zip::ZipArchive<BufReader<File>>,
    out: &mut MetadataSet,
) -> Result<(), CoreError> {
    let tmpdir = tempfile::tempdir().map_err(|e| CoreError::CleanError {
        path: PathBuf::new(),
        detail: format!("tempdir: {e}"),
    })?;

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
        let mut buf = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut buf).ok();
        std::fs::write(&probe_path, &buf).ok();

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

fn clean_zip(path: &Path, output_path: &Path) -> Result<(), CoreError> {
    let f = File::open(path).map_err(|e| CoreError::ReadError {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut archive = zip::ZipArchive::new(BufReader::new(f)).map_err(|e| {
        CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("bad zip: {e}"),
        }
    })?;

    // Gather entry names and sort lexicographically (kills member-
    // order fingerprinting).
    let mut names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|e| e.name().to_string()))
        .collect();
    names.sort();

    let out_file = File::create(output_path).map_err(|e| CoreError::CleanError {
        path: path.to_path_buf(),
        detail: format!("create output: {e}"),
    })?;
    let mut writer = zip::ZipWriter::new(out_file);

    let tmpdir = tempfile::tempdir().map_err(|e| CoreError::CleanError {
        path: path.to_path_buf(),
        detail: format!("tempdir: {e}"),
    })?;

    for name in &names {
        if is_path_traversal(name) {
            return Err(CoreError::CleanError {
                path: path.to_path_buf(),
                detail: format!("path traversal: {name}"),
            });
        }

        let (bytes, compression) = {
            let mut entry =
                archive.by_name(name).map_err(|e| CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: format!("read entry {name}: {e}"),
                })?;
            if entry.is_dir() {
                continue;
            }
            let compression = entry.compression();
            let mut buf = Vec::with_capacity(entry.size() as usize);
            entry
                .read_to_end(&mut buf)
                .map_err(|e| CoreError::CleanError {
                    path: path.to_path_buf(),
                    detail: format!("read entry body {name}: {e}"),
                })?;
            (buf, compression)
        };

        // Try to clean the member via format dispatch; honor the
        // process-wide UnknownMemberPolicy when no handler applies.
        let action = dispatch_member(name, bytes, tmpdir.path(), path)?;
        let cleaned = match action {
            ArchiveAction::Write(b) => b,
            ArchiveAction::Drop => continue,
        };

        let options = zip_util::normalized_options(compression);
        writer
            .start_file(name, options)
            .map_err(|e| CoreError::CleanError {
                path: path.to_path_buf(),
                detail: format!("start entry {name}: {e}"),
            })?;
        writer
            .write_all(&cleaned)
            .map_err(|e| CoreError::CleanError {
                path: path.to_path_buf(),
                detail: format!("write entry {name}: {e}"),
            })?;
    }

    writer.finish().map_err(|e| CoreError::CleanError {
        path: path.to_path_buf(),
        detail: format!("finalize zip: {e}"),
    })?;
    Ok(())
}

// ============================================================
// TAR path
// ============================================================

/// Produce a decompressed byte stream for a (possibly compressed) tar.
fn open_tar(path: &Path, fmt: ArchiveFormat) -> Result<Box<dyn Read>, CoreError> {
    let f = File::open(path).map_err(|e| CoreError::ReadError {
        path: path.to_path_buf(),
        source: e,
    })?;
    let reader: Box<dyn Read> = match fmt {
        ArchiveFormat::Tar => Box::new(BufReader::new(f)),
        ArchiveFormat::TarGz => Box::new(GzDecoder::new(BufReader::new(f))),
        ArchiveFormat::TarBz2 => Box::new(bzip2::read::BzDecoder::new(BufReader::new(f))),
        ArchiveFormat::TarXz => Box::new(xz2::read::XzDecoder::new(BufReader::new(f))),
        ArchiveFormat::Zip => unreachable!(),
    };
    Ok(reader)
}

/// Open the archive once, enforce safety invariants, return each
/// header for the reader summary.
fn read_tar_entries(
    path: &Path,
    fmt: ArchiveFormat,
) -> Result<Vec<(String, TarHeader)>, CoreError> {
    let reader = open_tar(path, fmt)?;
    let mut archive = TarArchive::new(reader);
    let mut out = Vec::new();
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    for entry in archive
        .entries()
        .map_err(|e| CoreError::ParseError {
            path: path.to_path_buf(),
            detail: format!("tar: {e}"),
        })?
    {
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

fn clean_tar(path: &Path, output_path: &Path, fmt: ArchiveFormat) -> Result<(), CoreError> {
    // 1. Decompress into memory. Real-world .tar archives are usually
    //    tens of MiB at most — if we ever need to support 10+ GiB
    //    archives this should become a streaming pipeline.
    let mut decompressed = Vec::new();
    open_tar(path, fmt)?.read_to_end(&mut decompressed).map_err(|e| {
        CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        }
    })?;

    // 2. Enumerate and clean each entry in-memory.
    let tmpdir = tempfile::tempdir().map_err(|e| CoreError::CleanError {
        path: path.to_path_buf(),
        detail: format!("tempdir: {e}"),
    })?;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut cleaned_members: Vec<(String, Vec<u8>)> = Vec::new();

    {
        let mut archive = TarArchive::new(&decompressed[..]);
        for entry in archive.entries().map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("tar entries: {e}"),
        })? {
            let mut entry = entry.map_err(|e| CoreError::CleanError {
                path: path.to_path_buf(),
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
                // Symlinks must already have been validated by
                // check_tar_safety above. Keep them as-is by stuffing
                // an empty body with a marker we'll expand later.
                cleaned_members.push((name, Vec::new()));
                continue;
            }
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).map_err(|e| CoreError::CleanError {
                path: path.to_path_buf(),
                detail: format!("read entry {name}: {e}"),
            })?;
            let action = dispatch_member(&name, buf, tmpdir.path(), path)?;
            match action {
                ArchiveAction::Write(cleaned) => cleaned_members.push((name, cleaned)),
                ArchiveAction::Drop => {}
            }
        }
    }

    // 3. Build the output tar. Sort by name for determinism.
    cleaned_members.sort_by(|a, b| a.0.cmp(&b.0));

    let out_file = File::create(output_path).map_err(|e| CoreError::CleanError {
        path: path.to_path_buf(),
        detail: format!("create output: {e}"),
    })?;
    let buf_out = BufWriter::new(out_file);
    let writer: Box<dyn Write> = match fmt {
        ArchiveFormat::Tar => Box::new(buf_out),
        ArchiveFormat::TarGz => Box::new(GzEncoder::new(buf_out, Compression::default())),
        ArchiveFormat::TarBz2 => {
            Box::new(bzip2::write::BzEncoder::new(buf_out, bzip2::Compression::default()))
        }
        ArchiveFormat::TarXz => Box::new(xz2::write::XzEncoder::new(buf_out, 6)),
        ArchiveFormat::Zip => unreachable!(),
    };

    let mut builder = TarBuilder::new(writer);
    for (name, data) in cleaned_members {
        let mut header = TarHeader::new_gnu();
        header.set_path(&name).map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
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
                path: path.to_path_buf(),
                detail: format!("append {name}: {e}"),
            })?;
    }
    builder.into_inner().map_err(|e| CoreError::CleanError {
        path: path.to_path_buf(),
        detail: format!("finish tar: {e}"),
    })?;
    Ok(())
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
fn dispatch_member(
    entry_name: &str,
    bytes: Vec<u8>,
    tmpdir: &Path,
    archive_path: &Path,
) -> Result<ArchiveAction, CoreError> {
    // Write to a temp file with the right extension so handlers' MIME
    // detection (which is extension-based) works.
    let safe = entry_name.replace(['/', '\\'], "_");
    let in_path = tmpdir.join(&safe);
    let out_path = tmpdir.join(format!("cleaned_{safe}"));

    // A tempfile-write failure is an I/O error, not an unknown-member
    // situation. Falling through to `apply_unknown_policy` would let the
    // default `Keep` policy ship the *unstripped* original bytes into the
    // cleaned archive, silently bypassing the handler that was already
    // matched. Surface the error instead.
    std::fs::write(&in_path, &bytes).map_err(|e| CoreError::CleanError {
        path: archive_path.to_path_buf(),
        detail: format!(
            "failed to stage archive member '{entry_name}' for dispatch: {e}"
        ),
    })?;

    let mime = crate::format_support::detect_mime(&in_path);

    // Don't recurse into archive handlers — this module IS the archive
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
    );

    let handler = if is_nested_archive {
        None
    } else {
        crate::format_support::get_handler_for_mime(&mime)
    };

    let Some(handler) = handler else {
        let _ = std::fs::remove_file(&in_path);
        return apply_unknown_policy(entry_name, bytes, archive_path);
    };

    match handler.clean_metadata(&in_path, &out_path) {
        Ok(()) => {
            // If reading the cleaned output fails, we must error out rather
            // than fall back to the original bytes. The original still
            // contains metadata that the handler claimed to have stripped,
            // so silently shipping it would defeat the whole point.
            let cleaned = std::fs::read(&out_path).map_err(|e| CoreError::CleanError {
                path: archive_path.to_path_buf(),
                detail: format!(
                    "cleaned archive member '{entry_name}' ({mime}) could not be read back: {e}"
                ),
            });
            let _ = std::fs::remove_file(&in_path);
            let _ = std::fs::remove_file(&out_path);
            Ok(ArchiveAction::Write(cleaned?))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&in_path);
            // A *known* handler failed. Surface the error regardless of
            // the unknown-member policy — the member was recognized,
            // and the caller explicitly asked us to process it.
            Err(CoreError::CleanError {
                path: archive_path.to_path_buf(),
                detail: format!(
                    "failed to clean archive member {entry_name} ({mime}): {e}"
                ),
            })
        }
    }
}

/// Apply `UnknownMemberPolicy` to a member with no registered handler.
fn apply_unknown_policy(
    entry_name: &str,
    bytes: Vec<u8>,
    archive_path: &Path,
) -> Result<ArchiveAction, CoreError> {
    match archive_unknown_policy() {
        UnknownMemberPolicy::Keep => Ok(ArchiveAction::Write(bytes)),
        UnknownMemberPolicy::Omit => {
            log::info!(
                "omitting unknown archive member '{entry_name}' from {}",
                archive_path.display()
            );
            Ok(ArchiveAction::Drop)
        }
        UnknownMemberPolicy::Abort => Err(CoreError::CleanError {
            path: archive_path.to_path_buf(),
            detail: format!(
                "unknown archive member '{entry_name}' — aborting per policy"
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
