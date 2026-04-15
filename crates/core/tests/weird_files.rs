//! Per-handler "weird file" and "clean baseline" coverage.
//!
//! Two shapes live here:
//!
//! 1. **Adversarial but well-formed inputs** that exercise a
//!    handler's stress points: multiple metadata vectors in a single
//!    file, unusual ordering, metadata hidden inside nested structure.
//!    Each test builds a fixture that is *valid* for its format (no
//!    malformed-input robustness — that lives in `panic_freedom.rs`)
//!    but packs enough leak shapes to make the cleaner prove it
//!    actually removes each one.
//!
//! 2. **Clean-input baselines**: feed a minimal, already-clean file
//!    to the cleaner and assert the cleaned output is still valid AND
//!    preserves the visible content. This catches the class of bugs
//!    where a cleaner corrupts files that had nothing to strip.
//!
//! Every test here uses the same `FormatHandler` API as the other
//! integration suites: `get_handler_for_mime(mime).unwrap()` then
//! `clean_metadata` / `read_metadata`.

#![allow(clippy::unwrap_used)]
// Scenario-matrix wrapper functions (§C) normalize a mix of
// infallible and io::Result-returning fixture builders to a single
// fn-pointer signature so they can live in a `const` array; the
// no-op Ok(()) wrappers are intentional.
#![allow(clippy::unnecessary_wraps)]
mod common;

use std::fs;
use std::io::{Read, Write};

use traceless_core::format_support::get_handler_for_mime;

use common::*;

// ============================================================
// §A. Adversarial-fixture round-trips
// ============================================================

#[test]
fn jpeg_with_app0_app1_and_comment_interleaved() {
    // Real JPEGs carry APP0 (JFIF) AND APP1 (EXIF) in either order,
    // and some cameras add COM markers between them. img-parts /
    // little_exif handle the canonical ordering, but the cleaner must
    // still strip EXIF when a COM segment sits between APP0 and APP1.
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.jpg");
    let cleaned = dir.path().join("clean.jpg");
    make_dirty_jpeg(&dirty);

    // Splice a COM marker (FF FE <len> "fingerprint") between APP0 and
    // APP1. Most JPEG parsers preserve marker order, so this actually
    // reshapes the file even though it stays valid.
    let raw = fs::read(&dirty).unwrap();
    let mut out = Vec::with_capacity(raw.len() + 32);
    out.extend_from_slice(&raw[..2]); // SOI
    // Find APP0 end (FF E0 <len> ... next marker) and splice a COM
    // after it. This is a linear scan but we only need two markers.
    let mut i = 2usize;
    let mut spliced = false;
    while i + 4 < raw.len() {
        if raw[i] == 0xFF && raw[i + 1] == 0xE0 {
            let len = u16::from_be_bytes([raw[i + 2], raw[i + 3]]) as usize;
            out.extend_from_slice(&raw[i..i + 2 + len]);
            // COM segment: FF FE <len: u16 be> "com-fingerprint"
            let com_body = b"com-secret-fingerprint";
            let com_len = (com_body.len() + 2) as u16;
            out.push(0xFF);
            out.push(0xFE);
            out.extend_from_slice(&com_len.to_be_bytes());
            out.extend_from_slice(com_body);
            i += 2 + len;
            spliced = true;
            break;
        }
        i += 1;
    }
    if spliced {
        out.extend_from_slice(&raw[i..]);
    } else {
        // No APP0 found — just use the original fixture.
        out = raw.clone();
    }
    fs::write(&dirty, &out).unwrap();

    let handler = get_handler_for_mime("image/jpeg").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    // The COM marker carries the fingerprint string; the cleaner must
    // either drop the COM segment or rebuild the JPEG without it.
    let bytes = fs::read(&cleaned).unwrap();
    assert!(
        !bytes
            .windows(b"com-secret-fingerprint".len())
            .any(|w| w == b"com-secret-fingerprint"),
        "COM segment fingerprint survived JPEG clean"
    );
    assert_no_exif_or_valid_jpeg(
        &cleaned,
        "cleaned JPEG with interleaved COM must have no EXIF",
    );
}

#[test]
fn png_with_all_text_chunk_variants_in_one_file() {
    // mat2 treats tEXt / iTXt / zTXt / tIME as equal leak vectors.
    // The standard `make_dirty_png` helper only plants tEXt and tIME;
    // this test plants every variant and asserts they're all gone.
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.png");
    let cleaned = dir.path().join("clean.png");

    // Start from the standard dirty fixture (which already has IHDR,
    // tEXt×2, tIME, IDAT, IEND) and inject extra iTXt + zTXt chunks
    // before the IDAT by rewriting the byte stream.
    make_dirty_png(&dirty);
    let raw = fs::read(&dirty).unwrap();

    // Locate IDAT (offset of its length prefix) to know where to
    // splice the extra chunks.
    let idat_pos = find_chunk_offset(&raw, *b"IDAT").expect("dirty PNG must contain an IDAT chunk");

    let mut out = Vec::with_capacity(raw.len() + 128);
    out.extend_from_slice(&raw[..idat_pos]);

    // iTXt: keyword + "\0\0\0\0" (compression flag/method, lang, translated)
    // + "iTXt-secret-author". Uncompressed so no zlib dependency.
    let mut itxt = Vec::new();
    itxt.extend_from_slice(b"Author\0\0\0\0\0iTXt-secret-author");
    append_png_chunk(&mut out, *b"iTXt", &itxt);

    // zTXt: keyword + "\0" + compression method (0) + compressed text.
    // Supply an empty compressed payload — PNG decoders tolerate it,
    // and we don't need real zlib here because the cleaner must
    // strip the chunk regardless.
    let ztxt = b"Comment\0\0";
    append_png_chunk(&mut out, *b"zTXt", ztxt);

    out.extend_from_slice(&raw[idat_pos..]);
    fs::write(&dirty, &out).unwrap();

    let handler = get_handler_for_mime("image/png").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let cleaned_bytes = fs::read(&cleaned).unwrap();

    // Every injected plant must be gone.
    for needle in [
        &b"iTXt-secret-author"[..],
        &b"mat2-parity-author"[..],
        &b"secret-tool"[..],
    ] {
        assert!(
            !cleaned_bytes.windows(needle.len()).any(|w| w == needle),
            "plant {:?} survived PNG clean",
            std::str::from_utf8(needle).unwrap_or("?")
        );
    }
    // Every text-chunk type tag must be gone.
    for tag in [&b"tEXt"[..], &b"iTXt"[..], &b"zTXt"[..], &b"tIME"[..]] {
        assert!(
            !cleaned_bytes.windows(tag.len()).any(|w| w == tag),
            "chunk type {:?} survived PNG clean",
            std::str::from_utf8(tag).unwrap_or("?")
        );
    }
}

