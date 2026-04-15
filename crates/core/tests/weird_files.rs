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
