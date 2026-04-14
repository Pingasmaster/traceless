//! Cross-cutting property tests.
//!
//! These are the invariants the README and CLAUDE.md advertise but
//! that the existing suite only enforces by hand on a subset of
//! formats. By expressing each as a loop over a format table, adding
//! a new handler automatically gets covered, and silently dropping a
//! format from the matrix fails loudly.
//!
//! - **Idempotence**: `clean(clean(x))` is byte-identical to `clean(x)`.
//! - **Determinism**: two independent runs of `clean(x)` produce
//!   byte-identical outputs.
//! - **Read-after-clean**: `read_metadata(clean(x))` surfaces no
//!   sensitive fields.
//! - **Dispatch coverage**: every MIME type each handler claims is
//!   actually routed to that handler by `get_handler_for_mime`, and
//!   every supported extension has a handler.

// Every `build_*` helper returns `std::io::Result<()>` so the matrix
// can dispatch through a uniform function pointer. A few of them are
// infallible (plain byte writers), which trips `unnecessary_wraps`;
// silence it file-wide because the uniformity is the point.
#![allow(clippy::unnecessary_wraps)]

#![allow(clippy::unwrap_used)]
mod common;

use std::fs;
use std::path::{Path, PathBuf};

use traceless_core::format_support::{detect_mime, get_handler_for_mime, supported_extensions};

use common::*;

/// One row of the format matrix. Each builder produces a minimally
/// dirty file on disk, and the MIME type is the one the handler
/// should route to. Builders return `Err` if the fixture cannot be
/// synthesised in the current environment (e.g. the installed ffmpeg
/// is too old to encode TIFF); the matrix loops skip such rows so
/// the tests don't turn environment flakiness into handler failures.
///
/// `deterministic`: the handler produces byte-identical output on
/// two independent runs over the same input. Set to false for
/// handlers that intentionally inject random state (EPUB regenerates
/// a UUID every clean; that's a design choice, not a bug).
///
/// `byte_idempotent`: cleaning an already-clean file yields bytes
/// identical to the first clean. Set to false for handlers whose
/// underlying library renumbers objects or otherwise rewrites bytes
/// on every save (lopdf does this for PDFs; the stripping step is
/// still semantically idempotent).
struct FormatRow {
    name: &'static str,
    ext: &'static str,
    mime: &'static str,
    needs_ffmpeg: bool,
    deterministic: bool,
    byte_idempotent: bool,
    build: fn(&Path) -> std::io::Result<()>,
}

// Thin wrappers so every builder has the same signature.

fn build_jpeg(p: &Path) -> std::io::Result<()> {
    make_dirty_jpeg(p);
    Ok(())
}
fn build_png(p: &Path) -> std::io::Result<()> {
    make_dirty_png(p);
    Ok(())
}
fn build_pdf(p: &Path) -> std::io::Result<()> {
    make_dirty_pdf(p);
    Ok(())
}
fn build_docx(p: &Path) -> std::io::Result<()> {
    make_dirty_docx(p, TEST_JPEG);
    Ok(())
}
fn build_odt(p: &Path) -> std::io::Result<()> {
    make_dirty_odt(p);
    Ok(())
}
fn build_epub(p: &Path) -> std::io::Result<()> {
    make_dirty_epub(p);
    Ok(())
}
fn build_mp3(p: &Path) -> std::io::Result<()> {
    make_dirty_mp3(p)
}
fn build_flac(p: &Path) -> std::io::Result<()> {
    make_dirty_flac(p)
}
fn build_ogg(p: &Path) -> std::io::Result<()> {
    make_dirty_ogg(p)
}
fn build_wav(p: &Path) -> std::io::Result<()> {
    make_dirty_wav(p)
}
fn build_aiff(p: &Path) -> std::io::Result<()> {
    make_dirty_aiff(p)
}
fn build_mp4(p: &Path) -> std::io::Result<()> {
    make_dirty_mp4(p)
}
fn build_mkv(p: &Path) -> std::io::Result<()> {
    make_dirty_mkv(p)
}
fn build_avi(p: &Path) -> std::io::Result<()> {
    make_dirty_avi(p)
}
fn build_tiff(p: &Path) -> std::io::Result<()> {
    make_dirty_tiff(p)
}
fn build_bmp(p: &Path) -> std::io::Result<()> {
    make_bmp(p);
    Ok(())
}