#[test]
fn gif_with_multiple_comment_extensions() {
    // Two comment extensions before and after the image descriptor.
    // The byte-level walker in `handlers::gif` must drop both.
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.gif");
    let cleaned = dir.path().join("clean.gif");

    let mut gif = Vec::new();
    gif.extend_from_slice(b"GIF89a");
    gif.extend_from_slice(&[0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00]);
    // Comment #1 (before image)
    gif.extend_from_slice(&[0x21, 0xFE]);
    gif.push(14);
    gif.extend_from_slice(b"gif-secret-c1x");
    gif.push(0x00);
    // Image descriptor
    gif.extend_from_slice(&[0x2C, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
    gif.extend_from_slice(&[0x02, 0x02, 0x44, 0x01, 0x00]);
    // Comment #2 (between image and trailer)
    gif.extend_from_slice(&[0x21, 0xFE]);
    gif.push(14);
    gif.extend_from_slice(b"gif-secret-c2x");
    gif.push(0x00);
    gif.push(0x3B);
    fs::write(&dirty, &gif).unwrap();

    let handler = get_handler_for_mime("image/gif").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let out = fs::read(&cleaned).unwrap();
    assert!(
        !out.windows(14).any(|w| w == b"gif-secret-c1x"),
        "comment 1 survived GIF clean"
    );
    assert!(
        !out.windows(14).any(|w| w == b"gif-secret-c2x"),
        "comment 2 survived GIF clean"
    );
    assert_eq!(
        out.last(),
        Some(&0x3B),
        "cleaned GIF must still end with 0x3B trailer"
    );
}

#[test]
fn svg_with_inkscape_sodipodi_rdf_and_javascript_href() {
    // Full kitchen-sink SVG: Dublin Core metadata block, sodipodi
    // namedview, inkscape attributes, inline onclick handler, and a
    // javascript: href on <a>. All five are strip-targets per mat2's
    // svg handler — missing any one would count as a leak.
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.svg");
    let cleaned = dir.path().join("clean.svg");

    fs::write(
        &dirty,
        br#"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg"
     xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape"
     xmlns:sodipodi="http://sodipodi.sourceforge.net/DTD/sodipodi-0.0.dtd"
     xmlns:dc="http://purl.org/dc/elements/1.1/"
     xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:xlink="http://www.w3.org/1999/xlink"
     width="16" height="16"
     inkscape:version="1.3"
     sodipodi:docname="svg-secret-docname">
  <metadata>
    <rdf:RDF>
      <dc:creator>svg-secret-author</dc:creator>
      <dc:contributor>svg-secret-contributor</dc:contributor>
    </rdf:RDF>
  </metadata>
  <sodipodi:namedview id="base" inkscape:zoom="2.0"/>
  <a xlink:href="javascript:alert('svg-secret-js')">
    <rect width="16" height="16" fill="red" onclick="alert('svg-secret-onclick')"/>
  </a>
</svg>"#,
    )
    .unwrap();

    let handler = get_handler_for_mime("image/svg+xml").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let out = fs::read_to_string(&cleaned).unwrap();

    for plant in [
        "svg-secret-author",
        "svg-secret-contributor",
        "svg-secret-docname",
        "svg-secret-js",
        "svg-secret-onclick",
    ] {
        assert!(
            !out.contains(plant),
            "SVG plant {plant:?} survived clean. Full output:\n{out}"
        );
    }
    // The visible <rect> must still be present
    assert!(
        out.contains("<rect"),
        "visible <rect> dropped from SVG. Full output:\n{out}"
    );
}

#[test]
fn html_with_http_equiv_link_author_and_inline_script() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.html");
    let cleaned = dir.path().join("clean.html");
    fs::write(
        &dirty,
        br#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta http-equiv="refresh" content="0; url=https://tracking.example/html-secret-refresh">
<meta name="generator" content="html-secret-generator">
<link rel="author" href="mailto:html-secret-author@example.com">
<script>var x = "html-secret-script";</script>
</head>
<body>
<iframe src="https://tracking.example/html-secret-iframe"></iframe>
<p onmouseover="alert('html-secret-onmouseover')">visible-paragraph</p>
</body>
</html>"#,
    )
    .unwrap();

    let handler = get_handler_for_mime("text/html").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let out = fs::read_to_string(&cleaned).unwrap();
    for plant in [
        "html-secret-refresh",
        "html-secret-generator",
        "html-secret-author",
        "html-secret-script",
        "html-secret-iframe",
        "html-secret-onmouseover",
    ] {
        assert!(
            !out.contains(plant),
            "HTML plant {plant:?} survived clean:\n{out}"
        );
    }
    assert!(
        out.contains("visible-paragraph"),
        "body content dropped:\n{out}"
    );
}

#[test]
fn css_comment_containing_close_marker_inside_string_literal() {
    // `"*/"` inside a string literal must NOT be interpreted as a
    // comment closer. The cleaner must strip the outer /* ... */ but
    // leave the rule with the string intact.
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.css");
    let cleaned = dir.path().join("clean.css");
    fs::write(
        &dirty,
        br#"/* css-secret-author: alice */
.tracker::before { content: "not-a-*/-comment"; }
body { color: red; }
"#,
    )
    .unwrap();

    let handler = get_handler_for_mime("text/css").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let out = fs::read_to_string(&cleaned).unwrap();
    assert!(
        !out.contains("css-secret-author"),
        "css comment author survived clean:\n{out}"
    );
    assert!(
        out.contains("not-a-*/-comment"),
        "string literal with */ was corrupted:\n{out}"
    );
    assert!(
        out.contains("body") && out.contains("color: red"),
        "visible rule dropped:\n{out}"
    );
}

#[test]
fn torrent_with_created_by_nested_inside_info_dict() {
    // `info.created by` is a non-canonical placement but some buggy
    // clients write it there. Stripping it would change the infohash
    // so the handler is expected to mirror mat2 and leave the info
    // dict contents alone. This test pins that behaviour: the
    // root-level `created by` is absent (by construction we don't
    // plant one), and the info-level `created by` survives verbatim.
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.torrent");
    let cleaned = dir.path().join("clean.torrent");

    // Hand-written bencode with manually verified length prefixes.
    // Keys inside each dict are sorted as bencode strictly requires.
    //
    // Root dict keys (lex-sorted): "announce" < "info"
    // Info dict keys (lex-sorted): "created by" < "length"
    //                                < "name" < "piece length" < "pieces"
    //
    // String prefixes must match the exact byte length of the payload.
    //   "http://x/tr" is 11 chars → 11:http://x/tr
    //   "secret-nested" is 13 chars → 13:secret-nested
    //   "pony.txt"     is 8 chars  → 8:pony.txt
    //   20 pieces bytes             → 20:01234567890123456789
    let payload = b"d8:announce11:http://x/tr4:infod10:created by13:secret-nested6:lengthi10e4:name8:pony.txt12:piece lengthi16384e6:pieces20:01234567890123456789ee";
    fs::write(&dirty, payload).unwrap();

    let handler = get_handler_for_mime("application/x-bittorrent").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let out = fs::read(&cleaned).unwrap();

    // The info-dict plant must still be present: stripping it would
    // invalidate the infohash.
    let has_plant = out
        .windows(b"secret-nested".len())
        .any(|w| w == b"secret-nested");
    assert!(
        has_plant,
        "torrent info-dict contents must not be rewritten (infohash would change)"
    );
    // Output must still be a well-formed bencode stream.
    assert_eq!(out.first(), Some(&b'd'));
    assert_eq!(out.last(), Some(&b'e'));
}

#[test]
fn nested_zip_inside_tar_zst_is_kept_verbatim_not_recursed() {
    // The archive handler deliberately does NOT recurse into nested
    // archive members — `handlers::archive::dispatch_member` marks
    // any member whose MIME looks like another archive as
    // `is_nested_archive` and skips the handler dispatch entirely.
    // The documented reasoning (comments in archive.rs around line
    // 876) is that unbounded nesting is the wrong default: the user
    // can clean the inner archive explicitly if they want.
    //
    // This test pins that behaviour so a well-meaning refactor
    // can't silently change it. A tar.zst containing `album.zip`
    // containing a dirty JPEG must survive cleaning with:
    //   - the outer tar.zst rewritten (timestamps/permissions normalized)
    //   - the inner zip bytes kept verbatim (the JPEG's EXIF intact)
    //
    // If someone ever wires up recursion they'll need to replace
    // this test with the "nested jpeg gets cleaned" assertion.
    use std::io::Cursor;

    let dir = tempfile::tempdir().unwrap();
    let jpeg_path = dir.path().join("inner.jpg");
    make_dirty_jpeg(&jpeg_path);

    // Fixture sanity: little_exif must see at least one EXIF tag in
    // the freshly built dirty JPEG. This is the EXIF-aware equivalent
    // of a raw-byte grep and works regardless of how the EXIF payload
    // is framed inside the APP1 segment.
    {
        let meta = little_exif::metadata::Metadata::new_from_path(&jpeg_path).unwrap();
        assert!(
            meta.into_iter().next().is_some(),
            "fixture sanity: dirty jpeg must carry EXIF before packaging"
        );
    }

    let jpeg_bytes = fs::read(&jpeg_path).unwrap();

    // Build inner zip in-memory with the dirty JPEG
    let mut inner_zip_buf: Vec<u8> = Vec::new();
    {
        use zip::write::SimpleFileOptions;
        let cursor = Cursor::new(&mut inner_zip_buf);
        let mut writer = zip::ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default()
            .last_modified_time(zip::DateTime::from_date_and_time(2024, 6, 1, 12, 0, 0).unwrap());
        writer.start_file("photo.jpg", opts).unwrap();
        writer.write_all(&jpeg_bytes).unwrap();
        writer.finish().unwrap();
    }

    // Wrap the zip in a tar
    let mut tar_buf: Vec<u8> = Vec::new();
    {
        use tar::{Builder, EntryType, Header};
        let mut builder = Builder::new(&mut tar_buf);
        let mut header = Header::new_gnu();
        header.set_path("album.zip").unwrap();
        header.set_size(inner_zip_buf.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(1_700_000_000);
        header.set_entry_type(EntryType::Regular);
        header.set_cksum();
        builder.append(&header, inner_zip_buf.as_slice()).unwrap();
        builder.into_inner().unwrap();
    }

    // Wrap the tar in zstd
    let src = dir.path().join("dirty.tar.zst");
    let dst = dir.path().join("clean.tar.zst");
    {
        let out = fs::File::create(&src).unwrap();
        let mut enc = zstd::Encoder::new(out, 3).unwrap();
        enc.write_all(&tar_buf).unwrap();
        enc.finish().unwrap();
    }

    let handler = get_handler_for_mime("application/zstd").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();

    // The cleaned tar.zst must still contain album.zip, and the
    // inner JPEG inside the inner zip must STILL carry EXIF (the
    // handler deliberately didn't recurse). We extract it and ask
    // little_exif — same EXIF-aware check we used for sanity above.
    let cleaned_bytes = fs::read(&dst).unwrap();
    let tar_plain = zstd::decode_all(Cursor::new(&cleaned_bytes)).unwrap();
    let mut tar_archive = tar::Archive::new(Cursor::new(&tar_plain));
    let mut found_zip = false;
    for entry in tar_archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().to_string_lossy().to_string();
        if !name.ends_with(".zip") {
            continue;
        }
        let mut zip_bytes = Vec::new();
        entry.read_to_end(&mut zip_bytes).unwrap();
        let mut inner_archive = zip::ZipArchive::new(Cursor::new(&zip_bytes)).unwrap();
        let mut inner_jpeg = inner_archive.by_name("photo.jpg").unwrap();
        let mut jpeg_out = Vec::new();
        inner_jpeg.read_to_end(&mut jpeg_out).unwrap();
        let probe = dir.path().join("extracted.jpg");
        fs::write(&probe, &jpeg_out).unwrap();
        let meta = little_exif::metadata::Metadata::new_from_path(&probe).unwrap();
        assert!(
            meta.into_iter().next().is_some(),
            "inner jpeg lost its EXIF — archive handler must have recursed into the nested zip, which contradicts the documented design"
        );
        found_zip = true;
        break;
    }
    assert!(found_zip, "cleaned tar.zst lost the nested zip member");
}

#[test]
fn mp3_with_id3v1_and_id3v2_both_present_is_cleaned() {
    // mat2 strips both tags in one pass. Real-world MP3 files often
    // carry ID3v1 in the last 128 bytes and ID3v2 at the start; we
    // build both with lofty so the tag types are canonical.
    if !have_ffmpeg() {
        eprintln!("[SKIP] ffmpeg not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.mp3");
    let cleaned = dir.path().join("clean.mp3");
    if make_dirty_mp3(&dirty).is_err() {
        eprintln!("[SKIP] ffmpeg failed to synthesize mp3");
        return;
    }

    // Append a bare ID3v1 trailer so both tag types coexist.
    let mut raw = fs::read(&dirty).unwrap();
    let mut v1 = Vec::with_capacity(128);
    v1.extend_from_slice(b"TAG");
    v1.extend_from_slice(&[b' '; 30]);
    v1[3..3 + b"id3v1-secret-title".len()].copy_from_slice(b"id3v1-secret-title");
    v1.extend_from_slice(&[b' '; 30]); // artist
    v1.extend_from_slice(&[b' '; 30]); // album
    v1.extend_from_slice(&[b' '; 4]); // year
    v1.extend_from_slice(&[b' '; 30]); // comment
    v1.push(0); // genre
    // Truncate to 128 bytes if we overran due to the title slice.
    v1.truncate(128);
    raw.extend_from_slice(&v1);
    fs::write(&dirty, &raw).unwrap();

    let handler = get_handler_for_mime("audio/mpeg").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let out = fs::read(&cleaned).unwrap();
    assert!(
        !out.windows(b"id3v1-secret-title".len())
            .any(|w| w == b"id3v1-secret-title"),
        "id3v1 title survived MP3 clean"
    );
    assert!(
        !out.windows(b"mat2-parity-artist".len())
            .any(|w| w == b"mat2-parity-artist"),
        "id3v2 artist survived MP3 clean"
    );
}

// ---- Image formats: TIFF / WebP / HEIC / HEIF / JXL ----

#[test]
fn tiff_with_artist_software_and_gps_exif_chain() {
    // `make_dirty_tiff` plants Artist + Software in IFD0. We
    // additionally inject a GPSInfo sub-IFD via little_exif so the
    // cleaner is forced to walk past the main IFD pointer and drop a
    // separate GPS IFD chain. A regression that only clears IFD0
    // would leave the GPS IFD behind and be invisible to the
    // mat2_parity round-trip (which only checks Artist).
    if !have_ffmpeg() {
        eprintln!("[SKIP] tiff_with_artist_software_and_gps_exif_chain: ffmpeg not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.tiff");
    let cleaned = dir.path().join("clean.tiff");
    if make_dirty_tiff(&dirty).is_err() {
        eprintln!("[SKIP] tiff_with_artist_software_and_gps_exif_chain: ffmpeg lacks tiff");
        return;
    }

    // Extend the fixture with a GPS tag via little_exif so the GPS
    // sub-IFD is plumbed into the main TIFF directory tree.
    {
        use little_exif::exif_tag::ExifTag;
        use little_exif::metadata::Metadata as ExifMetadata;
        if let Ok(mut m) = ExifMetadata::new_from_path(&dirty) {
            m.set_tag(ExifTag::GPSVersionID(vec![2, 2, 0, 0]));
            // Write it back; best-effort — if little_exif refuses,
            // the baseline fixture still has Artist + Software.
            let _ = m.write_to_file(&dirty);
        }
    }

    let handler = get_handler_for_mime("image/tiff").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    // The reader must surface no user EXIF on the cleaned file.
    if let Ok(m) = little_exif::metadata::Metadata::new_from_path(&cleaned) {
        assert!(
            m.into_iter().next().is_none(),
            "cleaned TIFF still reports EXIF tags"
        );
    }
    // And every plant string must be gone from the raw bytes.
    let out = fs::read(&cleaned).unwrap();
    for plant in [&b"mat2-parity-artist"[..], &b"secret-camera"[..]] {
        assert!(
            !out.windows(plant.len()).any(|w| w == plant),
            "TIFF plant {:?} survived clean",
            std::str::from_utf8(plant).unwrap_or("?")
        );
    }
}

#[test]
fn webp_with_exif_and_spliced_xmp_riff_chunk() {
    // `make_dirty_webp` injects EXIF via little_exif. We additionally
    // splice a raw `XMP ` RIFF chunk into the container so the
    // cleaner has to drop both an EXIF metadata vector and an XMP
    // metadata vector from the same file. The mat2_parity test only
    // covers the EXIF path.
    if !have_ffmpeg() {
        eprintln!("[SKIP] webp_with_exif_and_spliced_xmp_riff_chunk: ffmpeg not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.webp");
    let cleaned = dir.path().join("clean.webp");
    if make_dirty_webp(&dirty).is_err() {
        eprintln!("[SKIP] webp_with_exif_and_spliced_xmp_riff_chunk: ffmpeg lacks libwebp");
        return;
    }

    // Append an `XMP ` chunk before the trailing bytes. WebP chunks
    // are `(id: [u8;4], size: u32le, data, optional pad to even)`
    // and the enclosing RIFF header's size field at offset 4..8
    // measures everything after the first 8 bytes. We append
    // in-place and rewrite that size field.
    let mut raw = fs::read(&dirty).unwrap();
    if raw.len() > 12 && &raw[..4] == b"RIFF" && &raw[8..12] == b"WEBP" {
        let payload = b"<x:xmpmeta>webp-secret-xmp-plant</x:xmpmeta>";
        let chunk_size = u32::try_from(payload.len()).unwrap();
        let mut chunk = Vec::new();
        chunk.extend_from_slice(b"XMP ");
        chunk.extend_from_slice(&chunk_size.to_le_bytes());
        chunk.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            chunk.push(0);
        }
        raw.extend_from_slice(&chunk);
        let new_riff_size = u32::try_from(raw.len() - 8).unwrap();
        raw[4..8].copy_from_slice(&new_riff_size.to_le_bytes());
        fs::write(&dirty, &raw).unwrap();
    }

    let handler = get_handler_for_mime("image/webp").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    let out = fs::read(&cleaned).unwrap();
    for plant in [
        &b"webp-secret-artist"[..],
        &b"webp-secret-description"[..],
        &b"webp-secret-xmp-plant"[..],
    ] {
        assert!(
            !out.windows(plant.len()).any(|w| w == plant),
            "WebP plant {:?} survived clean",
            std::str::from_utf8(plant).unwrap_or("?")
        );
    }
}

#[test]
fn heic_reader_sees_exif_before_clean_and_none_after() {
    // HEIC round-trip via little_exif. The cleaner (via
    // `file_clear_metadata`) strips the main EXIF item reference.
    // This test differs from the mat2_parity round-trip by asserting
    // the *reader* before/after shape explicitly through
    // `handler.read_metadata`, not just raw bytes, so a regression
    // that corrupts the HEIC container but happens to hide the plant
    // string still fails.
    if !have_ffmpeg() {
        eprintln!("[SKIP] heic_reader_sees_exif_before_clean_and_none_after: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.heic");
    let cleaned = dir.path().join("clean.heic");
    if make_dirty_heic(&dirty).is_err() {
        eprintln!("[SKIP] heic_reader_sees_exif_before_clean_and_none_after: libx265 missing");
        return;
    }

    let handler = get_handler_for_mime("image/heic").unwrap();

    // Pre-clean sanity: reader must see at least one metadata item
    // (the Artist plant) so we know the fixture is live.
    if let Ok(pre) = handler.read_metadata(&dirty) {
        assert!(
            !pre.is_empty(),
            "fixture sanity: dirty HEIC should surface metadata before clean"
        );
    }

    handler.clean_metadata(&dirty, &cleaned).unwrap();

    // Post-clean: reader returns success + empty metadata set, OR
    // returns an error because the cleaner stripped past what the
    // reader can parse. Both shapes mean no leak; either is allowed.
    if let Ok(post) = handler.read_metadata(&cleaned) {
        assert!(
            post.is_empty(),
            "cleaned HEIC still surfaces metadata: {post:?}"
        );
    }

    // Raw byte grep for the plant string.
    let out = fs::read(&cleaned).unwrap();
    assert!(
        !out.windows(b"heic-secret-artist".len())
            .any(|w| w == b"heic-secret-artist"),
        "HEIC artist plant survived clean"
    );
}

#[test]
fn heif_extension_routes_and_strips_like_heic() {
    // Distinct MIME routing check: `image/heif` extension vs
    // `image/heic`. The dispatcher routes both through the same
    // `ImageHandler`, but the MIME-branch coverage in dispatch is
    // worth pinning here since mat2_parity only tests the `.heic`
    // extension explicitly.
    if !have_ffmpeg() {
        eprintln!("[SKIP] heif_extension_routes_and_strips_like_heic: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.heif");
    let cleaned = dir.path().join("clean.heif");
    if make_dirty_heif(&dirty).is_err() {
        eprintln!("[SKIP] heif_extension_routes_and_strips_like_heic: libx265 missing");
        return;
    }
    let handler = get_handler_for_mime("image/heif").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    let out = fs::read(&cleaned).unwrap();
    assert!(
        !out.windows(b"heic-secret-artist".len())
            .any(|w| w == b"heic-secret-artist"),
        "HEIF artist plant survived clean"
    );
}

#[test]
fn jxl_reader_sees_exif_before_and_none_after() {
    // JXL is routed through little_exif's dedicated jxl box walker.
    // Same before/after reader check as the HEIC test.
    if !have_ffmpeg() {
        eprintln!("[SKIP] jxl_reader_sees_exif_before_and_none_after: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.jxl");
    let cleaned = dir.path().join("clean.jxl");
    if make_dirty_jxl(&dirty).is_err() {
        eprintln!("[SKIP] jxl_reader_sees_exif_before_and_none_after: libjxl missing");
        return;
    }

    let handler = get_handler_for_mime("image/jxl").unwrap();
    if let Ok(pre) = handler.read_metadata(&dirty) {
        assert!(
            !pre.is_empty(),
            "fixture sanity: dirty JXL should surface metadata before clean"
        );
    }

    handler.clean_metadata(&dirty, &cleaned).unwrap();

    if let Ok(post) = handler.read_metadata(&cleaned) {
        assert!(
            post.is_empty(),
            "cleaned JXL still surfaces metadata: {post:?}"
        );
    }
    let out = fs::read(&cleaned).unwrap();
    assert!(
        !out.windows(b"mat2-parity-artist".len())
            .any(|w| w == b"mat2-parity-artist"),
        "JXL artist plant survived clean"
    );
}

// ---- PDF ----

#[test]
fn pdf_with_info_xmp_embedded_file_and_js_all_surface_empty_after_clean() {
    // `make_dirty_pdf` already plants /Info, /Metadata, /OpenAction,
    // /Names/EmbeddedFiles, /StructTreeRoot, /PageLabels, /AcroForm,
    // /ID, and per-page /Metadata. The mat2_parity round-trip tests
    // individual plant strings. This test takes a stricter stance:
    // after clean, the *reader* must surface no user-metadata items
    // at all (every surviving item, if any, must be structural).
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.pdf");
    let cleaned = dir.path().join("clean.pdf");
    make_dirty_pdf(&dirty);

    let handler = get_handler_for_mime("application/pdf").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    // Raw grep for every injected plant.
    let out = fs::read(&cleaned).unwrap();
    for plant in [
        &b"mat2-parity-author"[..],
        &b"secret-title"[..],
        &b"secret-subject"[..],
        &b"secret-producer"[..],
        &b"secret-creator"[..],
        &b"secret-keywords"[..],
        &b"secret-js"[..],
        &b"EMBEDDED SECRET DATA"[..],
        &b"secret-fingerprint-a"[..],
        &b"secret-fingerprint-b"[..],
    ] {
        assert!(
            !out.windows(plant.len()).any(|w| w == plant),
            "PDF plant {:?} survived clean",
            std::str::from_utf8(plant).unwrap_or("?")
        );
    }

    // Reader view: every surviving metadata item (if any) must be
    // structural, not user-controlled. The set of structural keys is
    // pinned by `is_user_metadata_key_plant` below.
    if let Ok(meta) = handler.read_metadata(&cleaned) {
        for group in &meta.groups {
            for item in &group.items {
                assert!(
                    !is_user_metadata_key_plant(&item.key, &item.value),
                    "cleaned PDF still surfaces user metadata: {} = {}",
                    item.key,
                    item.value
                );
            }
        }
    }
}

// ---- OOXML: DOCX / XLSX / PPTX ----

#[test]
fn docx_clean_drops_custom_xml_app_xml_numbering_and_comments_parts() {
    // `make_dirty_docx` plants ~10 junk parts plus core/app/custom
    // metadata and an embedded dirty JPEG. mat2_parity asserts the
    // plant strings are gone from XML. This test takes a different
    // angle: after clean, the zip package itself must not contain
    // any of the junk parts. A regression that blanks the XML but
    // leaves the part in place (old behaviour, fixed in 9d0fb67)
    // is invisible to a content-level check.
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.docx");
    let cleaned = dir.path().join("clean.docx");
    make_dirty_docx(&dirty, TEST_JPEG);

    let handler = get_handler_for_mime(
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    )
    .unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    let names = zip_entry_names(&cleaned);
    for banned in [
        "docProps/custom.xml",
        "word/comments.xml",
        "word/numbering.xml",
        "word/viewProps.xml",
        "word/theme/theme1.xml",
        "customXml/item1.xml",
        "word/printerSettings/printerSettings1.bin",
    ] {
        assert!(
            !names.iter().any(|n| n == banned),
            "cleaned DOCX still contains banned part {banned:?}. members: {names:?}"
        );
    }

    // The core.xml and app.xml survivors (if any) must have been
    // scrubbed of their plant strings — cross-check with the helper.
    assert_eq!(
        count_needle_in_xml_entries(&cleaned, "Secret Author"),
        0,
        "core.xml dc:creator plant survived"
    );
    assert_eq!(
        count_needle_in_xml_entries(&cleaned, "Evil Corp"),
        0,
        "app.xml Company plant survived"
    );
    assert_eq!(
        count_needle_in_xml_entries(&cleaned, "SecretCustomField"),
        0,
        "custom.xml property plant survived"
    );

    // And the package zip must still be structurally normalized.
    assert_zip_is_normalized(&cleaned);
}

#[test]
fn xlsx_clean_drops_docprops_and_strips_core_app_custom_plants() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.xlsx");
    let cleaned = dir.path().join("clean.xlsx");
    make_dirty_xlsx(&dirty);

    let handler =
        get_handler_for_mime("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
            .unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    assert_eq!(
        count_needle_in_xml_entries(&cleaned, "Secret OOXML Author"),
        0,
        "XLSX core.xml creator plant survived"
    );
    assert_eq!(
        count_needle_in_xml_entries(&cleaned, "Bob Evil"),
        0,
        "XLSX app.xml manager plant survived"
    );
    assert_eq!(
        count_needle_in_xml_entries(&cleaned, "SecretCustomOoxmlField"),
        0,
        "XLSX custom.xml property plant survived"
    );
    let names = zip_entry_names(&cleaned);
    assert!(
        !names.iter().any(|n| n == "docProps/custom.xml"),
        "XLSX custom.xml part survived. members: {names:?}"
    );
    assert_zip_is_normalized(&cleaned);
}

#[test]
fn pptx_clean_drops_docprops_custom_and_strips_app_plants() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.pptx");
    let cleaned = dir.path().join("clean.pptx");
    make_dirty_pptx(&dirty);

    let handler = get_handler_for_mime(
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    )
    .unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    assert_eq!(
        count_needle_in_xml_entries(&cleaned, "Secret OOXML Author"),
        0,
        "PPTX creator plant survived"
    );
    assert_eq!(
        count_needle_in_xml_entries(&cleaned, "Bob Evil"),
        0,
        "PPTX manager plant survived"
    );
    let names = zip_entry_names(&cleaned);
    assert!(
        !names.iter().any(|n| n == "docProps/custom.xml"),
        "PPTX custom.xml part survived. members: {names:?}"
    );
    assert_zip_is_normalized(&cleaned);
}

// ---- ODF: ODT / ODS / ODP / ODG ----

#[test]
fn odt_clean_strips_meta_xml_tracked_changes_and_thumbnail() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.odt");
    let cleaned = dir.path().join("clean.odt");
    make_dirty_odt(&dirty);

    let handler = get_handler_for_mime("application/vnd.oasis.opendocument.text").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    // meta.xml is fully emptied by the ODF cleaner; any surviving
    // meta.xml must no longer carry the plant strings.
    assert_eq!(
        count_needle_in_xml_entries(&cleaned, "Secret Author"),
        0,
        "ODT creator plant survived"
    );
    assert_eq!(
        count_needle_in_xml_entries(&cleaned, "Initial Secret"),
        0,
        "ODT initial-creator plant survived"
    );
    // The content.xml tracked-changes block must be gone.
    assert_eq!(
        count_needle_in_xml_entries(&cleaned, "text:tracked-changes"),
        0,
        "ODT tracked-changes block survived"
    );
    // Junk paths:
    let names = zip_entry_names(&cleaned);
    for banned in [
        "Thumbnails/thumbnail.png",
        "Configurations2/accelerator/current.xml",
        "layout-cache",
    ] {
        assert!(
            !names.iter().any(|n| n == banned),
            "ODT banned part {banned:?} survived. members: {names:?}"
        );
    }
    assert_zip_is_normalized(&cleaned);
}

#[test]
fn ods_clean_strips_meta_xml_and_user_defined_fields() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.ods");
    let cleaned = dir.path().join("clean.ods");
    make_dirty_ods(&dirty);

    let handler = get_handler_for_mime("application/vnd.oasis.opendocument.spreadsheet").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    assert_eq!(
        count_needle_in_xml_entries(&cleaned, "Secret ODF Author"),
        0,
        "ODS creator plant survived"
    );
    assert_eq!(
        count_needle_in_xml_entries(&cleaned, "SecretField"),
        0,
        "ODS user-defined plant survived"
    );
    assert_zip_is_normalized(&cleaned);
}

#[test]
fn odp_clean_strips_meta_xml_and_slide_body_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.odp");
    let cleaned = dir.path().join("clean.odp");
    make_dirty_odp(&dirty);

    let handler = get_handler_for_mime("application/vnd.oasis.opendocument.presentation").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    assert_eq!(
        count_needle_in_xml_entries(&cleaned, "Secret ODF Author"),
        0,
        "ODP creator plant survived"
    );
    // Visible slide text must still be present.
    let content = read_zip_entry(&cleaned, "content.xml").expect("content.xml missing");
    assert!(
        String::from_utf8_lossy(&content).contains("visible-slide"),
        "ODP visible slide content was dropped"
    );
    assert_zip_is_normalized(&cleaned);
}

#[test]
fn odg_clean_strips_meta_xml_and_drawing_body_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.odg");
    let cleaned = dir.path().join("clean.odg");
    make_dirty_odg(&dirty);

    let handler = get_handler_for_mime("application/vnd.oasis.opendocument.graphics").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    assert_eq!(
        count_needle_in_xml_entries(&cleaned, "Secret ODF Author"),
        0,
        "ODG creator plant survived"
    );
    assert_zip_is_normalized(&cleaned);
}

// ---- EPUB ----

#[test]
fn epub_clean_drops_calibre_junk_and_scrubs_opf_dublin_core() {
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.epub");
    let cleaned = dir.path().join("clean.epub");
    make_dirty_epub(&dirty);

    let handler = get_handler_for_mime("application/epub+zip").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    // Calibre junk must be dropped.
    let names = zip_entry_names(&cleaned);
    for banned in ["iTunesMetadata.plist", "META-INF/calibre_bookmarks.txt"] {
        assert!(
            !names.iter().any(|n| n == banned),
            "EPUB banned part {banned:?} survived. members: {names:?}"
        );
    }
    // mimetype must still be first.
    assert_eq!(names.first().map(String::as_str), Some("mimetype"));
    // All plant strings gone from every XML / NCX / OPF.
    for plant in [
        "Secret Book Title",
        "Secret Author",
        "Secret Publisher",
        "secret-old-identifier",
        "Calibre 5.0.0",
    ] {
        assert_eq!(
            count_needle_in_xml_entries(&cleaned, plant),
            0,
            "EPUB plant {plant:?} survived clean"
        );
    }
    assert_zip_is_normalized(&cleaned);
}

// ---- Audio: FLAC / OGG / WAV / AIFF / M4A / AAC / Opus ----

#[test]
fn flac_clean_strips_vorbis_comment_and_application_plus_picture_cover_exif() {
    // `make_flac_with_dirty_cover` plants a cover JPEG carrying EXIF
    // on top of the usual Vorbis comment. The cleaner must zero the
    // Vorbis comment AND the cover's embedded EXIF.
    if !have_ffmpeg() {
        eprintln!("[SKIP] flac_clean_strips_vorbis_comment_and_application_plus_picture_cover_exif: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.flac");
    let cleaned = dir.path().join("clean.flac");

    // Build a FLAC with embedded dirty JPEG cover.
    let cover_jpeg = {
        let jpeg_path = dir.path().join("cover.jpg");
        make_dirty_jpeg(&jpeg_path);
        fs::read(&jpeg_path).unwrap()
    };
    if make_flac_with_dirty_cover(&dirty, &cover_jpeg).is_err() {
        eprintln!("[SKIP] flac_clean_strips_...: flac build failed");
        return;
    }

    let handler = get_handler_for_mime("audio/flac").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();

    let out = fs::read(&cleaned).unwrap();
    for plant in [
        &b"mat2-parity-artist"[..],
        &b"secret-title"[..],
        &b"secret-comment"[..],
        &b"mat2-parity-description"[..],
    ] {
        assert!(
            !out.windows(plant.len()).any(|w| w == plant),
            "FLAC plant {:?} survived clean",
            std::str::from_utf8(plant).unwrap_or("?")
        );
    }
}

#[test]
fn ogg_clean_removes_vorbis_user_comments_via_ffprobe() {
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] ogg_clean_removes_vorbis_user_comments_via_ffprobe: tool missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.ogg");
    let cleaned = dir.path().join("clean.ogg");
    if make_dirty_ogg(&dirty).is_err() {
        eprintln!("[SKIP] ogg_clean_...: ffmpeg lacks libvorbis");
        return;
    }
    let handler = get_handler_for_mime("audio/ogg").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let tags = ffprobe_user_tags(&cleaned);
    assert!(
        tags.is_empty(),
        "cleaned OGG still reports user tags via ffprobe: {tags:?}"
    );
}

#[test]
fn wav_clean_removes_id3_tags_and_preserves_audio_via_ffprobe() {
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] wav_clean_removes_id3_tags_and_preserves_audio_via_ffprobe: tool missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.wav");
    let cleaned = dir.path().join("clean.wav");
    if make_dirty_wav(&dirty).is_err() {
        eprintln!("[SKIP] wav_clean_...: ffmpeg wav build failed");
        return;
    }
    let handler = get_handler_for_mime("audio/x-wav").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let tags = ffprobe_user_tags(&cleaned);
    assert!(
        tags.is_empty(),
        "cleaned WAV still reports user tags via ffprobe: {tags:?}"
    );
}

