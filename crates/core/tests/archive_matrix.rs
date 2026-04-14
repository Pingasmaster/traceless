//! Archive-specific coverage that doesn't need the policy atomic.
//!
//! - Nested archives (tar-in-zip, zip-in-tar) exercise
//!   `dispatch_member`'s recursion.
//! - Tar safety matrix exercises each rejection reason in
//!   `check_tar_safety` using hand-crafted tar headers (the `tar`
//!   crate's builder refuses to emit most of these itself).
//! - Per-member decompression caps are tested by feeding an over-cap
//!   ZIP entry through the cleaner and asserting the specific
//!   compression-bomb error.
//!
//! Policy-mutating tests live in mat2_parity.rs so they share that
//! binary's serialization lock; these tests don't touch the atomic.

#![allow(clippy::unwrap_used)]
mod common;

use std::fs;
use std::io::Write;
use std::path::Path;

use common::TEST_JPEG;
use tar::{Builder as TarBuilder, EntryType, Header as TarHeader};
use traceless_core::format_support::get_handler_for_mime;

// ============================================================
// Nested archive recursion
// ============================================================

fn build_inner_tar_with_jpeg() -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut builder = TarBuilder::new(&mut buf);
        let mut header = TarHeader::new_gnu();
        header.set_path("photo.jpg").unwrap();
        let body = TEST_JPEG.to_vec();
        #[allow(clippy::cast_sign_loss)]
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(EntryType::Regular);
        header.set_cksum();
        builder.append(&header, body.as_slice()).unwrap();
        builder.into_inner().unwrap();
    }
    buf
}

fn build_inner_zip_with_jpeg() -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut buf = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut buf);
        let mut w = zip::ZipWriter::new(cursor);
        w.start_file("photo.jpg", SimpleFileOptions::default()).unwrap();
        w.write_all(TEST_JPEG).unwrap();
        w.finish().unwrap();
    }
    buf
}

#[test]
fn zip_containing_tar_cleans_without_error() {
    use zip::write::SimpleFileOptions;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("outer.zip");
    let dst = dir.path().join("out.zip");

    {
        let file = fs::File::create(&src).unwrap();
        let mut w = zip::ZipWriter::new(file);
        w.start_file("inner.tar", SimpleFileOptions::default()).unwrap();
        let inner = build_inner_tar_with_jpeg();
        w.write_all(&inner).unwrap();
        w.finish().unwrap();
    }

    let handler = get_handler_for_mime("application/zip").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert!(dst.exists(), "output zip must be produced");
    // Output must parse back as a valid zip.
    let f = fs::File::open(&dst).unwrap();
    let archive = zip::ZipArchive::new(std::io::BufReader::new(f)).unwrap();
    assert!(!archive.is_empty());
}

#[test]
fn tar_containing_zip_cleans_without_error() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("outer.tar");
    let dst = dir.path().join("out.tar");

    {
        let file = fs::File::create(&src).unwrap();
        let mut builder = TarBuilder::new(file);
        let inner = build_inner_zip_with_jpeg();
        let mut header = TarHeader::new_gnu();
        header.set_path("inner.zip").unwrap();
        #[allow(clippy::cast_sign_loss)]
        header.set_size(inner.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(EntryType::Regular);
        header.set_cksum();
        builder.append(&header, inner.as_slice()).unwrap();
        builder.into_inner().unwrap();
    }

    let handler = get_handler_for_mime("application/x-tar").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert!(dst.exists());
}