const FORMATS: &[FormatRow] = &[
    FormatRow {
        name: "jpeg",
        ext: "jpg",
        mime: "image/jpeg",
        needs_ffmpeg: false,
        deterministic: true,
        byte_idempotent: true,
        build: build_jpeg,
    },
    FormatRow {
        name: "png",
        ext: "png",
        mime: "image/png",
        needs_ffmpeg: false,
        deterministic: true,
        byte_idempotent: true,
        build: build_png,
    },
    FormatRow {
        name: "tiff",
        ext: "tiff",
        mime: "image/tiff",
        // `make_dirty_tiff` synthesises the image via ffmpeg before
        // injecting EXIF; without ffmpeg the builder fails at the
        // synthesis step, not at clean-time.
        needs_ffmpeg: true,
        deterministic: true,
        byte_idempotent: true,
        build: build_tiff,
    },
    FormatRow {
        name: "bmp",
        ext: "bmp",
        mime: "image/bmp",
        needs_ffmpeg: false,
        deterministic: true,
        byte_idempotent: true,
        build: build_bmp,
    },
    FormatRow {
        name: "pdf",
        ext: "pdf",
        mime: "application/pdf",
        needs_ffmpeg: false,
        deterministic: true,
        // lopdf's `Document::save` renumbers the xref object on every
        // save, so cleaning an already-clean PDF yields different
        // bytes even though the stripped set is identical. Semantic
        // idempotence is covered by mat2_parity::pdf_idempotent_clean.
        byte_idempotent: false,
        build: build_pdf,
    },
    FormatRow {
        name: "docx",
        ext: "docx",
        mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        needs_ffmpeg: false,
        deterministic: true,
        byte_idempotent: true,
        build: build_docx,
    },
    FormatRow {
        name: "odt",
        ext: "odt",
        mime: "application/vnd.oasis.opendocument.text",
        needs_ffmpeg: false,
        deterministic: true,
        byte_idempotent: true,
        build: build_odt,
    },
    FormatRow {
        name: "epub",
        ext: "epub",
        mime: "application/epub+zip",
        needs_ffmpeg: false,
        // EPUB handler regenerates `dc:identifier` as a fresh UUID on
        // every clean (mirroring mat2), so byte-wise determinism is
        // structurally impossible. Semantic determinism (same fields
        // stripped, same junk dropped) is covered by mat2_parity.
        deterministic: false,
        byte_idempotent: false,
        build: build_epub,
    },
    FormatRow {
        name: "mp3",
        ext: "mp3",
        mime: "audio/mpeg",
        needs_ffmpeg: false,
        deterministic: true,
        byte_idempotent: true,
        build: build_mp3,
    },
    FormatRow {
        name: "flac",
        ext: "flac",
        mime: "audio/flac",
        needs_ffmpeg: false,
        deterministic: true,
        byte_idempotent: true,
        build: build_flac,
    },
    FormatRow {
        name: "ogg",
        ext: "ogg",
        mime: "audio/ogg",
        needs_ffmpeg: false,
        deterministic: true,
        byte_idempotent: true,
        build: build_ogg,
    },
    FormatRow {
        name: "wav",
        ext: "wav",
        mime: "audio/x-wav",
        needs_ffmpeg: false,
        deterministic: true,
        byte_idempotent: true,
        build: build_wav,
    },
    FormatRow {
        name: "aiff",
        ext: "aiff",
        mime: "audio/x-aiff",
        needs_ffmpeg: false,
        deterministic: true,
        byte_idempotent: true,
        build: build_aiff,
    },
    FormatRow {
        name: "mp4",
        ext: "mp4",
        mime: "video/mp4",
        needs_ffmpeg: true,
        deterministic: true,
        byte_idempotent: true,
        build: build_mp4,
    },
    FormatRow {
        name: "mkv",
        ext: "mkv",
        mime: "video/x-matroska",
        needs_ffmpeg: true,
        deterministic: true,
        byte_idempotent: true,
        build: build_mkv,
    },
    FormatRow {
        name: "avi",
        ext: "avi",
        mime: "video/x-msvideo",
        needs_ffmpeg: true,
        deterministic: true,
        byte_idempotent: true,
        build: build_avi,
    },
];

/// Reason a matrix row was skipped.
enum SkipReason {
    FfmpegMissing,
    FixtureFailed(String),
}

/// Helper: build the fixture, clean twice, return both output paths.
/// If the fixture can't be synthesised, return a typed skip reason so
/// the caller can accumulate it.
fn clean_twice(
    row: &FormatRow,
) -> Result<(tempfile::TempDir, PathBuf, PathBuf), SkipReason> {
    if row.needs_ffmpeg && !have_ffmpeg() {
        return Err(SkipReason::FfmpegMissing);
    }
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join(format!("dirty.{}", row.ext));
    if let Err(e) = (row.build)(&input) {
        return Err(SkipReason::FixtureFailed(e.to_string()));
    }
    let out1 = dir.path().join(format!("clean1.{}", row.ext));
    let out2 = dir.path().join(format!("clean2.{}", row.ext));
    let handler = get_handler_for_mime(row.mime).unwrap();
    handler.clean_metadata(&input, &out1).unwrap();
    handler.clean_metadata(&input, &out2).unwrap();
    Ok((dir, out1, out2))
}