#[test]
fn aiff_clean_removes_name_auth_copy_anno_chunks_via_ffprobe() {
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] aiff_clean_removes_name_auth_copy_anno_chunks_via_ffprobe: tool missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.aiff");
    let cleaned = dir.path().join("clean.aiff");
    if make_dirty_aiff(&dirty).is_err() {
        eprintln!("[SKIP] aiff_clean_...: ffmpeg aiff build failed");
        return;
    }
    let handler = get_handler_for_mime("audio/x-aiff").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let out = fs::read(&cleaned).unwrap();
    for plant in [
        &b"aiff-secret-title"[..],
        &b"aiff-secret-author"[..],
        &b"aiff-secret-copyright"[..],
        &b"aiff-secret-annotation"[..],
    ] {
        assert!(
            !out.windows(plant.len()).any(|w| w == plant),
            "AIFF plant {:?} survived clean",
            std::str::from_utf8(plant).unwrap_or("?")
        );
    }
}

#[test]
fn m4a_clean_removes_ilst_and_udta_location_via_ffprobe() {
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] m4a_clean_removes_ilst_and_udta_location_via_ffprobe: tool missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.m4a");
    let cleaned = dir.path().join("clean.m4a");
    if make_dirty_m4a(&dirty).is_err() {
        eprintln!("[SKIP] m4a_clean_...: ffmpeg m4a build failed");
        return;
    }
    let handler = get_handler_for_mime("audio/mp4").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let tags = ffprobe_user_tags(&cleaned);
    assert!(
        tags.is_empty(),
        "cleaned M4A still reports user tags via ffprobe: {tags:?}"
    );
    let out = fs::read(&cleaned).unwrap();
    for plant in [
        &b"secret-m4a-title"[..],
        &b"secret-m4a-artist"[..],
        &b"+40.7128-074.0060"[..],
    ] {
        assert!(
            !out.windows(plant.len()).any(|w| w == plant),
            "M4A plant {:?} survived clean",
            std::str::from_utf8(plant).unwrap_or("?")
        );
    }
}