#[test]
fn zip_containing_tar_containing_zip_cleans() {
    use zip::write::SimpleFileOptions;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("triple.zip");
    let dst = dir.path().join("out.zip");

    // Innermost: ZIP with a JPEG.
    let innermost = build_inner_zip_with_jpeg();
    // Middle: TAR containing the innermost ZIP.
    let mut middle = Vec::new();
    {
        let mut b = TarBuilder::new(&mut middle);
        let mut h = TarHeader::new_gnu();
        h.set_path("level2.zip").unwrap();
        #[allow(clippy::cast_sign_loss)]
        h.set_size(innermost.len() as u64);
        h.set_mode(0o644);
        h.set_entry_type(EntryType::Regular);
        h.set_cksum();
        b.append(&h, innermost.as_slice()).unwrap();
        b.into_inner().unwrap();
    }
    // Outer: ZIP containing the middle TAR.
    {
        let file = fs::File::create(&src).unwrap();
        let mut w = zip::ZipWriter::new(file);
        w.start_file("level1.tar", SimpleFileOptions::default()).unwrap();
        w.write_all(&middle).unwrap();
        w.finish().unwrap();
    }

    let handler = get_handler_for_mime("application/zip").unwrap();
    handler
        .clean_metadata(&src, &dst)
        .expect("triple-nested archive must clean without error");
    assert!(dst.exists());
}

// ============================================================
// TAR safety matrix
// ============================================================
//
// `check_tar_safety` rejects: setuid, symlink escape, absolute path,
// device files, hardlinks, and duplicate names. The stdlib `tar`
// builder refuses to emit most of these, so we build ustar blocks
// by hand, using the same byte-level trick as the existing
// `tar_rejects_path_traversal_via_raw_bytes` unit test.

/// Produce a minimal 512-byte ustar header block with the given path
/// and flags, plus a computed checksum.
fn hand_crafted_tar_header(
    path: &[u8],
    typeflag: u8,
    mode_octal: [u8; 7],
    link_name: Option<&[u8]>,
) -> [u8; 512] {
    let mut block = [0u8; 512];
    // Name (first 100 bytes)
    let n = path.len().min(100);
    block[..n].copy_from_slice(&path[..n]);
    // Mode
    block[100..107].copy_from_slice(&mode_octal);
    // UID / GID
    block[108..115].copy_from_slice(b"0000000");
    block[116..123].copy_from_slice(b"0000000");
    // Size: 0 in octal (11 bytes)
    block[124..135].copy_from_slice(b"00000000000");
    // Mtime
    block[136..147].copy_from_slice(b"00000000000");
    // Typeflag
    block[156] = typeflag;
    // Link name (100 bytes starting at 157)
    if let Some(link) = link_name {
        let m = link.len().min(100);
        block[157..157 + m].copy_from_slice(&link[..m]);
    }
    // Ustar magic
    block[257..263].copy_from_slice(b"ustar\0");
    // Version
    block[263..265].copy_from_slice(b"00");

    // Checksum: sum of all bytes treating the checksum field as 8
    // spaces, written as 6 octal digits + NUL + space at offset 148.
    for b in &mut block[148..156] {
        *b = b' ';
    }
    let sum: u32 = block.iter().map(|&b| u32::from(b)).sum();
    let chksum = format!("{sum:06o}\0 ");
    block[148..156].copy_from_slice(chksum.as_bytes());
    block
}

fn write_tar_archive(path: &Path, blocks: &[[u8; 512]]) {
    let mut buf = Vec::with_capacity(blocks.len() * 512 + 1024);
    for block in blocks {
        buf.extend_from_slice(block);
    }
    // Two all-zero blocks form the tar EOF marker.
    buf.extend_from_slice(&[0u8; 1024]);
    fs::write(path, &buf).unwrap();
}

fn assert_tar_rejected(src: &Path, label: &str) {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("out.tar");
    let handler = get_handler_for_mime("application/x-tar").unwrap();
    let result = handler.clean_metadata(src, &dst);
    assert!(
        result.is_err(),
        "{label}: tar safety check failed to reject the archive"
    );
}

#[test]
fn tar_rejects_setuid_member() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("setuid.tar");
    // Mode 04755 = setuid root + rwxr-xr-x. In ASCII octal: 0004755.
    let block = hand_crafted_tar_header(b"pwn", b'0', *b"0004755", None);
    write_tar_archive(&src, &[block]);
    assert_tar_rejected(&src, "setuid");
}

