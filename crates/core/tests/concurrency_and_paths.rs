//! Concurrency stress and path-edge-case integration tests.
//!
//! - Concurrent cleaning of many files through the worker pool.
//! - Concurrent submission from multiple OS threads.
//! - Handlers invoked against filenames containing unicode,
//!   whitespace, and special shell characters. The sandbox layer
//!   passes paths via `&OsStr`, so none of these should need
//!   escaping, but the only way to prove that is to try each one.

mod common;

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use common::*;

use traceless_core::format_support::get_handler_for_mime;

// ============================================================
// Mixed-format concurrent clean stress
// ============================================================

#[test]
fn clean_many_mixed_files_concurrently_via_os_threads() {
    // Spin up 16 OS threads, each cleaning a different format in a
    // tight loop. The handlers have no shared mutable state beyond
    // the process-wide UnknownMemberPolicy atomic (which we don't
    // touch), so concurrent use must be safe. This is the
    // integration-level counterpart to the worker_pool unit tests.
    let dir = tempfile::tempdir().unwrap();

    let mut fixtures: Vec<(&str, &str, std::path::PathBuf)> = Vec::new();

    // Build one fixture per format we're confident works without
    // extra external tools, so the test is portable.
    {
        let p = dir.path().join("a.jpg");
        make_dirty_jpeg(&p);
        fixtures.push(("image/jpeg", "jpg", p));
    }
    {
        let p = dir.path().join("b.png");
        make_dirty_png(&p);
        fixtures.push(("image/png", "png", p));
    }
    {
        let p = dir.path().join("c.pdf");
        make_dirty_pdf(&p);
        fixtures.push(("application/pdf", "pdf", p));
    }
    {
        let p = dir.path().join("d.docx");
        make_dirty_docx(&p, TEST_JPEG);
        fixtures.push((
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "docx",
            p,
        ));
    }
    {
        let p = dir.path().join("e.odt");
        make_dirty_odt(&p);
        fixtures.push((
            "application/vnd.oasis.opendocument.text",
            "odt",
            p,
        ));
    }
    {
        let p = dir.path().join("f.epub");
        make_dirty_epub(&p);
        fixtures.push(("application/epub+zip", "epub", p));
    }
    {
        let p = dir.path().join("g.mp3");
        if make_dirty_mp3(&p).is_ok() {
            fixtures.push(("audio/mpeg", "mp3", p));
        }
    }
    {
        let p = dir.path().join("h.flac");
        if make_dirty_flac(&p).is_ok() {
            fixtures.push(("audio/flac", "flac", p));
        }
    }

    let out_dir = Arc::new(tempfile::tempdir().unwrap());
    let fixtures = Arc::new(fixtures);
    let processed = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for i in 0..16 {
        let fixtures = fixtures.clone();
        let out_dir = out_dir.clone();
        let processed = processed.clone();
        handles.push(thread::spawn(move || {
            let (mime, ext, src) = &fixtures[i % fixtures.len()];
            let dst = out_dir.path().join(format!("t{i}.{ext}"));
            let handler = get_handler_for_mime(mime).unwrap();
            handler
                .clean_metadata(src, &dst)
                .expect("concurrent clean must not error");
            processed.fetch_add(1, Ordering::Relaxed);
        }));
    }

    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(processed.load(Ordering::Relaxed), 16);
}

#[test]
fn clean_same_file_repeatedly_from_many_threads() {
    // Determinism plus race safety: feeding the same file to N
    // threads must yield N byte-identical outputs.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("shared.jpg");
    make_dirty_jpeg(&src);

    let out_dir = Arc::new(tempfile::tempdir().unwrap());
    let mut handles = Vec::new();
    for i in 0..20 {
        let out_dir = out_dir.clone();
        let src = src.clone();
        handles.push(thread::spawn(move || {
            let dst = out_dir.path().join(format!("out_{i}.jpg"));
            let handler = get_handler_for_mime("image/jpeg").unwrap();
            handler.clean_metadata(&src, &dst).unwrap();
            fs::read(&dst).unwrap()
        }));
    }
    let mut outputs: Vec<Vec<u8>> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let first = outputs.pop().unwrap();
    for (i, other) in outputs.iter().enumerate() {
        assert_eq!(&first, other, "thread {i}'s output diverged from thread 0");
    }
}

// ============================================================
// Worker-pool integration: submit from multiple threads
// ============================================================