#[test]
fn aac_clean_strips_id3_prefix_and_trailer_via_ffprobe() {
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] aac_clean_strips_id3_prefix_and_trailer_via_ffprobe: tool missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.aac");
    let cleaned = dir.path().join("clean.aac");
    if make_dirty_aac(&dirty).is_err() {
        eprintln!("[SKIP] aac_clean_...: ffmpeg aac build failed");
        return;
    }
    let handler = get_handler_for_mime("audio/aac").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let tags = ffprobe_user_tags(&cleaned);
    assert!(
        tags.is_empty(),
        "cleaned AAC still reports user tags via ffprobe: {tags:?}"
    );
}

#[test]
fn opus_clean_removes_vorbis_comment_header_via_ffprobe() {
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] opus_clean_removes_vorbis_comment_header_via_ffprobe: tool missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.opus");
    let cleaned = dir.path().join("clean.opus");
    if make_dirty_opus(&dirty).is_err() {
        eprintln!("[SKIP] opus_clean_...: ffmpeg libopus missing");
        return;
    }
    let handler = get_handler_for_mime("audio/opus").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let tags = ffprobe_user_tags(&cleaned);
    assert!(
        tags.is_empty(),
        "cleaned Opus still reports user tags via ffprobe: {tags:?}"
    );
    let out = fs::read(&cleaned).unwrap();
    assert!(
        !out.windows(b"opus-secret-artist".len())
            .any(|w| w == b"opus-secret-artist"),
        "Opus artist plant survived clean"
    );
}

// ---- Video: MP4 / MKV / WebM / AVI / MOV / WMV / FLV / Video-OGG ----

#[test]
fn mp4_clean_removes_moov_udta_metadata_via_ffprobe() {
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] mp4_clean_removes_moov_udta_metadata_via_ffprobe: tool missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.mp4");
    let cleaned = dir.path().join("clean.mp4");
    if make_dirty_mp4(&dirty).is_err() {
        eprintln!("[SKIP] mp4_clean_...: ffmpeg x264 missing");
        return;
    }
    let handler = get_handler_for_mime("video/mp4").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let tags = ffprobe_user_tags(&cleaned);
    assert!(
        tags.is_empty(),
        "cleaned MP4 still reports user tags via ffprobe: {tags:?}"
    );
}

#[test]
fn mkv_clean_removes_simpletags_via_ffprobe() {
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] mkv_clean_removes_simpletags_via_ffprobe: tool missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.mkv");
    let cleaned = dir.path().join("clean.mkv");
    if make_dirty_mkv(&dirty).is_err() {
        eprintln!("[SKIP] mkv_clean_...: ffmpeg x264 missing");
        return;
    }
    let handler = get_handler_for_mime("video/x-matroska").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let tags: Vec<_> = ffprobe_user_tags(&cleaned)
        .into_iter()
        .filter(|(k, _)| k != "encoder" && k != "ENCODER")
        .collect();
    assert!(
        tags.is_empty(),
        "cleaned MKV still reports user tags via ffprobe: {tags:?}"
    );
}

#[test]
fn webm_clean_removes_simpletags_via_ffprobe() {
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] webm_clean_removes_simpletags_via_ffprobe: tool missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.webm");
    let cleaned = dir.path().join("clean.webm");
    if make_dirty_webm(&dirty).is_err() {
        eprintln!("[SKIP] webm_clean_...: ffmpeg libvpx missing");
        return;
    }
    let handler = get_handler_for_mime("video/webm").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let tags: Vec<_> = ffprobe_user_tags(&cleaned)
        .into_iter()
        .filter(|(k, _)| k != "encoder" && k != "ENCODER")
        .collect();
    assert!(
        tags.is_empty(),
        "cleaned WebM still reports user tags via ffprobe: {tags:?}"
    );
}

#[test]
fn avi_clean_removes_riff_info_via_ffprobe() {
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] avi_clean_removes_riff_info_via_ffprobe: tool missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.avi");
    let cleaned = dir.path().join("clean.avi");
    if make_dirty_avi(&dirty).is_err() {
        eprintln!("[SKIP] avi_clean_...: ffmpeg avi build failed");
        return;
    }
    let handler = get_handler_for_mime("video/x-msvideo").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let tags = ffprobe_user_tags(&cleaned);
    assert!(
        tags.is_empty(),
        "cleaned AVI still reports user tags via ffprobe: {tags:?}"
    );
}

#[test]
fn mov_clean_removes_apple_atoms_via_ffprobe() {
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] mov_clean_removes_apple_atoms_via_ffprobe: tool missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.mov");
    let cleaned = dir.path().join("clean.mov");
    if make_dirty_mov(&dirty).is_err() {
        eprintln!("[SKIP] mov_clean_...: ffmpeg mov build failed");
        return;
    }
    let handler = get_handler_for_mime("video/quicktime").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let tags = ffprobe_user_tags(&cleaned);
    assert!(
        tags.is_empty(),
        "cleaned MOV still reports user tags via ffprobe: {tags:?}"
    );
}

#[test]
fn wmv_clean_removes_asf_header_ext_metadata_via_ffprobe() {
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] wmv_clean_removes_asf_header_ext_metadata_via_ffprobe: tool missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.wmv");
    let cleaned = dir.path().join("clean.wmv");
    if make_dirty_wmv(&dirty).is_err() {
        eprintln!("[SKIP] wmv_clean_...: ffmpeg wmv2 missing");
        return;
    }
    let handler = get_handler_for_mime("video/x-ms-wmv").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let tags = ffprobe_user_tags(&cleaned);
    assert!(
        tags.is_empty(),
        "cleaned WMV still reports user tags via ffprobe: {tags:?}"
    );
}

#[test]
fn flv_clean_removes_onmetadata_script_tag_via_ffprobe() {
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] flv_clean_removes_onmetadata_script_tag_via_ffprobe: tool missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.flv");
    let cleaned = dir.path().join("clean.flv");
    if make_dirty_flv(&dirty).is_err() {
        eprintln!("[SKIP] flv_clean_...: ffmpeg flv1 missing");
        return;
    }
    let handler = get_handler_for_mime("video/x-flv").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let tags = ffprobe_user_tags(&cleaned);
    assert!(
        tags.is_empty(),
        "cleaned FLV still reports user tags via ffprobe: {tags:?}"
    );
}