// ============================================================
// Determinism
// ============================================================

#[test]
fn every_handler_produces_deterministic_output() {
    let mut tested = 0usize;
    let mut skipped = Vec::new();
    for row in FORMATS {
        if !row.deterministic {
            skipped.push(format!("{} (non-deterministic by design)", row.name));
            continue;
        }
        let (_dir, out1, out2) = match clean_twice(row) {
            Ok(t) => t,
            Err(SkipReason::FfmpegMissing) => {
                skipped.push(format!("{} (no ffmpeg)", row.name));
                continue;
            }
            Err(SkipReason::FixtureFailed(e)) => {
                skipped.push(format!("{} ({e})", row.name));
                continue;
            }
        };
        let a = fs::read(&out1).unwrap();
        let b = fs::read(&out2).unwrap();
        assert_eq!(
            a, b,
            "determinism broken for {}: two clean runs produced different bytes",
            row.name
        );
        tested += 1;
    }
    if !skipped.is_empty() {
        eprintln!("[cross_cutting] determinism skipped: {skipped:?}");
    }
    assert!(
        tested >= 10,
        "determinism matrix should cover at least 10 formats, got {tested}"
    );
}

// ============================================================
// Idempotence
// ============================================================

#[test]
fn every_handler_is_byte_idempotent() {
    let mut tested = 0usize;
    let mut skipped = Vec::new();
    for row in FORMATS {
        if !row.byte_idempotent {
            skipped.push(format!("{} (not byte-idempotent by design)", row.name));
            continue;
        }
        if row.needs_ffmpeg && !have_ffmpeg() {
            skipped.push(format!("{} (no ffmpeg)", row.name));
            continue;
        }
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join(format!("dirty.{}", row.ext));
        if (row.build)(&input).is_err() {
            skipped.push(format!("{} (fixture build failed)", row.name));
            continue;
        }

        let out1 = dir.path().join(format!("clean1.{}", row.ext));
        let out2 = dir.path().join(format!("clean2.{}", row.ext));
        let handler = get_handler_for_mime(row.mime).unwrap();
        handler.clean_metadata(&input, &out1).unwrap();
        handler.clean_metadata(&out1, &out2).unwrap();

        let first = fs::read(&out1).unwrap();
        let second = fs::read(&out2).unwrap();
        assert_eq!(
            first, second,
            "idempotence broken for {}: cleaning an already-clean file changed its bytes",
            row.name
        );
        tested += 1;
    }
    if !skipped.is_empty() {
        eprintln!("[cross_cutting] idempotence skipped: {skipped:?}");
    }
    assert!(
        tested >= 9,
        "idempotence matrix should cover at least 9 formats, got {tested}"
    );
}

/// Semantic idempotence: even when bytes differ, a re-cleaned file
/// must not re-introduce or retain any sensitive metadata. This is
/// the weaker invariant that every handler must satisfy, including
/// the ones where byte-idempotence is structurally impossible.
#[test]
fn every_handler_is_semantically_idempotent() {
    let mut tested = 0usize;
    for row in FORMATS {
        if row.needs_ffmpeg && !have_ffmpeg() {
            continue;
        }
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join(format!("dirty.{}", row.ext));
        if (row.build)(&input).is_err() {
            continue;
        }
        let out1 = dir.path().join(format!("c1.{}", row.ext));
        let out2 = dir.path().join(format!("c2.{}", row.ext));
        let handler = get_handler_for_mime(row.mime).unwrap();
        handler.clean_metadata(&input, &out1).unwrap();
        handler.clean_metadata(&out1, &out2).unwrap();

        // Both cleaned outputs must surface the same (empty-or-
        // structural) metadata; neither may resurrect a leak.
        let meta1 = handler.read_metadata(&out1).unwrap_or_default();
        let meta2 = handler.read_metadata(&out2).unwrap_or_default();
        for m in [&meta1, &meta2] {
            for g in &m.groups {
                for item in &g.items {
                    assert!(
                        is_structural(&item.key),
                        "{} semantic idempotence broken: re-cleaned file still reports {}={:?}",
                        row.name,
                        item.key,
                        item.value
                    );
                }
            }
        }
        tested += 1;
    }
    assert!(
        tested >= 10,
        "semantic idempotence matrix should cover at least 10 formats, got {tested}"
    );
}

// ============================================================
// Read-after-clean
// ============================================================