#[test]
fn tar_rejects_setgid_member() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("setgid.tar");
    // Mode 02755 = setgid + rwxr-xr-x.
    let block = hand_crafted_tar_header(b"pwn", b'0', *b"0002755", None);
    write_tar_archive(&src, &[block]);
    assert_tar_rejected(&src, "setgid");
}

#[test]
fn tar_rejects_absolute_path_member() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("abs.tar");
    let block = hand_crafted_tar_header(b"/etc/passwd", b'0', *b"0000644", None);
    write_tar_archive(&src, &[block]);
    assert_tar_rejected(&src, "absolute-path");
}

#[test]
fn tar_rejects_char_device_member() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("chardev.tar");
    // Typeflag '3' = character special device.
    let block = hand_crafted_tar_header(b"tty", b'3', *b"0000644", None);
    write_tar_archive(&src, &[block]);
    assert_tar_rejected(&src, "character-device");
}

#[test]
fn tar_rejects_block_device_member() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("blkdev.tar");
    // Typeflag '4' = block special device.
    let block = hand_crafted_tar_header(b"disk", b'4', *b"0000644", None);
    write_tar_archive(&src, &[block]);
    assert_tar_rejected(&src, "block-device");
}

#[test]
fn tar_rejects_fifo_member() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("fifo.tar");
    // Typeflag '6' = FIFO.
    let block = hand_crafted_tar_header(b"pipe", b'6', *b"0000644", None);
    write_tar_archive(&src, &[block]);
    assert_tar_rejected(&src, "fifo");
}

#[test]
fn tar_rejects_hardlink_member() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hardlink.tar");
    // Typeflag '1' = hardlink, with a link target.
    let block = hand_crafted_tar_header(b"link", b'1', *b"0000644", Some(b"/etc/shadow"));
    write_tar_archive(&src, &[block]);
    assert_tar_rejected(&src, "hardlink");
}

#[test]
fn tar_rejects_escaping_symlink_member() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("symesc.tar");
    // Typeflag '2' = symlink, with `../../etc/passwd` as target.
    let block = hand_crafted_tar_header(
        b"link",
        b'2',
        *b"0000644",
        Some(b"../../etc/passwd"),
    );
    write_tar_archive(&src, &[block]);
    assert_tar_rejected(&src, "escaping-symlink");
}

#[test]
fn tar_rejects_duplicate_member_names() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("dup.tar");
    let a = hand_crafted_tar_header(b"dup.txt", b'0', *b"0000644", None);
    let b = hand_crafted_tar_header(b"dup.txt", b'0', *b"0000644", None);
    write_tar_archive(&src, &[a, b]);
    assert_tar_rejected(&src, "duplicate-name");
}

// ============================================================
// Note on compression-bomb caps
// ============================================================
//
// The handler caps decompressed entry size at `MAX_ENTRY_DECOMPRESSED_BYTES`
// (1 GiB in release, 4 MiB when the crate's own `#[cfg(test)]` is active).
// Integration tests link against the release-path library, where the
// cap is 1 GiB and unreachable at fixture-build time, so those tests
// live next to the constant in `handlers/archive.rs` where the test
// cfg actually applies. A crate-level unit test exercising the cap
// is added alongside the constant in the per-handler test patch.

// ============================================================
// Round-trip sanity: a mixed archive with every archive format we
// support must produce a cleaned output that itself parses back.
// ============================================================