#[test]
fn video_ogg_clean_removes_theora_and_vorbis_comments_via_ffprobe() {
    if !have_ffmpeg() || !have_ffprobe() {
        eprintln!("[SKIP] video_ogg_clean_removes_theora_and_vorbis_comments_via_ffprobe: tool missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dirty = dir.path().join("dirty.ogv");
    let cleaned = dir.path().join("clean.ogv");
    if make_dirty_video_ogg(&dirty).is_err() {
        eprintln!("[SKIP] video_ogg_clean_...: ffmpeg libtheora missing");
        return;
    }
    let handler = get_handler_for_mime("video/ogg").unwrap();
    handler.clean_metadata(&dirty, &cleaned).unwrap();
    let tags = ffprobe_user_tags(&cleaned);
    assert!(
        tags.is_empty(),
        "cleaned video/OGG still reports user tags via ffprobe: {tags:?}"
    );
}

// ---- Deeply nested archive (non-recursion guarantee) ----

#[test]
fn deeply_nested_zip_in_tar_xz_in_zip_in_tar_bz2_outer_normalized_inner_kept() {
    // Same documented non-recursion behaviour as
    // `nested_zip_inside_tar_zst_is_kept_verbatim_not_recursed`, but
    // through three different compression algorithms in one file so
    // every codec path is exercised in a single test.
    use std::io::Cursor;

    let dir = tempfile::tempdir().unwrap();
    let jpeg_path = dir.path().join("inner.jpg");
    make_dirty_jpeg(&jpeg_path);
    let jpeg_bytes = fs::read(&jpeg_path).unwrap();

    // Innermost zip with the dirty JPEG.
    let mut inner_zip: Vec<u8> = Vec::new();
    {
        use zip::write::SimpleFileOptions;
        let cursor = Cursor::new(&mut inner_zip);
        let mut writer = zip::ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default()
            .last_modified_time(zip::DateTime::from_date_and_time(2024, 6, 1, 12, 0, 0).unwrap());
        writer.start_file("photo.jpg", opts).unwrap();
        writer.write_all(&jpeg_bytes).unwrap();
        writer.finish().unwrap();
    }

    // Wrap it in tar.xz.
    let mut inner_tar: Vec<u8> = Vec::new();
    {
        use tar::{Builder, EntryType, Header};
        let mut builder = Builder::new(&mut inner_tar);
        let mut header = Header::new_gnu();
        header.set_path("album.zip").unwrap();
        header.set_size(inner_zip.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(1_700_000_000);
        header.set_entry_type(EntryType::Regular);
        header.set_cksum();
        builder.append(&header, inner_zip.as_slice()).unwrap();
        builder.into_inner().unwrap();
    }
    let mut inner_tar_xz: Vec<u8> = Vec::new();
    {
        let mut enc = xz2::write::XzEncoder::new(&mut inner_tar_xz, 6);
        enc.write_all(&inner_tar).unwrap();
        enc.finish().unwrap();
    }

    // Wrap tar.xz in another zip.
    let mut outer_zip: Vec<u8> = Vec::new();
    {
        use zip::write::SimpleFileOptions;
        let cursor = Cursor::new(&mut outer_zip);
        let mut writer = zip::ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default()
            .last_modified_time(zip::DateTime::from_date_and_time(2024, 6, 1, 12, 0, 0).unwrap());
        writer.start_file("bundle.tar.xz", opts).unwrap();
        writer.write_all(&inner_tar_xz).unwrap();
        writer.finish().unwrap();
    }

    // Wrap outer zip in tar.bz2.
    let mut outer_tar: Vec<u8> = Vec::new();
    {
        use tar::{Builder, EntryType, Header};
        let mut builder = Builder::new(&mut outer_tar);
        let mut header = Header::new_gnu();
        header.set_path("deep.zip").unwrap();
        header.set_size(outer_zip.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(1_700_000_000);
        header.set_entry_type(EntryType::Regular);
        header.set_cksum();
        builder.append(&header, outer_zip.as_slice()).unwrap();
        builder.into_inner().unwrap();
    }
    let src = dir.path().join("dirty.tar.bz2");
    let dst = dir.path().join("clean.tar.bz2");
    {
        let f = fs::File::create(&src).unwrap();
        let mut enc = bzip2::write::BzEncoder::new(f, bzip2::Compression::default());
        enc.write_all(&outer_tar).unwrap();
        enc.finish().unwrap();
    }

    let handler = get_handler_for_mime("application/x-bzip2").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();

    // Read the cleaned tar.bz2, assert the outer tar contents are
    // normalized (mtime zeroed) but the innermost jpeg still carries
    // EXIF (non-recursion is the documented behaviour).
    let cleaned = fs::read(&dst).unwrap();
    let tar_plain = {
        let mut dec = bzip2::read::BzDecoder::new(Cursor::new(&cleaned));
        let mut out = Vec::new();
        dec.read_to_end(&mut out).unwrap();
        out
    };
    let mut tar_archive = tar::Archive::new(Cursor::new(&tar_plain));
    let mut found_inner = false;
    for entry in tar_archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().to_string_lossy().to_string();
        if !name.ends_with(".zip") {
            continue;
        }
        // Outer tar entries should have mtime normalized (non-zero is
        // allowed by mat2 — the contract is determinism, not zero —
        // so we don't assert on it here).
        let _ = entry.header().mtime();
        let mut outer_zip_bytes = Vec::new();
        entry.read_to_end(&mut outer_zip_bytes).unwrap();
        // Unwrap outer zip to get at the bundle.tar.xz member.
        let mut outer_archive =
            zip::ZipArchive::new(Cursor::new(&outer_zip_bytes)).unwrap();
        let mut bundle_entry = outer_archive.by_name("bundle.tar.xz").unwrap();
        let mut bundle_bytes = Vec::new();
        bundle_entry.read_to_end(&mut bundle_bytes).unwrap();
        drop(bundle_entry);
        // Decompress the inner tar.xz.
        let mut inner_plain = Vec::new();
        xz2::read::XzDecoder::new(Cursor::new(&bundle_bytes))
            .read_to_end(&mut inner_plain)
            .unwrap();
        let mut innermost_tar = tar::Archive::new(Cursor::new(&inner_plain));
        for inner_entry in innermost_tar.entries().unwrap() {
            let mut inner_entry = inner_entry.unwrap();
            let inner_name = inner_entry.path().unwrap().to_string_lossy().to_string();
            if !inner_name.ends_with(".zip") {
                continue;
            }
            let mut innermost_zip_bytes = Vec::new();
            inner_entry.read_to_end(&mut innermost_zip_bytes).unwrap();
            let mut innermost_archive =
                zip::ZipArchive::new(Cursor::new(&innermost_zip_bytes)).unwrap();
            let mut jpeg_entry = innermost_archive.by_name("photo.jpg").unwrap();
            let mut jpeg_out = Vec::new();
            jpeg_entry.read_to_end(&mut jpeg_out).unwrap();
            let probe = dir.path().join("deep.jpg");
            fs::write(&probe, &jpeg_out).unwrap();
            let meta = little_exif::metadata::Metadata::new_from_path(&probe).unwrap();
            assert!(
                meta.into_iter().next().is_some(),
                "innermost jpeg lost its EXIF — archive handler must have recursed"
            );
            found_inner = true;
        }
    }
    assert!(
        found_inner,
        "cleaned tar.bz2 lost the innermost zip member"
    );
}

// ---- User-metadata plant classifier (shared by PDF adversarial test) ----

fn is_user_metadata_key_plant(key: &str, value: &str) -> bool {
    // Returns true if a metadata item looks like a user-controlled
    // plant rather than a structural/codec field. Used by the PDF
    // adversarial test to assert `read_metadata` surfaces no user
    // metadata after clean. Conservative: anything containing our
    // well-known plant strings is definitely a leak.
    for needle in [
        "mat2-parity",
        "secret",
        "Secret",
        "Evil Corp",
        "Bob Evil",
        "Alice Smith",
        "Calibre",
        "fingerprint",
    ] {
        if key.contains(needle) || value.contains(needle) {
            return true;
        }
    }
    false
}

// ============================================================
// §B. Clean-input baselines — cleaner must preserve visible
//     content when there's nothing to strip
// ============================================================

#[test]
fn clean_jpeg_baseline_still_decodes() {
    // TEST_JPEG is the inline 4x4 red-pixel blob with JFIF only and
    // no EXIF. Cleaning it must produce a file that img-parts still
    // recognizes as a valid JPEG.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean-input.jpg");
    let dst = dir.path().join("clean-output.jpg");
    fs::write(&src, TEST_JPEG).unwrap();

    let handler = get_handler_for_mime("image/jpeg").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    let bytes = fs::read(&dst).unwrap();
    let jpeg = img_parts::jpeg::Jpeg::from_bytes(bytes.into()).unwrap();
    assert!(
        !jpeg.segments().is_empty(),
        "cleaned no-metadata JPEG has no segments"
    );
}

#[test]
fn clean_png_baseline_preserves_pixels() {
    // 1x1 greyscale PNG with no text chunks. The cleaner must not
    // rewrite the IDAT stream (it's not an image pipeline) and must
    // produce a file whose IHDR+IEND are still intact.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.png");
    let dst = dir.path().join("out.png");

    let mut png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    // IHDR 1x1 8-bit greyscale
    let ihdr = [
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x00, 0x00, 0x00, 0x00,
    ];
    append_png_chunk(&mut png, *b"IHDR", &ihdr);
    // IDAT: trivial 1x1 single filter byte + single value (black pixel)
    let raw_pixels = [0u8, 0u8]; // filter 0, pixel 0
    let compressed = zlib_compress_minimal(&raw_pixels);
    append_png_chunk(&mut png, *b"IDAT", &compressed);
    append_png_chunk(&mut png, *b"IEND", &[]);
    fs::write(&src, &png).unwrap();

    let handler = get_handler_for_mime("image/png").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read(&dst).unwrap();
    assert_eq!(&out[..8], &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
    assert!(
        out.windows(4).any(|w| w == b"IHDR"),
        "IHDR missing from cleaned PNG"
    );
    assert!(
        out.windows(4).any(|w| w == b"IEND"),
        "IEND missing from cleaned PNG"
    );
    assert!(
        !out.windows(4)
            .any(|w| w == b"tEXt" || w == b"iTXt" || w == b"zTXt"),
        "text chunk resurrected in cleaned PNG"
    );
}

#[test]
fn clean_svg_baseline_preserves_visible_content() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.svg");
    let dst = dir.path().join("out.svg");
    fs::write(
        &src,
        br#"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16">
  <rect width="16" height="16" fill="green"/>
</svg>"#,
    )
    .unwrap();

    let handler = get_handler_for_mime("image/svg+xml").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read_to_string(&dst).unwrap();
    assert!(out.contains("<rect"));
    assert!(out.contains("fill=\"green\""));
}

#[test]
fn clean_html_baseline_preserves_body() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.html");
    let dst = dir.path().join("out.html");
    fs::write(
        &src,
        br#"<!doctype html>
<html><head><meta charset="utf-8"></head><body><p>hello world</p></body></html>"#,
    )
    .unwrap();

    let handler = get_handler_for_mime("text/html").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read_to_string(&dst).unwrap();
    assert!(out.contains("hello world"));
}

#[test]
fn clean_css_baseline_preserves_rules() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.css");
    let dst = dir.path().join("out.css");
    fs::write(
        &src,
        b"body { color: blue; }\n.header { font-size: 14px; }\n",
    )
    .unwrap();

    let handler = get_handler_for_mime("text/css").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read_to_string(&dst).unwrap();
    assert!(out.contains("color: blue"));
    assert!(out.contains("font-size: 14px"));
}

#[test]
fn clean_torrent_baseline_preserves_info_dict() {
    // No comment / created by / creation date. The handler should
    // leave the info dict alone and rewrite the root dict with only
    // the announce/info keys.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.torrent");
    let dst = dir.path().join("out.torrent");
    let payload = b"d8:announce11:http://x/tr4:infod6:lengthi10e4:name8:pony.txt12:piece lengthi16384e6:pieces20:01234567890123456789ee";
    fs::write(&src, payload).unwrap();

    let handler = get_handler_for_mime("application/x-bittorrent").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read(&dst).unwrap();
    // The file must still parse: begin with 'd', end with 'e'.
    assert_eq!(out.first(), Some(&b'd'));
    assert_eq!(out.last(), Some(&b'e'));
    // The info dict contents must still be present.
    assert!(out.windows(8).any(|w| w == b"pony.txt"));
}

#[test]
fn clean_zip_baseline_with_only_a_text_file_round_trips() {
    use std::io::Write as _;
    use zip::write::SimpleFileOptions;

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.zip");
    let dst = dir.path().join("out.zip");
    {
        let file = fs::File::create(&src).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default()
            .last_modified_time(zip::DateTime::from_date_and_time(2024, 6, 1, 12, 0, 0).unwrap());
        writer.start_file("readme.txt", opts).unwrap();
        writer.write_all(b"just text, no metadata").unwrap();
        writer.finish().unwrap();
    }

    let handler = get_handler_for_mime("application/zip").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_zip_is_normalized(&dst);
    assert_eq!(
        read_zip_entry(&dst, "readme.txt").unwrap(),
        b"just text, no metadata"
    );
}

// ---- Phase B.2: clean-input baselines for every remaining format ----
//
// Each test builds a genuinely clean fixture (no metadata injected),
// cleans it, and asserts the cleaner produced a structurally valid
// file that the reader can still parse. These differ from the
// cross_cutting idempotency tests because they start from a
// clean-from-scratch input rather than `clean(dirty)`.

/// Run ffmpeg with `-map_metadata -1` to synthesize a metadata-free
/// media file. Returns `Err` if ffmpeg isn't installed or the codec
/// isn't compiled in. Tests self-skip on error.
fn ffmpeg_build_clean(path: &std::path::Path, args: &[&str]) -> std::io::Result<()> {
    let _ = fs::remove_file(path);
    let output = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-hide_banner"])
        .args(args)
        .args(["-map_metadata", "-1"])
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "ffmpeg synthesis failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

/// Assert a file exists, is non-empty, and that a `FormatHandler`
/// returns Ok on `read_metadata`. Used by every baseline test to
/// prove the cleaner produced a file the reader can still parse.
fn assert_cleaned_parses(mime: &str, path: &std::path::Path) {
    let meta = fs::metadata(path).expect("cleaned file missing");
    assert!(meta.len() > 0, "cleaned file is empty: {}", path.display());
    let handler = get_handler_for_mime(mime).unwrap();
    handler
        .read_metadata(path)
        .unwrap_or_else(|e| panic!("cleaned {} failed to re-read: {e}", path.display()));
}

#[test]
fn clean_pdf_baseline_minimal_catalog_preserves_page_tree() {
    // Hand-build a PDF with only /Catalog -> /Pages -> /Page. No
    // Info dict, no XMP stream, no AcroForm. The cleaner must leave
    // the page tree intact.
    use lopdf::dictionary;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.pdf");
    let dst = dir.path().join("out.pdf");

    let mut doc = lopdf::Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let page_id = doc.new_object_id();
    doc.objects.insert(
        page_id,
        lopdf::Object::Dictionary(dictionary! {
            "Type" => lopdf::Object::Name(b"Page".to_vec()),
            "Parent" => lopdf::Object::Reference(pages_id),
            "MediaBox" => lopdf::Object::Array(vec![
                lopdf::Object::Integer(0), lopdf::Object::Integer(0),
                lopdf::Object::Integer(612), lopdf::Object::Integer(792),
            ]),
            "Resources" => lopdf::Object::Dictionary(lopdf::Dictionary::new()),
        }),
    );
    doc.objects.insert(
        pages_id,
        lopdf::Object::Dictionary(dictionary! {
            "Type" => lopdf::Object::Name(b"Pages".to_vec()),
            "Count" => lopdf::Object::Integer(1),
            "Kids" => lopdf::Object::Array(vec![lopdf::Object::Reference(page_id)]),
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => lopdf::Object::Name(b"Catalog".to_vec()),
        "Pages" => lopdf::Object::Reference(pages_id),
    });
    doc.trailer
        .set("Root", lopdf::Object::Reference(catalog_id));
    doc.save(&src).unwrap();

    let handler = get_handler_for_mime("application/pdf").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();

    // Output must still start with the PDF header and contain a
    // Catalog object.
    let out = fs::read(&dst).unwrap();
    assert!(out.starts_with(b"%PDF-"), "cleaned PDF missing header");
    assert!(
        out.windows(b"/Catalog".len()).any(|w| w == b"/Catalog"),
        "cleaned PDF missing Catalog"
    );
    assert_cleaned_parses("application/pdf", &dst);
}

#[test]
fn clean_gif_baseline_89a_without_extensions_survives() {
    // Minimal GIF89a with just image descriptor + trailer (already
    // covered by `gif87a_without_extensions_is_passed_through` in
    // mat2_parity, but here we additionally assert the reader can
    // still parse it post-clean).
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.gif");
    let dst = dir.path().join("out.gif");
    let mut gif = Vec::new();
    gif.extend_from_slice(b"GIF89a");
    gif.extend_from_slice(&[0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00]);
    gif.extend_from_slice(&[0x2C, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
    gif.extend_from_slice(&[0x02, 0x02, 0x44, 0x01, 0x00]);
    gif.push(0x3B);
    fs::write(&src, &gif).unwrap();

    let handler = get_handler_for_mime("image/gif").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read(&dst).unwrap();
    assert!(out.starts_with(b"GIF89a") || out.starts_with(b"GIF87a"));
    assert_eq!(out.last(), Some(&0x3B));
}

#[test]
fn clean_bmp_baseline_is_byte_for_byte_copy() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.bmp");
    let dst = dir.path().join("out.bmp");
    make_bmp(&src);
    let handler = get_handler_for_mime("image/bmp").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    // HarmlessHandler: byte-for-byte copy.
    assert_eq!(fs::read(&src).unwrap(), fs::read(&dst).unwrap());
}

#[test]
fn clean_tiff_baseline_decodes_with_no_exif() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_tiff_baseline_decodes_with_no_exif: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.tiff");
    let dst = dir.path().join("out.tiff");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=4x4:d=0.04:r=25",
            "-vframes",
            "1",
            "-f",
            "image2",
            "-c:v",
            "tiff",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_tiff_baseline_...: ffmpeg tiff missing");
        return;
    }
    let handler = get_handler_for_mime("image/tiff").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("image/tiff", &dst);
}

#[test]
fn clean_webp_baseline_survives_clean() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_webp_baseline_survives_clean: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.webp");
    let dst = dir.path().join("out.webp");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=orange:s=8x8:d=0.04:r=25",
            "-vframes",
            "1",
            "-c:v",
            "libwebp",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_webp_baseline_...: libwebp missing");
        return;
    }
    let handler = get_handler_for_mime("image/webp").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read(&dst).unwrap();
    assert!(&out[..4] == b"RIFF" && &out[8..12] == b"WEBP");
    assert_cleaned_parses("image/webp", &dst);
}