#[test]
fn worker_pool_handles_concurrent_submits_from_many_threads() {
    // Drive `file_store::FileStore` via the real public API to make
    // sure the worker pool is exercised end-to-end. The worker_pool
    // unit tests only cover trivial counter increments; this adds a
    // real workload with genuine handler invocations.
    //
    // `FileStore::add_files` pushes each file through the pool for
    // MIME detection + metadata reading; the terminal event for each
    // file is either `MetadataReady` or `FileError`. We collect both
    // and assert that every file produces exactly one.
    use std::collections::HashSet;
    use traceless_core::{FileStore, FileStoreEvent};

    let dir = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for i in 0..32 {
        let p = dir.path().join(format!("img_{i}.jpg"));
        make_dirty_jpeg(&p);
        paths.push(p);
    }

    let (tx, rx) = async_channel::unbounded::<FileStoreEvent>();
    let mut store = FileStore::new();
    store.add_files(paths.clone(), &tx);

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut completed_ids: HashSet<u64> = HashSet::new();
    while completed_ids.len() < paths.len() && Instant::now() < deadline {
        match rx.recv_blocking() {
            Ok(FileStoreEvent::MetadataReady { id, .. } | FileStoreEvent::FileError { id, .. }) => {
                completed_ids.insert(id.0);
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    assert_eq!(
        completed_ids.len(),
        paths.len(),
        "not every file reached a terminal event within the timeout"
    );
}

// ============================================================
// Path edge cases
// ============================================================
//
// Every test builds a valid fixture under a filename containing the
// tricky character and asserts the handler completes without
// error. No new fixture format is exercised; the target is the
// sandbox / argv plumbing, not the format parsers.

fn clean_jpeg_through(src: &Path, dst: &Path) {
    let handler = get_handler_for_mime("image/jpeg").unwrap();
    handler.clean_metadata(src, dst).unwrap();
}

#[test]
fn clean_handles_unicode_filename() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("日本語.jpg");
    make_dirty_jpeg(&src);
    let dst = dir.path().join("クリーン.jpg");
    clean_jpeg_through(&src, &dst);
    assert!(dst.exists());
}

#[test]
fn clean_handles_emoji_filename() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("photo-📷.jpg");
    make_dirty_jpeg(&src);
    let dst = dir.path().join("clean-✅.jpg");
    clean_jpeg_through(&src, &dst);
    assert!(dst.exists());
}

#[test]
fn clean_handles_whitespace_in_filename() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("my photo with spaces.jpg");
    make_dirty_jpeg(&src);
    let dst = dir.path().join("cleaned photo.jpg");
    clean_jpeg_through(&src, &dst);
    assert!(dst.exists());
}

#[test]
fn clean_handles_quotes_in_filename() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("it's a \"test\".jpg");
    make_dirty_jpeg(&src);
    let dst = dir.path().join("clean 'single' and \"double\".jpg");
    clean_jpeg_through(&src, &dst);
    assert!(dst.exists());
}

#[test]
fn clean_handles_dollar_and_backtick_in_filename() {
    // These are sh expansion metacharacters; passing the path
    // through `&OsStr` must not let them escape into a subshell.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("$HOME and `id`.jpg");
    make_dirty_jpeg(&src);
    let dst = dir.path().join("cleaned $PATH.jpg");
    clean_jpeg_through(&src, &dst);
    assert!(dst.exists());
}

#[test]
fn clean_handles_long_filename() {
    // Most filesystems allow 255-byte filenames. 200 is safely under
    // that while still being a load test on any code that
    // concatenates paths into a fixed-size buffer.
    let dir = tempfile::tempdir().unwrap();
    let long = "a".repeat(200);
    let src = dir.path().join(format!("{long}.jpg"));
    make_dirty_jpeg(&src);
    let dst = dir.path().join("out.jpg");
    clean_jpeg_through(&src, &dst);
    assert!(dst.exists());
}

#[test]
fn clean_handles_parent_symlink_path() {
    // Regression pin for the round-6 bug where a symlinked parent
    // directory broke bwrap's bind-path assertion by emitting an
    // argv path that didn't match the resolved bind mount. The
    // handler still has to work via the symlinked path.
    #[cfg(unix)]
    {
        let real_dir = tempfile::tempdir().unwrap();
        let link_parent = real_dir.path().parent().unwrap();
        let link = link_parent.join("traceless-sym-test-link");
        // Clean up any lingering link from a prior run.
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(real_dir.path(), &link).unwrap();

        let src = link.join("photo.jpg");
        make_dirty_jpeg(&src);
        let dst = link.join("clean.jpg");
        clean_jpeg_through(&src, &dst);
        assert!(dst.exists());

        let _ = fs::remove_file(&link);
    }
}

#[test]
fn clean_handles_nested_deep_directory() {
    // 15 levels deep is deeper than any real-world "Pictures/2024/01"
    // tree and ensures the sandbox bind-mount machinery handles long
    // paths correctly.
    let root = tempfile::tempdir().unwrap();
    let mut path = root.path().to_path_buf();
    for i in 0..15 {
        path = path.join(format!("level{i}"));
    }
    fs::create_dir_all(&path).unwrap();
    let src = path.join("deep.jpg");
    make_dirty_jpeg(&src);
    let dst = path.join("clean.jpg");
    clean_jpeg_through(&src, &dst);
    assert!(dst.exists());
}