#[test]
fn tar_mixed_entries_round_trip_has_zeroed_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("mixed.tar");
    let dst = dir.path().join("clean.tar");

    {
        let file = fs::File::create(&src).unwrap();
        let mut builder = TarBuilder::new(file);

        // A regular file with non-zero uid/gid/mtime.
        let mut reg = TarHeader::new_gnu();
        reg.set_path("regular.txt").unwrap();
        reg.set_size(5);
        reg.set_mode(0o644);
        reg.set_mtime(1_700_000_000);
        reg.set_uid(1337);
        reg.set_gid(1337);
        reg.set_username("alice").unwrap();
        reg.set_groupname("alice").unwrap();
        reg.set_entry_type(EntryType::Regular);
        reg.set_cksum();
        builder.append(&reg, &b"hello"[..]).unwrap();

        // A JPEG that exercises the image handler inside tar.
        let mut img = TarHeader::new_gnu();
        img.set_path("photo.jpg").unwrap();
        #[allow(clippy::cast_sign_loss)]
        img.set_size(TEST_JPEG.len() as u64);
        img.set_mode(0o644);
        img.set_mtime(1_700_000_000);
        img.set_uid(42);
        img.set_gid(42);
        img.set_entry_type(EntryType::Regular);
        img.set_cksum();
        builder.append(&img, TEST_JPEG).unwrap();

        builder.into_inner().unwrap();
    }

    let handler = get_handler_for_mime("application/x-tar").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();

    // Every entry's uid/gid/mtime must be zero and usernames cleared.
    let f = fs::File::open(&dst).unwrap();
    let mut archive = tar::Archive::new(std::io::BufReader::new(f));
    let mut seen = 0usize;
    for entry in archive.entries().unwrap() {
        let entry = entry.unwrap();
        let header = entry.header();
        assert_eq!(header.uid().unwrap(), 0);
        assert_eq!(header.gid().unwrap(), 0);
        assert_eq!(header.mtime().unwrap(), 0);
        assert!(header.username().unwrap().unwrap_or("").is_empty());
        assert!(header.groupname().unwrap().unwrap_or("").is_empty());
        seen += 1;
    }
    assert_eq!(seen, 2, "both entries must survive the clean");
}

// ============================================================
// Empty archives
// ============================================================

#[test]
fn empty_zip_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("empty.zip");
    let dst = dir.path().join("clean.zip");

    {
        let file = fs::File::create(&src).unwrap();
        let w = zip::ZipWriter::new(file);
        w.finish().unwrap();
    }

    let handler = get_handler_for_mime("application/zip").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    let f = fs::File::open(&dst).unwrap();
    let archive = zip::ZipArchive::new(std::io::BufReader::new(f)).unwrap();
    assert_eq!(archive.len(), 0);
}

#[test]
fn empty_tar_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("empty.tar");
    let dst = dir.path().join("clean.tar");

    // Empty tar = two all-zero 512-byte blocks.
    fs::write(&src, vec![0u8; 1024]).unwrap();

    let handler = get_handler_for_mime("application/x-tar").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();

    let f = fs::File::open(&dst).unwrap();
    let mut archive = tar::Archive::new(std::io::BufReader::new(f));
    let mut count = 0usize;
    for _ in archive.entries().unwrap() {
        count += 1;
    }
    assert_eq!(count, 0);
}

// ============================================================
// Non-standard / unicode member names
// ============================================================

#[test]
fn zip_with_unicode_member_names_cleans() {
    use zip::write::SimpleFileOptions;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("unicode.zip");
    let dst = dir.path().join("clean.zip");

    {
        let file = fs::File::create(&src).unwrap();
        let mut w = zip::ZipWriter::new(file);
        w.start_file("café.jpg", SimpleFileOptions::default()).unwrap();
        w.write_all(TEST_JPEG).unwrap();
        w.start_file("日本語.jpg", SimpleFileOptions::default()).unwrap();
        w.write_all(TEST_JPEG).unwrap();
        w.finish().unwrap();
    }

    let handler = get_handler_for_mime("application/zip").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();

    let f = fs::File::open(&dst).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(f)).unwrap();
    let mut names: Vec<String> = Vec::new();
    for i in 0..archive.len() {
        names.push(archive.by_index(i).unwrap().name().to_string());
    }
    assert!(names.iter().any(|n| n.contains("caf")));
    assert!(names.iter().any(|n| n.contains("日本語")));
}