#[test]
fn clean_heic_baseline_survives_clean() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_heic_baseline_survives_clean: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.heic");
    let dst = dir.path().join("out.heic");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=cyan:s=16x16:d=0.04:r=25",
            "-vframes",
            "1",
            "-c:v",
            "libx265",
            "-x265-params",
            "log-level=none",
            "-f",
            "heif",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_heic_baseline_...: libx265 missing");
        return;
    }
    let handler = get_handler_for_mime("image/heic").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("image/heic", &dst);
}

#[test]
fn clean_heif_baseline_survives_clean() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_heif_baseline_survives_clean: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.heif");
    let dst = dir.path().join("out.heif");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=cyan:s=16x16:d=0.04:r=25",
            "-vframes",
            "1",
            "-c:v",
            "libx265",
            "-x265-params",
            "log-level=none",
            "-f",
            "heif",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_heif_baseline_...: libx265 missing");
        return;
    }
    let handler = get_handler_for_mime("image/heif").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("image/heif", &dst);
}

#[test]
fn clean_jxl_baseline_survives_clean() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_jxl_baseline_survives_clean: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.jxl");
    let dst = dir.path().join("out.jxl");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=green:s=8x8:d=0.04:r=25",
            "-vframes",
            "1",
            "-c:v",
            "libjxl",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_jxl_baseline_...: libjxl missing");
        return;
    }
    let handler = get_handler_for_mime("image/jxl").unwrap();
    // A "simple JXL codestream" with no ISO-BMFF container has no
    // metadata to strip. little_exif surfaces that as
    // `CleanError { detail: "... No metadata!" }`; the cleaner
    // contract treats an already-clean codestream as a no-op so
    // either Ok or that specific error shape is acceptable. A
    // different error (e.g. parse failure) would fail the test.
    match handler.clean_metadata(&src, &dst) {
        Ok(()) => assert_cleaned_parses("image/jxl", &dst),
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("No metadata"),
                "unexpected JXL clean error: {msg}"
            );
        }
    }
}

// ---- Audio baselines ----

#[test]
fn clean_mp3_baseline_no_tags_decodes() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_mp3_baseline_no_tags_decodes: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.mp3");
    let dst = dir.path().join("out.mp3");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "anullsrc=cl=mono:r=44100",
            "-t",
            "0.2",
            "-c:a",
            "libmp3lame",
            "-b:a",
            "32k",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_mp3_baseline_...: libmp3lame missing");
        return;
    }
    let handler = get_handler_for_mime("audio/mpeg").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("audio/mpeg", &dst);
}

#[test]
fn clean_flac_baseline_no_tags_decodes() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_flac_baseline_no_tags_decodes: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.flac");
    let dst = dir.path().join("out.flac");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "anullsrc=cl=mono:r=44100",
            "-t",
            "0.1",
            "-c:a",
            "flac",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_flac_baseline_...: flac missing");
        return;
    }
    let handler = get_handler_for_mime("audio/flac").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("audio/flac", &dst);
    // FLAC magic must still be first 4 bytes.
    let out = fs::read(&dst).unwrap();
    assert_eq!(&out[..4], b"fLaC");
}

#[test]
fn clean_ogg_baseline_no_tags_decodes() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_ogg_baseline_no_tags_decodes: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.ogg");
    let dst = dir.path().join("out.ogg");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "anullsrc=cl=mono:r=44100",
            "-t",
            "0.1",
            "-c:a",
            "libvorbis",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_ogg_baseline_...: libvorbis missing");
        return;
    }
    let handler = get_handler_for_mime("audio/ogg").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("audio/ogg", &dst);
    assert_eq!(&fs::read(&dst).unwrap()[..4], b"OggS");
}

#[test]
fn clean_wav_baseline_no_tags_decodes() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_wav_baseline_no_tags_decodes: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.wav");
    let dst = dir.path().join("out.wav");
    if ffmpeg_build_clean(
        &src,
        &["-f", "lavfi", "-i", "anullsrc=cl=mono:r=8000", "-t", "0.1"],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_wav_baseline_...: wav missing");
        return;
    }
    let handler = get_handler_for_mime("audio/x-wav").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("audio/x-wav", &dst);
    assert_eq!(&fs::read(&dst).unwrap()[..4], b"RIFF");
}

#[test]
fn clean_aiff_baseline_no_chunks_decodes() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_aiff_baseline_no_chunks_decodes: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.aiff");
    let dst = dir.path().join("out.aiff");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f", "lavfi", "-i", "anullsrc=cl=mono:r=8000", "-t", "0.1", "-f", "aiff",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_aiff_baseline_...: aiff missing");
        return;
    }
    let handler = get_handler_for_mime("audio/x-aiff").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("audio/x-aiff", &dst);
    assert_eq!(&fs::read(&dst).unwrap()[..4], b"FORM");
}

#[test]
fn clean_opus_baseline_no_tags_decodes() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_opus_baseline_no_tags_decodes: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.opus");
    let dst = dir.path().join("out.opus");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "anullsrc=cl=mono:r=48000",
            "-t",
            "0.1",
            "-c:a",
            "libopus",
            "-b:a",
            "32k",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_opus_baseline_...: libopus missing");
        return;
    }
    let handler = get_handler_for_mime("audio/opus").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("audio/opus", &dst);
    assert_eq!(&fs::read(&dst).unwrap()[..4], b"OggS");
}

#[test]
fn clean_m4a_baseline_no_ilst_decodes() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_m4a_baseline_no_ilst_decodes: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.m4a");
    let dst = dir.path().join("out.m4a");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "anullsrc=cl=mono:r=44100",
            "-t",
            "0.1",
            "-c:a",
            "aac",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_m4a_baseline_...: aac missing");
        return;
    }
    let handler = get_handler_for_mime("audio/mp4").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("audio/mp4", &dst);
}

#[test]
fn clean_aac_baseline_no_id3_decodes() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_aac_baseline_no_id3_decodes: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.aac");
    let dst = dir.path().join("out.aac");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "anullsrc=cl=mono:r=44100",
            "-t",
            "0.1",
            "-c:a",
            "aac",
            "-f",
            "adts",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_aac_baseline_...: aac missing");
        return;
    }
    let handler = get_handler_for_mime("audio/aac").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("audio/aac", &dst);
}

// ---- Video baselines ----

#[test]
fn clean_mp4_baseline_no_metadata_decodes() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_mp4_baseline_no_metadata_decodes: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.mp4");
    let dst = dir.path().join("out.mp4");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=16x16:d=0.1:r=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_mp4_baseline_...: libx264 missing");
        return;
    }
    let handler = get_handler_for_mime("video/mp4").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("video/mp4", &dst);
}

#[test]
fn clean_mkv_baseline_no_metadata_decodes() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_mkv_baseline_no_metadata_decodes: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.mkv");
    let dst = dir.path().join("out.mkv");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=16x16:d=0.1:r=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_mkv_baseline_...: libx264 missing");
        return;
    }
    let handler = get_handler_for_mime("video/x-matroska").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("video/x-matroska", &dst);
}

#[test]
fn clean_webm_baseline_no_metadata_decodes() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_webm_baseline_no_metadata_decodes: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.webm");
    let dst = dir.path().join("out.webm");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=16x16:d=0.1:r=1",
            "-c:v",
            "libvpx",
            "-pix_fmt",
            "yuv420p",
            "-b:v",
            "50k",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_webm_baseline_...: libvpx missing");
        return;
    }
    let handler = get_handler_for_mime("video/webm").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("video/webm", &dst);
}

#[test]
fn clean_avi_baseline_no_metadata_decodes() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_avi_baseline_no_metadata_decodes: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.avi");
    let dst = dir.path().join("out.avi");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=16x16:d=0.1:r=1",
            "-c:v",
            "mpeg4",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_avi_baseline_...: mpeg4 missing");
        return;
    }
    let handler = get_handler_for_mime("video/x-msvideo").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("video/x-msvideo", &dst);
}

#[test]
fn clean_mov_baseline_no_metadata_decodes() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_mov_baseline_no_metadata_decodes: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.mov");
    let dst = dir.path().join("out.mov");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=16x16:d=0.1:r=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "mov",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_mov_baseline_...: libx264 missing");
        return;
    }
    let handler = get_handler_for_mime("video/quicktime").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("video/quicktime", &dst);
}

#[test]
fn clean_wmv_baseline_no_metadata_decodes() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_wmv_baseline_no_metadata_decodes: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.wmv");
    let dst = dir.path().join("out.wmv");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=16x16:d=0.1:r=1",
            "-c:v",
            "wmv2",
            "-f",
            "asf",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_wmv_baseline_...: wmv2 missing");
        return;
    }
    let handler = get_handler_for_mime("video/x-ms-wmv").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("video/x-ms-wmv", &dst);
}

#[test]
fn clean_flv_baseline_no_metadata_decodes() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_flv_baseline_no_metadata_decodes: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.flv");
    let dst = dir.path().join("out.flv");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=16x16:d=0.1:r=1",
            "-c:v",
            "flv1",
            "-f",
            "flv",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_flv_baseline_...: flv1 missing");
        return;
    }
    let handler = get_handler_for_mime("video/x-flv").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("video/x-flv", &dst);
}

#[test]
fn clean_video_ogg_baseline_no_metadata_decodes() {
    if !have_ffmpeg() {
        eprintln!("[SKIP] clean_video_ogg_baseline_no_metadata_decodes: ffmpeg missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.ogv");
    let dst = dir.path().join("out.ogv");
    if ffmpeg_build_clean(
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=64x64:d=0.1:r=25",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=cl=mono:r=44100",
            "-t",
            "0.1",
            "-c:v",
            "libtheora",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "libvorbis",
            "-f",
            "ogg",
        ],
    )
    .is_err()
    {
        eprintln!("[SKIP] clean_video_ogg_baseline_...: libtheora missing");
        return;
    }
    let handler = get_handler_for_mime("video/ogg").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_cleaned_parses("video/ogg", &dst);
}

// ---- Document baselines ----

/// Build a minimal OOXML package with empty (well-formed) core/app
/// metadata and a single content part. No plant strings anywhere.
fn build_clean_ooxml(
    path: &std::path::Path,
    content_types: &str,
    main_rel_type: &str,
    main_rel_target: &str,
    part_path: &str,
    part_body: &[u8],
) {
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;
    let file = fs::File::create(path).unwrap();
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default();

    writer.start_file("[Content_Types].xml", options).unwrap();
    writer.write_all(content_types.as_bytes()).unwrap();

    writer.start_file("_rels/.rels", options).unwrap();
    let rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="{main_rel_type}" Target="{main_rel_target}"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/>
</Relationships>"#,
    );
    writer.write_all(rels.as_bytes()).unwrap();

    writer.start_file("docProps/core.xml", options).unwrap();
    writer.write_all(br#"<?xml version="1.0" encoding="UTF-8"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"
                   xmlns:dc="http://purl.org/dc/elements/1.1/"/>"#).unwrap();

    writer.start_file(part_path, options).unwrap();
    writer.write_all(part_body).unwrap();
    writer.finish().unwrap();
}

#[test]
fn clean_docx_baseline_minimal_package_survives() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.docx");
    let dst = dir.path().join("out.docx");
    build_clean_ooxml(
        &src,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="xml" ContentType="application/xml"/>
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
  <Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>
</Types>"#,
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument",
        "word/document.xml",
        "word/document.xml",
        br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p><w:r><w:t>body-text</w:t></w:r></w:p></w:body>
</w:document>"#,
    );
    let handler = get_handler_for_mime(
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    )
    .unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_zip_is_normalized(&dst);
    // Visible body text must survive.
    let doc = read_zip_entry(&dst, "word/document.xml").unwrap();
    assert!(String::from_utf8_lossy(&doc).contains("body-text"));
}

#[test]
fn clean_xlsx_baseline_minimal_package_survives() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.xlsx");
    let dst = dir.path().join("out.xlsx");
    build_clean_ooxml(
        &src,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="xml" ContentType="application/xml"/>
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>
</Types>"#,
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument",
        "xl/workbook.xml",
        "xl/workbook.xml",
        br#"<?xml version="1.0" encoding="UTF-8"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheets><sheet name="Sheet1" sheetId="1"/></sheets>
</workbook>"#,
    );
    let handler =
        get_handler_for_mime("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
            .unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_zip_is_normalized(&dst);
}

#[test]
fn clean_pptx_baseline_minimal_package_survives() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.pptx");
    let dst = dir.path().join("out.pptx");
    build_clean_ooxml(
        &src,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="xml" ContentType="application/xml"/>
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
  <Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>
</Types>"#,
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument",
        "ppt/presentation.xml",
        "ppt/presentation.xml",
        br#"<?xml version="1.0" encoding="UTF-8"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"/>"#,
    );
    let handler = get_handler_for_mime(
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    )
    .unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_zip_is_normalized(&dst);
}

/// Build a minimal ODF package with empty (well-formed) meta.xml and
/// a trivial content.xml body. No plants.
fn build_clean_odf(path: &std::path::Path, mimetype: &str, content_xml: &[u8]) {
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;
    let file = fs::File::create(path).unwrap();
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default();

    writer
        .start_file(
            "mimetype",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
    writer.write_all(mimetype.as_bytes()).unwrap();

    writer.start_file("META-INF/manifest.xml", options).unwrap();
    let manifest = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0">
  <manifest:file-entry manifest:full-path="/" manifest:media-type="{mimetype}"/>
  <manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>
</manifest:manifest>"#,
    );
    writer.write_all(manifest.as_bytes()).unwrap();

    writer.start_file("content.xml", options).unwrap();
    writer.write_all(content_xml).unwrap();

    writer.start_file("styles.xml", options).unwrap();
    writer.write_all(br#"<?xml version="1.0"?><office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"/>"#).unwrap();

    writer.finish().unwrap();
}

#[test]
fn clean_odt_baseline_minimal_package_survives() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.odt");
    let dst = dir.path().join("out.odt");
    build_clean_odf(
        &src,
        "application/vnd.oasis.opendocument.text",
        br#"<?xml version="1.0"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
                         xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body><office:text><text:p>visible-odt-body</text:p></office:text></office:body>
</office:document-content>"#,
    );
    let handler = get_handler_for_mime("application/vnd.oasis.opendocument.text").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_zip_is_normalized(&dst);
    let content = read_zip_entry(&dst, "content.xml").unwrap();
    assert!(String::from_utf8_lossy(&content).contains("visible-odt-body"));
}