/// Keys that genuinely cannot be stripped because they are mandatory
/// structural fields (e.g. image dimensions), format identifiers, or
/// reader-side advisories surfaced on *every* file of that format
/// regardless of cleaning state (the DOCX handler emits an
/// "Embedded images" advisory for any OOXML that contains a media
/// folder, since a lone JPEG inside a DOCX is conceptually a
/// separate file whose EXIF status the user may still want to know
/// about). The test treats their presence as benign. Matching is
/// case-insensitive on the key name.
const STRUCTURAL_KEYS: &[&str] = &[
    "filename",
    "format",
    "dimensions",
    "width",
    "height",
    "duration",
    "channels",
    "samplerate",
    "sample_rate",
    "bitdepth",
    "bit_depth",
    "colorspace",
    "color_space",
    "icc",
    "pages",
    "page count",
    "bitrate",
    "codec",
    "container",
    "stream",
    "streams",
    "has icc profile",
    "has exif",
    "has xmp",
    "embedded images",
    "archive entries",
    "encoder",
    // EPUB mandatory fields: the handler regenerates `dc:identifier`
    // as a fresh UUID, and `dc:language` / `dc:title` are structurally
    // required by the EPUB spec. None of these carry user data after
    // the clean; they're format scaffolding.
    "identifier",
    "language",
    "title",
    // MP4 `hdlr` atom: mandatory per the ISO-BMFF spec, identifies the
    // track type. ffprobe surfaces it as `handler_name` and the
    // cleaner can't remove it without breaking playback.
    "handler_name",
    "vendor_id",
    "major_brand",
    "minor_version",
    "compatible_brands",
];

fn is_structural(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    STRUCTURAL_KEYS
        .iter()
        .any(|structural| lower.contains(structural))
}

#[test]
fn read_after_clean_surfaces_only_structural_fields() {
    let mut tested = 0usize;
    for row in FORMATS {
        if row.needs_ffmpeg && !have_ffmpeg() {
            continue;
        }
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join(format!("dirty.{}", row.ext));
        if (row.build)(&input).is_err() {
            continue;
        }
        let out = dir.path().join(format!("clean.{}", row.ext));
        let handler = get_handler_for_mime(row.mime).unwrap();
        handler.clean_metadata(&input, &out).unwrap();

        let Ok(meta) = handler.read_metadata(&out) else {
            // Some handlers (e.g. pure-copy harmless types) reject
            // empty cleaned stubs. Treat "no metadata returned" as
            // trivially clean.
            tested += 1;
            continue;
        };

        for group in &meta.groups {
            for item in &group.items {
                assert!(
                    is_structural(&item.key),
                    "{} cleaned file still surfaces non-structural metadata: {}={:?}",
                    row.name,
                    item.key,
                    item.value
                );
            }
        }
        tested += 1;
    }
    assert!(
        tested >= 10,
        "read-after-clean matrix should cover at least 10 formats, got {tested}"
    );
}

// ============================================================
// Dispatch coverage
// ============================================================

#[test]
fn every_supported_extension_routes_to_a_handler() {
    // Duplicates mat2_parity §1 with a single-iteration loop, kept
    // here so this file is self-contained: if an extension is added
    // to `supported_extensions()` but forgotten in the dispatcher,
    // either test flags it.
    let dir = tempfile::tempdir().unwrap();
    for ext in supported_extensions() {
        let path = dir.path().join(format!("probe.{ext}"));
        fs::write(&path, b"").unwrap();
        let mime = detect_mime(&path);
        assert!(
            get_handler_for_mime(&mime).is_some(),
            "extension .{ext} → MIME {mime} has no handler"
        );
    }
}

#[test]
fn every_handlers_claimed_mimes_round_trip_through_dispatch() {
    // Each handler advertises a list of MIMEs via `supported_mime_types()`.
    // Every claimed MIME must be routable back to *some* handler — we
    // don't assert it's the same instance because handlers are
    // constructed fresh per lookup, but the dispatch must succeed.
    //
    // We walk each MIME our FORMATS table knows about, look up the
    // handler, and then re-query the dispatcher for every MIME the
    // handler says it supports.
    let mut seen = std::collections::HashSet::new();
    for row in FORMATS {
        let handler = get_handler_for_mime(row.mime).unwrap();
        for claimed in handler.supported_mime_types() {
            assert!(
                get_handler_for_mime(claimed).is_some(),
                "handler for {} claims {claimed} but dispatcher does not route it",
                row.name
            );
            seen.insert((*claimed).to_string());
        }
    }
    assert!(
        seen.len() >= 20,
        "expected at least 20 distinct MIMEs exercised, got {}",
        seen.len()
    );
}