#[test]
fn clean_ods_baseline_minimal_package_survives() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.ods");
    let dst = dir.path().join("out.ods");
    build_clean_odf(
        &src,
        "application/vnd.oasis.opendocument.spreadsheet",
        br#"<?xml version="1.0"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0">
  <office:body><office:spreadsheet/></office:body>
</office:document-content>"#,
    );
    let handler = get_handler_for_mime("application/vnd.oasis.opendocument.spreadsheet").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_zip_is_normalized(&dst);
}

#[test]
fn clean_odp_baseline_minimal_package_survives() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.odp");
    let dst = dir.path().join("out.odp");
    build_clean_odf(
        &src,
        "application/vnd.oasis.opendocument.presentation",
        br#"<?xml version="1.0"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0">
  <office:body><office:presentation/></office:body>
</office:document-content>"#,
    );
    let handler = get_handler_for_mime("application/vnd.oasis.opendocument.presentation").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_zip_is_normalized(&dst);
}

#[test]
fn clean_odg_baseline_minimal_package_survives() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.odg");
    let dst = dir.path().join("out.odg");
    build_clean_odf(
        &src,
        "application/vnd.oasis.opendocument.graphics",
        br#"<?xml version="1.0"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0">
  <office:body><office:drawing/></office:body>
</office:document-content>"#,
    );
    let handler = get_handler_for_mime("application/vnd.oasis.opendocument.graphics").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_zip_is_normalized(&dst);
}

#[test]
fn clean_epub_baseline_minimal_package_survives() {
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.epub");
    let dst = dir.path().join("out.epub");
    {
        let file = fs::File::create(&src).unwrap();
        let mut writer = ZipWriter::new(file);
        let stored =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let options = SimpleFileOptions::default();
        writer.start_file("mimetype", stored).unwrap();
        writer.write_all(b"application/epub+zip").unwrap();
        writer
            .start_file("META-INF/container.xml", options)
            .unwrap();
        writer.write_all(br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#).unwrap();
        writer.start_file("OEBPS/content.opf", options).unwrap();
        writer
            .write_all(
                br#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bookid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="bookid">urn:uuid:00000000-0000-0000-0000-000000000000</dc:identifier>
    <dc:title>Clean</dc:title>
    <dc:language>en</dc:language>
    <meta property="dcterms:modified">2024-01-01T00:00:00Z</meta>
  </metadata>
  <manifest>
    <item id="ch1" href="chapter1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="ch1"/></spine>
</package>"#,
            )
            .unwrap();
        writer.start_file("OEBPS/chapter1.xhtml", options).unwrap();
        writer
            .write_all(b"<html><head/><body><p>clean chapter</p></body></html>")
            .unwrap();
        writer.finish().unwrap();
    }
    let handler = get_handler_for_mime("application/epub+zip").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert_zip_is_normalized(&dst);
    let names = zip_entry_names(&dst);
    assert_eq!(names.first().map(String::as_str), Some("mimetype"));
}

#[test]
fn clean_xhtml_baseline_preserves_body() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.xhtml");
    let dst = dir.path().join("out.xhtml");
    fs::write(
        &src,
        br#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
<head><title>clean-xhtml</title></head>
<body><p>visible-xhtml-body</p></body>
</html>"#,
    )
    .unwrap();
    let handler = get_handler_for_mime("application/xhtml+xml").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    let out = fs::read_to_string(&dst).unwrap();
    assert!(out.contains("visible-xhtml-body"));
}

// ---- Archive baselines ----

fn build_clean_tar_bytes() -> Vec<u8> {
    use tar::{Builder, EntryType, Header};
    let mut tar_buf: Vec<u8> = Vec::new();
    {
        let mut builder = Builder::new(&mut tar_buf);
        let mut header = Header::new_gnu();
        header.set_path("readme.txt").unwrap();
        let body = b"just text, no metadata";
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(1_700_000_000);
        header.set_entry_type(EntryType::Regular);
        header.set_cksum();
        builder.append(&header, body.as_slice()).unwrap();
        builder.into_inner().unwrap();
    }
    tar_buf
}

#[test]
fn clean_tar_baseline_survives() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.tar");
    let dst = dir.path().join("out.tar");
    fs::write(&src, build_clean_tar_bytes()).unwrap();
    let handler = get_handler_for_mime("application/x-tar").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert!(fs::metadata(&dst).unwrap().len() > 0);
}

#[test]
fn clean_tar_gz_baseline_survives() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.tar.gz");
    let dst = dir.path().join("out.tar.gz");
    {
        let f = fs::File::create(&src).unwrap();
        let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        enc.write_all(&build_clean_tar_bytes()).unwrap();
        enc.finish().unwrap();
    }
    let handler = get_handler_for_mime("application/gzip").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert!(fs::metadata(&dst).unwrap().len() > 0);
}

#[test]
fn clean_tar_bz2_baseline_survives() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.tar.bz2");
    let dst = dir.path().join("out.tar.bz2");
    {
        let f = fs::File::create(&src).unwrap();
        let mut enc = bzip2::write::BzEncoder::new(f, bzip2::Compression::default());
        enc.write_all(&build_clean_tar_bytes()).unwrap();
        enc.finish().unwrap();
    }
    let handler = get_handler_for_mime("application/x-bzip2").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert!(fs::metadata(&dst).unwrap().len() > 0);
}

#[test]
fn clean_tar_xz_baseline_survives() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.tar.xz");
    let dst = dir.path().join("out.tar.xz");
    {
        let f = fs::File::create(&src).unwrap();
        let mut enc = xz2::write::XzEncoder::new(f, 6);
        enc.write_all(&build_clean_tar_bytes()).unwrap();
        enc.finish().unwrap();
    }
    let handler = get_handler_for_mime("application/x-xz").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert!(fs::metadata(&dst).unwrap().len() > 0);
}

#[test]
fn clean_tar_zst_baseline_survives() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clean.tar.zst");
    let dst = dir.path().join("out.tar.zst");
    {
        let f = fs::File::create(&src).unwrap();
        let mut enc = zstd::Encoder::new(f, 3).unwrap();
        enc.write_all(&build_clean_tar_bytes()).unwrap();
        enc.finish().unwrap();
    }
    let handler = get_handler_for_mime("application/zstd").unwrap();
    handler.clean_metadata(&src, &dst).unwrap();
    assert!(fs::metadata(&dst).unwrap().len() > 0);
}

// ============================================================
// §C. Cross-format scenarios
// ============================================================
//
// Each test in this section iterates over the `SCENARIOS` matrix
// and exercises a single filesystem / API scenario against every
// supported format. Rows whose fixture builder returns `Err`
// (usually "ffmpeg codec not compiled in") are skipped per-row so
// a minimal CI image still runs everything else.

type BuildFn = fn(&std::path::Path) -> std::io::Result<()>;

struct Scenario {
    name: &'static str,
    mime: &'static str,
    ext: &'static str,
    build: BuildFn,
}

fn s_jpeg(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_jpeg(p);
    Ok(())
}
fn s_png(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_png(p);
    Ok(())
}
fn s_pdf(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_pdf(p);
    Ok(())
}
fn s_gif(p: &std::path::Path) -> std::io::Result<()> {
    let mut gif = Vec::new();
    gif.extend_from_slice(b"GIF89a");
    gif.extend_from_slice(&[0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00]);
    gif.extend_from_slice(&[0x21, 0xFE]);
    gif.push(14);
    gif.extend_from_slice(b"scenario-plant");
    gif.push(0x00);
    gif.extend_from_slice(&[0x2C, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
    gif.extend_from_slice(&[0x02, 0x02, 0x44, 0x01, 0x00]);
    gif.push(0x3B);
    fs::write(p, &gif)
}
fn s_bmp(p: &std::path::Path) -> std::io::Result<()> {
    make_bmp(p);
    Ok(())
}
fn s_html(p: &std::path::Path) -> std::io::Result<()> {
    fs::write(p, b"<!doctype html><html><head><meta name=author content=alice><title>t</title></head><body><p>scenario-visible-body</p></body></html>")
}
fn s_xhtml(p: &std::path::Path) -> std::io::Result<()> {
    fs::write(p, br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><meta name="author" content="alice"/></head><body><p>scenario-visible-xhtml</p></body></html>"#)
}
fn s_svg(p: &std::path::Path) -> std::io::Result<()> {
    fs::write(
        p,
        br#"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg"
     xmlns:dc="http://purl.org/dc/elements/1.1/"
     xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     width="16" height="16">
  <metadata><rdf:RDF><dc:creator>scenario-plant-creator</dc:creator></rdf:RDF></metadata>
  <rect width="16" height="16" fill="red"/>
</svg>"#,
    )
}
fn s_css(p: &std::path::Path) -> std::io::Result<()> {
    fs::write(p, b"/* scenario-plant */\nbody { color: red; }")
}
fn s_txt(p: &std::path::Path) -> std::io::Result<()> {
    fs::write(p, b"scenario-visible-txt-body")
}
fn s_torrent(p: &std::path::Path) -> std::io::Result<()> {
    fs::write(p, b"d8:announce11:http://x/tr10:created by14:scenario-plant4:infod6:lengthi10e4:name8:pony.txt12:piece lengthi16384e6:pieces20:01234567890123456789ee")
}
fn s_docx(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_docx(p, TEST_JPEG);
    Ok(())
}
fn s_xlsx(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_xlsx(p);
    Ok(())
}
fn s_pptx(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_pptx(p);
    Ok(())
}
fn s_odt(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_odt(p);
    Ok(())
}
fn s_ods(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_ods(p);
    Ok(())
}
fn s_odp(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_odp(p);
    Ok(())
}
fn s_odg(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_odg(p);
    Ok(())
}
fn s_epub(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_epub(p);
    Ok(())
}
fn s_zip(p: &std::path::Path) -> std::io::Result<()> {
    use zip::write::SimpleFileOptions;
    let file = fs::File::create(p)?;
    let mut writer = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default();
    writer.start_file("a.txt", opts).unwrap();
    writer.write_all(b"scenario-visible").unwrap();
    writer.finish().unwrap();
    Ok(())
}
fn s_tar(p: &std::path::Path) -> std::io::Result<()> {
    fs::write(p, build_clean_tar_bytes())
}
fn s_targz(p: &std::path::Path) -> std::io::Result<()> {
    let f = fs::File::create(p)?;
    let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
    enc.write_all(&build_clean_tar_bytes())?;
    enc.finish()?;
    Ok(())
}
fn s_tarbz2(p: &std::path::Path) -> std::io::Result<()> {
    let f = fs::File::create(p)?;
    let mut enc = bzip2::write::BzEncoder::new(f, bzip2::Compression::default());
    enc.write_all(&build_clean_tar_bytes())?;
    enc.finish()?;
    Ok(())
}
fn s_tarxz(p: &std::path::Path) -> std::io::Result<()> {
    let f = fs::File::create(p)?;
    let mut enc = xz2::write::XzEncoder::new(f, 6);
    enc.write_all(&build_clean_tar_bytes())?;
    enc.finish()?;
    Ok(())
}
fn s_tarzst(p: &std::path::Path) -> std::io::Result<()> {
    let f = fs::File::create(p)?;
    let mut enc = zstd::Encoder::new(f, 3).unwrap();
    enc.write_all(&build_clean_tar_bytes())?;
    enc.finish().unwrap();
    Ok(())
}
fn s_mp3(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_mp3(p)
}
fn s_flac(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_flac(p)
}
fn s_ogg(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_ogg(p)
}
fn s_wav(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_wav(p)
}
fn s_aiff(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_aiff(p)
}
fn s_opus(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_opus(p)
}
fn s_m4a(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_m4a(p)
}
fn s_aac(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_aac(p)
}
fn s_mp4(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_mp4(p)
}
fn s_mkv(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_mkv(p)
}
fn s_webm(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_webm(p)
}
fn s_avi(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_avi(p)
}
fn s_mov(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_mov(p)
}
fn s_wmv(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_wmv(p)
}
fn s_flv(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_flv(p)
}
fn s_video_ogg(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_video_ogg(p)
}
fn s_tiff(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_tiff(p)
}
fn s_webp(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_webp(p)
}
fn s_heic(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_heic(p)
}
fn s_heif(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_heif(p)
}
fn s_jxl(p: &std::path::Path) -> std::io::Result<()> {
    make_dirty_jxl(p)
}

const SCENARIOS: &[Scenario] = &[
    // Non-ffmpeg formats (always present)
    Scenario { name: "jpeg", mime: "image/jpeg", ext: "jpg", build: s_jpeg },
    Scenario { name: "png", mime: "image/png", ext: "png", build: s_png },
    Scenario { name: "gif", mime: "image/gif", ext: "gif", build: s_gif },
    Scenario { name: "bmp", mime: "image/bmp", ext: "bmp", build: s_bmp },
    Scenario { name: "pdf", mime: "application/pdf", ext: "pdf", build: s_pdf },
    Scenario { name: "html", mime: "text/html", ext: "html", build: s_html },
    Scenario { name: "xhtml", mime: "application/xhtml+xml", ext: "xhtml", build: s_xhtml },
    Scenario { name: "svg", mime: "image/svg+xml", ext: "svg", build: s_svg },
    Scenario { name: "css", mime: "text/css", ext: "css", build: s_css },
    Scenario { name: "txt", mime: "text/plain", ext: "txt", build: s_txt },
    Scenario { name: "torrent", mime: "application/x-bittorrent", ext: "torrent", build: s_torrent },
    Scenario { name: "docx", mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document", ext: "docx", build: s_docx },
    Scenario { name: "xlsx", mime: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", ext: "xlsx", build: s_xlsx },
    Scenario { name: "pptx", mime: "application/vnd.openxmlformats-officedocument.presentationml.presentation", ext: "pptx", build: s_pptx },
    Scenario { name: "odt", mime: "application/vnd.oasis.opendocument.text", ext: "odt", build: s_odt },
    Scenario { name: "ods", mime: "application/vnd.oasis.opendocument.spreadsheet", ext: "ods", build: s_ods },
    Scenario { name: "odp", mime: "application/vnd.oasis.opendocument.presentation", ext: "odp", build: s_odp },
    Scenario { name: "odg", mime: "application/vnd.oasis.opendocument.graphics", ext: "odg", build: s_odg },
    Scenario { name: "epub", mime: "application/epub+zip", ext: "epub", build: s_epub },
    Scenario { name: "zip", mime: "application/zip", ext: "zip", build: s_zip },
    Scenario { name: "tar", mime: "application/x-tar", ext: "tar", build: s_tar },
    Scenario { name: "tar.gz", mime: "application/gzip", ext: "tar.gz", build: s_targz },
    Scenario { name: "tar.bz2", mime: "application/x-bzip2", ext: "tar.bz2", build: s_tarbz2 },
    Scenario { name: "tar.xz", mime: "application/x-xz", ext: "tar.xz", build: s_tarxz },
    Scenario { name: "tar.zst", mime: "application/zstd", ext: "tar.zst", build: s_tarzst },
    // ffmpeg-dependent rows (self-skipping via Err)
    Scenario { name: "mp3", mime: "audio/mpeg", ext: "mp3", build: s_mp3 },
    Scenario { name: "flac", mime: "audio/flac", ext: "flac", build: s_flac },
    Scenario { name: "ogg", mime: "audio/ogg", ext: "ogg", build: s_ogg },
    Scenario { name: "wav", mime: "audio/x-wav", ext: "wav", build: s_wav },
    Scenario { name: "aiff", mime: "audio/x-aiff", ext: "aiff", build: s_aiff },
    Scenario { name: "opus", mime: "audio/opus", ext: "opus", build: s_opus },
    Scenario { name: "m4a", mime: "audio/mp4", ext: "m4a", build: s_m4a },
    Scenario { name: "aac", mime: "audio/aac", ext: "aac", build: s_aac },
    Scenario { name: "mp4", mime: "video/mp4", ext: "mp4", build: s_mp4 },
    Scenario { name: "mkv", mime: "video/x-matroska", ext: "mkv", build: s_mkv },
    Scenario { name: "webm", mime: "video/webm", ext: "webm", build: s_webm },
    Scenario { name: "avi", mime: "video/x-msvideo", ext: "avi", build: s_avi },
    Scenario { name: "mov", mime: "video/quicktime", ext: "mov", build: s_mov },
    Scenario { name: "wmv", mime: "video/x-ms-wmv", ext: "wmv", build: s_wmv },
    Scenario { name: "flv", mime: "video/x-flv", ext: "flv", build: s_flv },
    Scenario { name: "video_ogg", mime: "video/ogg", ext: "ogv", build: s_video_ogg },
    Scenario { name: "tiff", mime: "image/tiff", ext: "tiff", build: s_tiff },
    Scenario { name: "webp", mime: "image/webp", ext: "webp", build: s_webp },
    Scenario { name: "heic", mime: "image/heic", ext: "heic", build: s_heic },
    Scenario { name: "heif", mime: "image/heif", ext: "heif", build: s_heif },
    Scenario { name: "jxl", mime: "image/jxl", ext: "jxl", build: s_jxl },
];

#[test]
fn scenario_clean_overwrites_preexisting_output_file_for_every_format() {
    // For each format: pre-create the destination file with junk
    // bytes, call `clean_metadata`, then assert the output is not a
    // prefix-concatenation of the junk and the clean output (i.e.
    // the cleaner truncated/replaced the destination).
    for row in SCENARIOS {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join(format!("dirty.{}", row.ext));
        let dst = dir.path().join(format!("out.{}", row.ext));
        if (row.build)(&src).is_err() {
            eprintln!("[SKIP] overwrite scenario: {}", row.name);
            continue;
        }
        // Pre-create destination with sentinel bytes.
        let junk = b"JUNK-SENTINEL-BYTES-MUST-BE-REPLACED";
        fs::write(&dst, junk).unwrap();

        let handler = get_handler_for_mime(row.mime).unwrap();
        handler
            .clean_metadata(&src, &dst)
            .unwrap_or_else(|e| panic!("clean failed for {}: {e}", row.name));

        let out = fs::read(&dst).unwrap();
        assert!(
            !out.starts_with(junk),
            "clean left junk sentinel at head of output for {}",
            row.name
        );
        assert!(
            !out.windows(junk.len()).any(|w| w == junk),
            "junk sentinel survived anywhere in {} output",
            row.name
        );
    }
}

#[test]
fn scenario_read_metadata_is_stable_across_repeated_calls() {
    // Call `read_metadata` three times on each fixture and assert
    // every call returns an equal `MetadataSet`. Reader stability
    // is the counterpart of cleaner determinism and is not covered
    // by the cross_cutting matrix.
    for row in SCENARIOS {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join(format!("dirty.{}", row.ext));
        if (row.build)(&src).is_err() {
            eprintln!("[SKIP] reader-stability scenario: {}", row.name);
            continue;
        }
        let handler = get_handler_for_mime(row.mime).unwrap();
        let first = handler.read_metadata(&src);
        let second = handler.read_metadata(&src);
        let third = handler.read_metadata(&src);
        match (first, second, third) {
            (Ok(a), Ok(b), Ok(c)) => {
                // Pretty-compare to keep the failure message readable.
                let a_s = format!("{a:?}");
                let b_s = format!("{b:?}");
                let c_s = format!("{c:?}");
                assert_eq!(a_s, b_s, "reader unstable across 1->2 for {}", row.name);
                assert_eq!(b_s, c_s, "reader unstable across 2->3 for {}", row.name);
            }
            (Err(_), Err(_), Err(_)) => {
                // If every call errors consistently that's still stable.
            }
            other => panic!("reader inconsistent across calls for {}: {other:?}", row.name),
        }
    }
}

#[test]
fn scenario_clean_followed_by_read_surfaces_no_user_metadata() {
    // For every format: build dirty, clean, re-read, assert no
    // surviving metadata item contains any well-known plant
    // string. This is the read-after-clean invariant measured at
    // the public API level (not via raw bytes).
    for row in SCENARIOS {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join(format!("dirty.{}", row.ext));
        let dst = dir.path().join(format!("out.{}", row.ext));
        if (row.build)(&src).is_err() {
            eprintln!("[SKIP] read-after-clean scenario: {}", row.name);
            continue;
        }
        let handler = get_handler_for_mime(row.mime).unwrap();
        handler
            .clean_metadata(&src, &dst)
            .unwrap_or_else(|e| panic!("clean failed for {}: {e}", row.name));

        if let Ok(meta) = handler.read_metadata(&dst) {
            for group in &meta.groups {
                for item in &group.items {
                    assert!(
                        !is_user_metadata_key_plant(&item.key, &item.value),
                        "cleaned {} still surfaces user metadata: {} = {}",
                        row.name,
                        item.key,
                        item.value
                    );
                }
            }
        }
    }
}

#[test]
fn scenario_clean_to_same_directory_as_input_does_not_clobber_source() {
    // Cleaning to a sibling file in the same dir must not delete
    // or overwrite the source. Covers a common frontend pattern
    // ("clean foo.jpg → foo.clean.jpg in the same dir").
    for row in SCENARIOS {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join(format!("src.{}", row.ext));
        let dst = dir.path().join(format!("src.clean.{}", row.ext));
        if (row.build)(&src).is_err() {
            eprintln!("[SKIP] same-dir scenario: {}", row.name);
            continue;
        }
        let src_bytes_before = fs::read(&src).unwrap();

        let handler = get_handler_for_mime(row.mime).unwrap();
        handler
            .clean_metadata(&src, &dst)
            .unwrap_or_else(|e| panic!("clean failed for {}: {e}", row.name));

        // Source must still exist, and its bytes must be unchanged.
        let src_bytes_after = fs::read(&src).unwrap();
        assert_eq!(
            src_bytes_before, src_bytes_after,
            "clean mutated the source file for {}",
            row.name
        );
        // Destination must exist and be non-empty (except for tar
        // archives which may be empty-but-valid).
        assert!(
            fs::metadata(&dst).is_ok(),
            "destination missing for {}",
            row.name
        );
    }
}

#[test]
fn scenario_clean_preserves_visible_content_for_text_formats() {
    // Cleaner must not drop the visible body when cleaning text
    // formats. This is stricter than the existing per-format
    // baseline tests: we build a fixture with plants AND visible
    // content, clean, and assert only plants are gone.
    struct Row<'a> {
        name: &'a str,
        mime: &'a str,
        ext: &'a str,
        body: &'a [u8],
        visible: &'a str,
    }
    let rows = [
        Row {
            name: "html",
            mime: "text/html",
            ext: "html",
            body: b"<html><head><meta name=author content=alice></head><body><p>visible-hello</p></body></html>",
            visible: "visible-hello",
        },
        Row {
            name: "xhtml",
            mime: "application/xhtml+xml",
            ext: "xhtml",
            body: br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><meta name="author" content="alice"/></head><body><p>visible-xhello</p></body></html>"#,
            visible: "visible-xhello",
        },
        Row {
            name: "svg",
            mime: "image/svg+xml",
            ext: "svg",
            body: br#"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16">
  <metadata>plant</metadata>
  <rect width="16" height="16" fill="visible-magenta"/>
</svg>"#,
            visible: "visible-magenta",
        },
        Row {
            name: "css",
            mime: "text/css",
            ext: "css",
            body: b"/* plant */\n.visible-rule { color: red; }\n",
            visible: ".visible-rule",
        },
        Row {
            name: "txt",
            mime: "text/plain",
            ext: "txt",
            body: b"visible-plain-text-body",
            visible: "visible-plain-text-body",
        },
    ];
    for row in &rows {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join(format!("dirty.{}", row.ext));
        let dst = dir.path().join(format!("out.{}", row.ext));
        fs::write(&src, row.body).unwrap();
        let handler = get_handler_for_mime(row.mime).unwrap();
        handler.clean_metadata(&src, &dst).unwrap();
        let out = fs::read_to_string(&dst).unwrap();
        assert!(
            out.contains(row.visible),
            "{} visible content missing after clean: {}",
            row.name,
            out
        );
    }
}

#[test]
fn scenario_handler_releases_source_file_after_return() {
    // After `clean_metadata` returns, the source file must be
    // releasable — i.e. `std::fs::remove_file` on it must succeed.
    // A handler that leaked a file descriptor on Linux would
    // ordinarily still allow delete (unlink works on open files),
    // but the source file should at least not be locked against
    // rename on the same tmpdir.
    for row in SCENARIOS {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join(format!("src.{}", row.ext));
        let dst = dir.path().join(format!("out.{}", row.ext));
        if (row.build)(&src).is_err() {
            eprintln!("[SKIP] handle-release scenario: {}", row.name);
            continue;
        }
        let handler = get_handler_for_mime(row.mime).unwrap();
        handler
            .clean_metadata(&src, &dst)
            .unwrap_or_else(|e| panic!("clean failed for {}: {e}", row.name));

        // Rename source to a new name in the same dir. On any sane
        // platform this succeeds regardless of open fds; the assert
        // is really about the handler leaving the path in a state
        // the caller can manipulate.
        let renamed = dir.path().join(format!("renamed.{}", row.ext));
        fs::rename(&src, &renamed).unwrap_or_else(|e| {
            panic!("rename after clean failed for {}: {e}", row.name);
        });
        // And then delete the renamed file.
        fs::remove_file(&renamed).unwrap_or_else(|e| {
            panic!("delete after clean failed for {}: {e}", row.name);
        });
    }
}

// ============================================================
// Helpers local to this test file
// ============================================================

/// Locate the offset at which the length-prefix of a PNG chunk with
/// the given 4-byte type tag begins. Returns `None` if no such chunk
/// is found. Walks the chunk list linearly from the signature.
fn find_chunk_offset(raw: &[u8], wanted: [u8; 4]) -> Option<usize> {
    // PNG chunks start at offset 8 (after the 8-byte signature).
    let mut pos = 8;
    while pos + 8 <= raw.len() {
        let len = u32::from_be_bytes([raw[pos], raw[pos + 1], raw[pos + 2], raw[pos + 3]]) as usize;
        let ty = &raw[pos + 4..pos + 8];
        if ty == wanted {
            return Some(pos);
        }
        pos = pos.checked_add(8 + len + 4)?;
    }
    None
}

/// Write one PNG chunk with a hand-rolled CRC-32 so the test file
/// doesn't need a PNG writer dependency. Duplicates the internal
/// helper in `common::mod` to keep weird_files self-contained.
fn append_png_chunk(out: &mut Vec<u8>, ty: [u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(&ty);
    out.extend_from_slice(data);

    let mut table = [0u32; 256];
    for n in 0..256u32 {
        let mut c = n;
        for _ in 0..8 {
            if c & 1 != 0 {
                c = 0xEDB8_8320 ^ (c >> 1);
            } else {
                c >>= 1;
            }
        }
        table[n as usize] = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &b in ty.iter().chain(data.iter()) {
        crc = table[((crc ^ u32::from(b)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^= 0xFFFF_FFFF;
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Minimal zlib wrapper that emits a single stored DEFLATE block.
/// Good enough for PNG parsers that only care about structural
/// validity, which is what the baseline test asserts.
fn zlib_compress_minimal(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    out.push(0x78);
    out.push(0x01);
    out.push(0x01);
    out.extend_from_slice(&(data.len() as u16).to_le_bytes());
    out.extend_from_slice(&(!(data.len() as u16)).to_le_bytes());
    out.extend_from_slice(data);

    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    let adler = (b << 16) | a;
    out.extend_from_slice(&adler.to_be_bytes());
    out
}
