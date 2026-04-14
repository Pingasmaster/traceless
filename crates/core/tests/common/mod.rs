//! Shared helpers for the mat2-parity integration test suite.
//!
//! Every public helper here generates a minimum-viable "dirty" file on
//! disk (or in memory) using only the dependencies traceless-core
//! already pulls in — no test fixtures are checked in. Helpers that need
//! ffmpeg self-skip via `have_ffmpeg()` so the suite still runs on
//! minimal CI images.

#![allow(dead_code)]

#![allow(clippy::unwrap_used)]
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use lopdf::dictionary;
use lopdf::{Dictionary, Object, Stream};

// Re-export the big TEST_JPEG constant out of the in-crate unit test
// suite would be nice but that module is gated on `#[cfg(test)]`, so
// we inline a small valid JPEG here. 4×4 red pixel, JFIF only.
pub const TEST_JPEG: &[u8] = &[
    0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00, 0x01,
    0x00, 0x01, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x03, 0x02, 0x02, 0x02, 0x02, 0x02, 0x03,
    0x02, 0x02, 0x02, 0x03, 0x03, 0x03, 0x03, 0x04, 0x06, 0x04, 0x04, 0x04, 0x04, 0x04, 0x08, 0x06,
    0x06, 0x05, 0x06, 0x09, 0x08, 0x0A, 0x0A, 0x09, 0x08, 0x09, 0x09, 0x0A, 0x0C, 0x0F, 0x0C, 0x0A,
    0x0B, 0x0E, 0x0B, 0x09, 0x09, 0x0D, 0x11, 0x0D, 0x0E, 0x0F, 0x10, 0x10, 0x11, 0x10, 0x0A, 0x0C,
    0x12, 0x13, 0x12, 0x10, 0x13, 0x0F, 0x10, 0x10, 0x10, 0xFF, 0xDB, 0x00, 0x43, 0x01, 0x03, 0x03,
    0x03, 0x04, 0x03, 0x04, 0x08, 0x04, 0x04, 0x08, 0x10, 0x0B, 0x09, 0x0B, 0x10, 0x10, 0x10, 0x10,
    0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10,
    0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10,
    0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0xFF, 0xC0,
    0x00, 0x11, 0x08, 0x00, 0x04, 0x00, 0x04, 0x03, 0x01, 0x11, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11,
    0x01, 0xFF, 0xC4, 0x00, 0x14, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0xFF, 0xC4, 0x00, 0x14, 0x10, 0x01, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xC4, 0x00,
    0x15, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x07, 0x09, 0xFF, 0xC4, 0x00, 0x14, 0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xDA, 0x00, 0x0C, 0x03, 0x01,
    0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3F, 0x00, 0x3A, 0x03, 0x15, 0x4D, 0xFF, 0xD9,
];

// ---------- Infrastructure ----------

pub fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn have_ffprobe() -> bool {
    Command::new("ffprobe")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Skip the current test if the given tool isn't available. Prints a
/// user-visible line to the test output so the skip is obvious.
#[macro_export]
macro_rules! skip_if_no {
    ($tool:expr) => {
        if !$tool() {
            eprintln!("[SKIP] {}: {} not available", module_path!(), stringify!($tool));
            return;
        }
    };
}

// ---------- Dirty-image builders ----------

/// Write a valid JPEG with an Artist EXIF tag. little_exif is the
/// canonical reader so we use it to embed the tag the same way the rest
/// of the crate will read it back.
pub fn make_dirty_jpeg(path: &Path) {
    use little_exif::exif_tag::ExifTag;
    use little_exif::metadata::Metadata as ExifMetadata;

    std::fs::write(path, TEST_JPEG).unwrap();

    let mut exif = ExifMetadata::new();
    exif.set_tag(ExifTag::Artist("mat2-parity-artist".to_string()));
    exif.set_tag(ExifTag::ImageDescription(
        "mat2-parity-description".to_string(),
    ));
    exif.write_to_file(path).unwrap();

    // Sanity check: the tag must actually be there before the test runs.
    let read_back = ExifMetadata::new_from_path(path).unwrap();
    assert!(
        read_back.into_iter().next().is_some(),
        "fixture JPEG should have EXIF before cleaning"
    );
}

/// Build a minimum-viable PNG (2×2 RGB, zlib-compressed single IDAT) and
/// append a tEXt chunk so there is a textual metadata leak to strip.
pub fn make_dirty_png(path: &Path) {
    // PNG signature
    let mut png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

    // IHDR: 2×2, 8-bit RGB, no interlace
    let ihdr = [
        0x00, 0x00, 0x00, 0x02, // width = 2
        0x00, 0x00, 0x00, 0x02, // height = 2
        0x08, // bit depth
        0x02, // color type (RGB)
        0x00, 0x00, 0x00, // compression, filter, interlace
    ];
    append_png_chunk(&mut png, *b"IHDR", &ihdr);

    // tEXt: "Author\0mat2-parity" + "Software\0secret-tool"
    let mut text = Vec::new();
    text.extend_from_slice(b"Author\0mat2-parity-author");
    append_png_chunk(&mut png, *b"tEXt", &text);

    let mut sw = Vec::new();
    sw.extend_from_slice(b"Software\0secret-tool");
    append_png_chunk(&mut png, *b"tEXt", &sw);

    // tIME chunk: 1995-06-15 12:00:00
    let time = [0x07, 0xCB, 0x06, 0x0F, 0x0C, 0x00, 0x00];
    append_png_chunk(&mut png, *b"tIME", &time);

    // IDAT: 2×2 RGB means 2 rows of (1 filter byte + 6 pixel bytes) = 14 raw bytes.
    // We compress with flate2 so it parses as a valid PNG.
    let raw: Vec<u8> = vec![
        0, // filter
        255, 0, 0, 0, 255, 0, // row 0
        0, // filter
        0, 0, 255, 255, 255, 0, // row 1
    ];
    let compressed = zlib_compress(&raw);
    append_png_chunk(&mut png, *b"IDAT", &compressed);

    // IEND
    append_png_chunk(&mut png, *b"IEND", &[]);

    std::fs::write(path, &png).unwrap();
}

fn append_png_chunk(out: &mut Vec<u8>, ty: [u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(&ty);
    out.extend_from_slice(data);

    let crc = crc32_png(ty, data);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn crc32_png(ty: [u8; 4], data: &[u8]) -> u32 {
    // Standard CRC-32 (polynomial 0xEDB8_8320) over type + data, as
    // required by the PNG spec.
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
    crc ^ 0xFFFF_FFFF
}

/// Minimal zlib (no external crate): emit a single stored DEFLATE block
/// wrapped in the 2-byte zlib header and the trailing adler32. Output is
/// valid enough for PNG decoders to accept.
fn zlib_compress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    // zlib header: CM=8 (deflate), CINFO=7 (default), FLG=0x01
    out.push(0x78);
    out.push(0x01);

    // Stored block: one non-final header then final header (splits if > 65535)
    let mut remaining = data;
    while !remaining.is_empty() {
        let chunk_len = remaining.len().min(0xFFFF);
        let last = chunk_len == remaining.len();
        out.push(u8::from(last));
        out.extend_from_slice(&(chunk_len as u16).to_le_bytes());
        out.extend_from_slice(&(!(chunk_len as u16)).to_le_bytes());
        out.extend_from_slice(&remaining[..chunk_len]);
        remaining = &remaining[chunk_len..];
    }

    // Adler-32 trailer
    let adler = adler32(data);
    out.extend_from_slice(&adler.to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

// ---------- Dirty-PDF builder ----------

/// Build a minimum-viable PDF that has as many metadata leaks as lopdf
/// lets us inject without a full page tree. We create:
/// - /Info dict with Author, Producer, CreationDate, ModDate
/// - /Catalog with /Metadata stream (XMP), /OpenAction, /Names/EmbeddedFiles,
///   /AcroForm, /StructTreeRoot, /MarkInfo, /PieceInfo, /PageLabels
pub fn make_dirty_pdf(path: &Path) {
    let mut doc = lopdf::Document::with_version("1.7");

    // --- /Info dict
    let info_id = doc.add_object(dictionary! {
        "Author" => Object::string_literal("mat2-parity-author"),
        "Title" => Object::string_literal("secret-title"),
        "Subject" => Object::string_literal("secret-subject"),
        "Keywords" => Object::string_literal("secret keywords"),
        "Creator" => Object::string_literal("secret-creator"),
        "Producer" => Object::string_literal("secret-producer"),
        "CreationDate" => Object::string_literal("D:20240101000000Z"),
        "ModDate" => Object::string_literal("D:20240102000000Z"),
    });
    doc.trailer.set("Info", Object::Reference(info_id));

    // --- /Metadata stream (XMP packet)
    let xmp_bytes = br#"<?xpacket begin='' id='W5M0MpCehiHzreSzNTczkc9d'?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="test">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about=""><dc:creator>secret</dc:creator></rdf:Description>
</rdf:RDF></x:xmpmeta><?xpacket end='w'?>"#;
    let mut meta_dict = Dictionary::new();
    meta_dict.set("Type", Object::Name(b"Metadata".to_vec()));
    meta_dict.set("Subtype", Object::Name(b"XML".to_vec()));
    let meta_id = doc.add_object(Object::Stream(Stream::new(meta_dict, xmp_bytes.to_vec())));

    // --- /OpenAction (JavaScript)
    let js_id = doc.add_object(dictionary! {
        "Type" => Object::Name(b"Action".to_vec()),
        "S" => Object::Name(b"JavaScript".to_vec()),
        "JS" => Object::string_literal("app.alert('secret-js');"),
    });

    // --- /AcroForm
    let acroform_id = doc.add_object(dictionary! {
        "Fields" => Object::Array(vec![]),
        "NeedAppearances" => Object::Boolean(true),
    });

    // --- /Names/EmbeddedFiles
    let ef_stream_id = doc.add_object(Object::Stream(Stream::new(
        Dictionary::new(),
        b"EMBEDDED SECRET DATA".to_vec(),
    )));
    let ef_spec_id = doc.add_object(dictionary! {
        "Type" => Object::Name(b"Filespec".to_vec()),
        "F" => Object::string_literal("secret.txt"),
        "EF" => dictionary! { "F" => Object::Reference(ef_stream_id) },
    });
    let ef_names_id = doc.add_object(dictionary! {
        "Names" => Object::Array(vec![
            Object::string_literal("secret.txt"),
            Object::Reference(ef_spec_id),
        ]),
    });
    let names_id = doc.add_object(dictionary! {
        "EmbeddedFiles" => Object::Reference(ef_names_id),
    });

    // --- /StructTreeRoot
    let struct_id = doc.add_object(dictionary! {
        "Type" => Object::Name(b"StructTreeRoot".to_vec()),
    });

    // --- empty /Pages + one blank page so PDF has a valid page tree
    let pages_id = doc.new_object_id();
    let page_id = doc.new_object_id();
    doc.objects.insert(
        page_id,
        Object::Dictionary(dictionary! {
            "Type" => Object::Name(b"Page".to_vec()),
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => Object::Array(vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(612), Object::Integer(792),
            ]),
            "Resources" => Object::Dictionary(Dictionary::new()),
            // per-page leaks
            "Metadata" => Object::Reference(meta_id),
            "Annots" => Object::Array(vec![]),
        }),
    );
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => Object::Name(b"Pages".to_vec()),
            "Count" => Object::Integer(1),
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
        }),
    );

    // --- /Catalog
    let catalog_id = doc.add_object(dictionary! {
        "Type" => Object::Name(b"Catalog".to_vec()),
        "Pages" => Object::Reference(pages_id),
        "Metadata" => Object::Reference(meta_id),
        "OpenAction" => Object::Reference(js_id),
        "AcroForm" => Object::Reference(acroform_id),
        "Names" => Object::Reference(names_id),
        "StructTreeRoot" => Object::Reference(struct_id),
        "MarkInfo" => dictionary! { "Marked" => Object::Boolean(true) },
        "PageLabels" => dictionary! {
            "Nums" => Object::Array(vec![
                Object::Integer(0),
                dictionary! { "S" => Object::Name(b"D".to_vec()) }.into(),
            ]),
        },
    });
    doc.trailer.set("Root", Object::Reference(catalog_id));

    // --- /ID (trailer)
    doc.trailer.set(
        "ID",
        Object::Array(vec![
            Object::string_literal("secret-fingerprint-a"),
            Object::string_literal("secret-fingerprint-b"),
        ]),
    );

    doc.save(path).unwrap();
}

// ---------- Dirty-office builders ----------

pub fn make_dirty_docx(path: &Path, embedded_jpeg: &[u8]) {
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    let file = std::fs::File::create(path).unwrap();
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default();

    writer.start_file("[Content_Types].xml", options).unwrap();
    writer
        .write_all(
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="xml" ContentType="application/xml"/>
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
  <Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/>
  <Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>
  <Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>
</Types>"#,
        )
        .unwrap();

    writer.start_file("_rels/.rels", options).unwrap();
    writer
        .write_all(br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/>
</Relationships>"#).unwrap();

    writer
        .start_file("word/_rels/document.xml.rels", options)
        .unwrap();
    writer
        .write_all(br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.jpeg"/>
</Relationships>"#).unwrap();

    writer.start_file("docProps/core.xml", options).unwrap();
    writer.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"
                   xmlns:dc="http://purl.org/dc/elements/1.1/"
                   xmlns:dcterms="http://purl.org/dc/terms/">
  <dc:creator>Secret Author</dc:creator>
  <cp:lastModifiedBy>Alice Smith</cp:lastModifiedBy>
  <dc:title>Secret Title</dc:title>
  <dcterms:created xsi:type="dcterms:W3CDTF" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">2024-03-14T00:00:00Z</dcterms:created>
</cp:coreProperties>"#).unwrap();

    writer.start_file("docProps/app.xml", options).unwrap();
    writer.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties">
  <Application>Microsoft Office Word</Application>
  <AppVersion>16.0</AppVersion>
  <Company>Evil Corp</Company>
  <Manager>Bob Evil</Manager>
</Properties>"#).unwrap();

    writer.start_file("docProps/custom.xml", options).unwrap();
    writer.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties">
  <property name="SecretCustomField">leak-me</property>
</Properties>"#).unwrap();

    writer.start_file("word/document.xml", options).unwrap();
    writer.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" mc:Ignorable="w14">
  <w:body>
    <w:p w:rsidR="00112233" w:rsidRPr="00AABBCC" w:rsidRDefault="00DDEEFF">
      <w:rPr><w:rFonts w:ascii="Times"/></w:rPr>
      <w:commentRangeStart w:id="1"/>
      <w:r><w:t>visible-content</w:t></w:r>
      <w:commentRangeEnd w:id="1"/>
      <w:commentReference w:id="1"/>
    </w:p>
    <w:p>
      <w:del w:id="2" w:author="deleter"><w:r><w:t>secret-deleted</w:t></w:r></w:del>
      <w:ins w:id="3" w:author="inserter"><w:r><w:t>inserted-survives</w:t></w:r></w:ins>
    </w:p>
    <w:rsids><w:rsidRoot w:val="00FFEEDD"/><w:rsid w:val="00112233"/></w:rsids>
  </w:body>
</w:document>"#).unwrap();

    // Junk files that must be omitted
    writer.start_file("word/comments.xml", options).unwrap();
    writer
        .write_all(b"<comments>should be dropped</comments>")
        .unwrap();

    writer.start_file("customXml/item1.xml", options).unwrap();
    writer.write_all(b"<junk/>").unwrap();

    writer.start_file("word/viewProps.xml", options).unwrap();
    writer.write_all(b"<viewProps/>").unwrap();

    writer
        .start_file("word/printerSettings/printerSettings1.bin", options)
        .unwrap();
    writer.write_all(b"some bytes").unwrap();

    writer
        .start_file("word/theme/theme1.xml", options)
        .unwrap();
    writer.write_all(b"<theme/>").unwrap();

    // Embedded media — the EXIF-recursion vector
    writer
        .start_file("word/media/image1.jpeg", options)
        .unwrap();
    writer.write_all(embedded_jpeg).unwrap();

    writer.finish().unwrap();
}

pub fn make_dirty_odt(path: &Path) {
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    let file = std::fs::File::create(path).unwrap();
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default();

    // mimetype must be first and stored uncompressed per ODF spec
    writer
        .start_file(
            "mimetype",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
    writer
        .write_all(b"application/vnd.oasis.opendocument.text")
        .unwrap();

    writer.start_file("META-INF/manifest.xml", options).unwrap();
    writer.write_all(br#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0">
  <manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text"/>
  <manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>
</manifest:manifest>"#).unwrap();

    writer.start_file("meta.xml", options).unwrap();
    writer
        .write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
                      xmlns:dc="http://purl.org/dc/elements/1.1/"
                      xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0">
  <office:meta>
    <dc:creator>Secret Author</dc:creator>
    <meta:initial-creator>Initial Secret</meta:initial-creator>
    <meta:creation-date>2024-03-14T00:00:00</meta:creation-date>
    <meta:generator>LibreOffice/7.6</meta:generator>
  </office:meta>
</office:document-meta>"#,
        )
        .unwrap();

    writer.start_file("content.xml", options).unwrap();
    writer.write_all(br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
                         xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:tracked-changes>
        <text:changed-region>
          <text:insertion>
            <office:change-info>
              <dc:creator xmlns:dc="http://purl.org/dc/elements/1.1/">secret-author</dc:creator>
              <dc:date xmlns:dc="http://purl.org/dc/elements/1.1/">2024-03-14T01:00:00</dc:date>
            </office:change-info>
          </text:insertion>
        </text:changed-region>
      </text:tracked-changes>
      <text:p>visible-body</text:p>
    </office:text>
  </office:body>
</office:document-content>"#).unwrap();

    writer.start_file("styles.xml", options).unwrap();
    writer
        .write_all(
            br#"<?xml version="1.0"?><office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"/>"#,
        )
        .unwrap();

    // Junk that must be dropped
    writer
        .start_file("Thumbnails/thumbnail.png", options)
        .unwrap();
    writer.write_all(b"fake-thumbnail-data").unwrap();

    writer
        .start_file("Configurations2/accelerator/current.xml", options)
        .unwrap();
    writer.write_all(b"<junk/>").unwrap();

    writer.start_file("layout-cache", options).unwrap();
    writer.write_all(b"layout-junk").unwrap();

    writer.finish().unwrap();
}

pub fn make_dirty_epub(path: &Path) {
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    let file = std::fs::File::create(path).unwrap();
    let mut writer = ZipWriter::new(file);
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let options = SimpleFileOptions::default();

    writer.start_file("mimetype", stored).unwrap();
    writer.write_all(b"application/epub+zip").unwrap();

    writer.start_file("META-INF/container.xml", options).unwrap();
    writer.write_all(br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#).unwrap();

    writer.start_file("OEBPS/content.opf", options).unwrap();
    writer.write_all(br#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bookid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
    <dc:title>Secret Book Title</dc:title>
    <dc:creator>Secret Author</dc:creator>
    <dc:publisher>Secret Publisher</dc:publisher>
    <dc:date>2024-03-14</dc:date>
    <dc:identifier id="bookid">secret-old-identifier</dc:identifier>
    <dc:language>en</dc:language>
    <meta property="dcterms:modified">2024-03-14T00:00:00Z</meta>
  </metadata>
  <manifest>
    <item id="toc" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
    <item id="ch1" href="chapter1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine toc="toc"><itemref idref="ch1"/></spine>
</package>"#).unwrap();

    writer.start_file("OEBPS/toc.ncx", options).unwrap();
    writer.write_all(br#"<?xml version="1.0"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/">
  <head>
    <meta name="dtb:uid" content="secret-uid"/>
    <meta name="dtb:generator" content="Calibre 5.0.0"/>
  </head>
  <docTitle><text>Secret Book Title</text></docTitle>
  <navMap><navPoint id="np1" playOrder="1"><navLabel><text>Ch1</text></navLabel><content src="chapter1.xhtml"/></navPoint></navMap>
</ncx>"#).unwrap();

    writer.start_file("OEBPS/chapter1.xhtml", options).unwrap();
    writer.write_all(b"<html><head/><body><p>text</p></body></html>").unwrap();

    // Junk that must be dropped
    writer.start_file("iTunesMetadata.plist", options).unwrap();
    writer.write_all(b"<plist/>").unwrap();

    writer
        .start_file("META-INF/calibre_bookmarks.txt", options)
        .unwrap();
    writer.write_all(b"secret=bookmark").unwrap();

    writer.finish().unwrap();
}

/// Make an encrypted-EPUB fixture (contains META-INF/encryption.xml).
/// These should be rejected by the cleaner.
pub fn make_encrypted_epub(path: &Path) {
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    let file = std::fs::File::create(path).unwrap();
    let mut writer = ZipWriter::new(file);
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let options = SimpleFileOptions::default();

    writer.start_file("mimetype", stored).unwrap();
    writer.write_all(b"application/epub+zip").unwrap();
    writer
        .start_file("META-INF/encryption.xml", options)
        .unwrap();
    writer.write_all(b"<encryption/>").unwrap();
    writer.finish().unwrap();
}

// ---------- ffmpeg-generated audio/video fixtures ----------

/// Generate a silent WAV file via ffmpeg, then inject an Artist tag
/// through lofty. Returns Ok(()) on success and bubbles failure up so
/// the test can assert it.
pub fn make_dirty_wav(path: &Path) -> std::io::Result<()> {
    ffmpeg_synthesize(path, &["-f", "lavfi", "-i", "anullsrc=cl=mono:r=8000", "-t", "0.1"])?;
    inject_audio_tag(path, "mat2-parity-artist")
}

pub fn make_dirty_mp3(path: &Path) -> std::io::Result<()> {
    ffmpeg_synthesize(
        path,
        &[
            "-f", "lavfi", "-i", "anullsrc=cl=mono:r=44100", "-t", "0.2", "-codec:a", "libmp3lame",
            "-b:a", "32k",
        ],
    )?;
    inject_audio_tag(path, "mat2-parity-artist")
}

pub fn make_dirty_flac(path: &Path) -> std::io::Result<()> {
    ffmpeg_synthesize(
        path,
        &[
            "-f", "lavfi", "-i", "anullsrc=cl=mono:r=44100", "-t", "0.1", "-codec:a", "flac",
        ],
    )?;
    inject_audio_tag(path, "mat2-parity-artist")
}

pub fn make_dirty_ogg(path: &Path) -> std::io::Result<()> {
    ffmpeg_synthesize(
        path,
        &[
            "-f", "lavfi", "-i", "anullsrc=cl=mono:r=44100", "-t", "0.1", "-codec:a",
            "libvorbis", "-b:a", "32k",
        ],
    )?;
    inject_audio_tag(path, "mat2-parity-artist")
}

pub fn make_dirty_aiff(path: &Path) -> std::io::Result<()> {
    ffmpeg_synthesize(
        path,
        &[
            "-f", "lavfi", "-i", "anullsrc=cl=mono:r=8000", "-t", "0.1", "-f", "aiff",
        ],
    )?;
    // Lofty's AIFF tag support is limited; for AIFF we accept the file
    // as-is and just verify that cleaning doesn't explode.
    Ok(())
}

/// Run ffprobe on `path` and return every (key, value) pair ffprobe
/// reports under a `.tags.` section, for both `format.tags.*` and
/// each `streams.stream.N.tags.*`. Used by the audio/video round-trip
/// tests as a ground-truth check that is independent of lofty /
/// our own readers: if ffprobe says a tag is gone, it really is gone.
///
/// A small allowlist of codec-structural tags (`language`,
/// `handler_name`, `vendor_id`) is filtered out because ffmpeg emits
/// these from stream codec context, not user metadata, and cleaning
/// them would require re-encoding.
pub fn ffprobe_user_tags(path: &Path) -> Vec<(String, String)> {
    let Ok(output) = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "flat",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    for line in stdout.lines() {
        let Some(idx) = line.find(".tags.") else {
            continue;
        };
        let after = &line[idx + ".tags.".len()..];
        let Some(eq) = after.find('=') else {
            continue;
        };
        let key = &after[..eq];
        // Structural / container-level tags ffmpeg always emits
        // regardless of our `-map_metadata -1` clean. These are codec
        // or ISO-BMFF brand fields, not user metadata.
        if matches!(
            key,
            "language"
                | "handler_name"
                | "vendor_id"
                | "major_brand"
                | "minor_version"
                | "compatible_brands"
        ) {
            continue;
        }
        let value = after[eq + 1..].trim_matches('"').to_string();
        out.push((key.to_string(), value));
    }
    out
}

/// Synthesize a silent M4A via ffmpeg and inject iTunes-style metadata
/// including a `location` tag that ends up in the udta/meta atom tree.
/// This is the round-trip fixture for the audio-handler M4A path,
/// specifically to prove that non-`ilst` atoms ffmpeg writes are still
/// scrubbed after the clean.
pub fn make_dirty_m4a(path: &Path) -> std::io::Result<()> {
    ffmpeg_synthesize(
        path,
        &[
            "-f",
            "lavfi",
            "-i",
            "anullsrc=cl=mono:r=44100",
            "-t",
            "0.1",
            "-c:a",
            "aac",
            "-metadata",
            "title=secret-m4a-title",
            "-metadata",
            "artist=secret-m4a-artist",
            "-metadata",
            "location=+40.7128-074.0060/",
        ],
    )
}

pub fn make_dirty_mp4(path: &Path) -> std::io::Result<()> {
    ffmpeg_synthesize(
        path,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=16x16:d=0.1:r=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-metadata",
            "title=secret-title",
            "-metadata",
            "comment=secret-comment",
            "-metadata",
            "artist=secret-artist",
        ],
    )
}

pub fn make_dirty_avi(path: &Path) -> std::io::Result<()> {
    ffmpeg_synthesize(
        path,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=16x16:d=0.1:r=1",
            "-c:v",
            "mpeg4",
            "-metadata",
            "title=secret-title",
        ],
    )
}

pub fn make_dirty_jxl(path: &Path) -> std::io::Result<()> {
    // Build a 1×1 JXL via ffmpeg if the binary has libjxl compiled in.
    // Then inject EXIF via little_exif which has a dedicated jxl path.
    ffmpeg_synthesize(
        path,
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
    )?;
    use little_exif::exif_tag::ExifTag;
    use little_exif::metadata::Metadata as ExifMetadata;
    let mut exif = ExifMetadata::new();
    exif.set_tag(ExifTag::Artist("mat2-parity-artist".to_string()));
    exif.write_to_file(path)
        .map_err(|e| std::io::Error::other(format!("little_exif jxl write: {e}")))?;
    Ok(())
}

pub fn make_dirty_tiff(path: &Path) -> std::io::Result<()> {
    // ffmpeg can output a 1×1 TIFF, then we inject EXIF via little_exif
    ffmpeg_synthesize(
        path,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=4x4:d=0.04:r=25",
            "-vframes",
            "1",
            "-f",
            "image2",
            "-c:v",
            "tiff",
        ],
    )?;
    use little_exif::exif_tag::ExifTag;
    use little_exif::metadata::Metadata as ExifMetadata;
    let mut exif = ExifMetadata::new();
    exif.set_tag(ExifTag::Artist("mat2-parity-artist".to_string()));
    exif.set_tag(ExifTag::Software("secret-camera".to_string()));
    exif.write_to_file(path)
        .map_err(|e| std::io::Error::other(format!("little_exif tiff write: {e}")))?;
    Ok(())
}

/// Build a minimum-viable BMP header. BMP has very little metadata
/// (and what exists is in the optional ICC profile V5 header), so we
/// just exercise the dispatch path.
pub fn make_bmp(path: &Path) {
    // 2x2 24-bit BMP. 14-byte file header + 40-byte info header +
    // padded pixel data (4 bytes per row because of DWORD alignment).
    let mut bmp = Vec::new();
    // BITMAPFILEHEADER
    bmp.extend_from_slice(b"BM");
    let file_size: u32 = 14 + 40 + 16;
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&[0, 0, 0, 0]); // reserved
    bmp.extend_from_slice(&54u32.to_le_bytes()); // pixel data offset
    // BITMAPINFOHEADER
    bmp.extend_from_slice(&40u32.to_le_bytes()); // header size
    bmp.extend_from_slice(&2i32.to_le_bytes()); // width
    bmp.extend_from_slice(&2i32.to_le_bytes()); // height
    bmp.extend_from_slice(&1u16.to_le_bytes()); // planes
    bmp.extend_from_slice(&24u16.to_le_bytes()); // bpp
    bmp.extend_from_slice(&0u32.to_le_bytes()); // compression BI_RGB
    bmp.extend_from_slice(&16u32.to_le_bytes()); // image size
    bmp.extend_from_slice(&0i32.to_le_bytes()); // x ppm
    bmp.extend_from_slice(&0i32.to_le_bytes()); // y ppm
    bmp.extend_from_slice(&0u32.to_le_bytes()); // colors used
    bmp.extend_from_slice(&0u32.to_le_bytes()); // colors important
    // Pixel data: 2 rows of (2 pixels × 3 bytes BGR) + 2 bytes padding
    bmp.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]); // row 0
    bmp.extend_from_slice(&[255, 0, 0, 0, 0, 255, 0, 0]); // row 1
    std::fs::write(path, &bmp).unwrap();
}

/// Build a FLAC file with an embedded JPEG cover that itself carries
/// EXIF metadata. Used to exercise the picture-recursion reader.
pub fn make_flac_with_dirty_cover(path: &Path, jpeg_bytes: &[u8]) -> std::io::Result<()> {
    make_dirty_flac(path)?;

    use lofty::config::WriteOptions;
    use lofty::ogg::{OggPictureStorage, VorbisComments};
    use lofty::picture::{MimeType, Picture, PictureType};
    use lofty::tag::TagExt;

    let mut vc = VorbisComments::default();
    vc.set_vendor(String::new());
    let picture = Picture::unchecked(jpeg_bytes.to_vec())
        .pic_type(PictureType::CoverFront)
        .mime_type(MimeType::Jpeg)
        .build();
    vc.insert_picture(picture, None)
        .map_err(|e| std::io::Error::other(format!("insert_picture: {e}")))?;
    vc.save_to_path(path, WriteOptions::default())
        .map_err(|e| std::io::Error::other(format!("flac save: {e}")))?;
    Ok(())
}

pub fn make_dirty_mkv(path: &Path) -> std::io::Result<()> {
    ffmpeg_synthesize(
        path,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=16x16:d=0.1:r=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-metadata",
            "title=secret-title",
        ],
    )
}

fn ffmpeg_synthesize(path: &Path, args: &[&str]) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    let output = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-hide_banner"])
        .args(args)
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "ffmpeg synthesis failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

fn inject_audio_tag(path: &Path, artist: &str) -> std::io::Result<()> {
    use lofty::config::WriteOptions;
    use lofty::file::TaggedFileExt;
    use lofty::prelude::*;
    use lofty::tag::{Tag, TagType};

    let mut tagged = lofty::read_from_path(path)
        .map_err(|e| std::io::Error::other(format!("lofty read: {e}")))?;

    // Use whatever tag type lofty reports as primary for this format.
    let tag_type = tagged.primary_tag_type();
    if tagged.tag(tag_type).is_none() {
        tagged.insert_tag(Tag::new(tag_type));
    }

    if let Some(tag) = tagged.tag_mut(tag_type) {
        tag.set_artist(artist.to_string());
        tag.set_title("secret-title".to_string());
        tag.set_comment("secret-comment".to_string());
    }
    // ID3v2 is mat2's test vector for MP3, so add it explicitly as well.
    if !tagged.tags().iter().any(|t| t.tag_type() == TagType::Id3v2) {
        let mut id3v2 = Tag::new(TagType::Id3v2);
        id3v2.set_artist(artist.to_string());
        tagged.insert_tag(id3v2);
    }
    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| std::io::Error::other(format!("lofty save: {e}")))?;
    Ok(())
}

// ---------- Image assertion helpers ----------

/// Assert that a cleaned JPEG has no EXIF metadata.
///
/// `little_exif::Metadata::new_from_path` can either return `Ok(empty)`
/// (JPEG with no EXIF block) *or* `Err` (JPEG without a recognisable
/// APP1/EXIF segment at all) for a successfully cleaned file. The old
/// `if let Ok(m) = ...` pattern treated `Err` as a silent pass, which
/// meant a cleaner that corrupted the JPEG would still make the test
/// green.
///
/// This helper handles both shapes safely:
/// - `Ok`: the iterator must be empty.
/// - `Err`: the file must still be a structurally valid JPEG as far as
///   `img_parts::Jpeg::from_bytes` is concerned, and it must not
///   contain any APP1 segment starting with `Exif\0\0`. Any other case
///   fails the test.
pub fn assert_no_exif_or_valid_jpeg(path: &Path, message: &str) {
    if let Ok(m) = little_exif::metadata::Metadata::new_from_path(path) {
        assert!(
            m.into_iter().next().is_none(),
            "{message}: little_exif still reports EXIF tags at {}",
            path.display()
        );
        return;
    }

    // little_exif reported an error; fall back to img_parts so we
    // verify the file is still a structurally valid JPEG and confirm
    // there is no raw EXIF APP1 segment left behind.
    let bytes = std::fs::read(path)
        .unwrap_or_else(|e| panic!("{message}: {} unreadable: {e}", path.display()));
    let jpeg = img_parts::jpeg::Jpeg::from_bytes(bytes.into()).unwrap_or_else(|e| {
        panic!(
            "{message}: {} is not a structurally valid JPEG: {e}",
            path.display()
        )
    });
    let has_exif_segment = jpeg
        .segments()
        .iter()
        .any(|segment| segment.marker() == 0xE1 && segment.contents().starts_with(b"Exif\0\0"));
    assert!(
        !has_exif_segment,
        "{message}: {} still contains an EXIF APP1 segment",
        path.display()
    );
}

// ---------- Zip assertion helpers ----------

/// Read every member of a cleaned zip-based archive and assert that
/// mat2's normalization invariants hold: last-modified = 1980-01-01,
/// comment empty, lexicographic order (with mimetype first if present).
pub fn assert_zip_is_normalized(path: &Path) {
    let file = std::fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).unwrap();

    // Collect every member name up-front so the mimetype invariant can
    // be checked once, instead of threading position bookkeeping
    // through the per-entry loop. The previous implementation had a
    // broken `assert_eq!(i, 1, "must be at index 0")` that only fired
    // when mimetype was at index 1, which was (a) physically incorrect
    // and (b) literally the opposite of the assertion message.
    let names: Vec<String> = (0..archive.len())
        .map(|i| archive.by_index_raw(i).unwrap().name().to_string())
        .collect();
    if let Some(mimetype_idx) = names.iter().position(|n| n == "mimetype") {
        assert_eq!(
            mimetype_idx, 0,
            "mimetype must be at index 0, found at {mimetype_idx}"
        );
    }

    let mut prev_name: Option<String> = None;
    for i in 0..archive.len() {
        let entry = archive.by_index_raw(i).unwrap();
        let name = entry.name().to_string();

        // Timestamp: must be the 1980-01-01 epoch
        let dt = entry.last_modified().unwrap_or_default();
        assert_eq!(
            (
                dt.year(),
                dt.month(),
                dt.day(),
                dt.hour(),
                dt.minute(),
                dt.second()
            ),
            (1980, 1, 1, 0, 0, 0),
            "zip entry {name} has non-epoch timestamp"
        );

        // Comment: must be empty
        assert!(
            entry.comment().is_empty(),
            "zip entry {name} has non-empty comment"
        );

        // Lexicographic ordering, with a single exception for mimetype
        // (which is always at index 0 per the check above). Every
        // other member must be >= its predecessor.
        if name != "mimetype"
            && let Some(prev) = &prev_name
            && prev != "mimetype"
        {
            assert!(
                &name >= prev,
                "zip entries not lexicographically sorted: {prev:?} then {name:?}"
            );
        }
        prev_name = Some(name);
    }
}

/// Extract every member of a zip archive and run it back through the
/// corresponding format handler; assert that each returns empty
/// metadata. This is mat2's TestZipMetadata.__check_deep_meta check.
pub fn assert_deep_meta_empty(path: &Path) {
    let file = std::fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).unwrap();
    let tmpdir = tempfile::tempdir().unwrap();

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        // Skip mimetype marker
        if name == "mimetype" {
            continue;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).unwrap();

        // Only check members we actually know how to parse. The core
        // library's dispatch decides which handler (if any) applies.
        let out_path: PathBuf = tmpdir.path().join(name.replace('/', "_"));
        std::fs::write(&out_path, &buf).unwrap();

        let mime = traceless_core::format_support::detect_mime(&out_path);
        if let Some(handler) = traceless_core::format_support::get_handler_for_mime(&mime)
            && let Ok(meta) = handler.read_metadata(&out_path)
        {
            assert!(
                meta.is_empty(),
                "embedded {name} inside {} still has metadata after clean: {meta:?}",
                path.display()
            );
        }
    }
}

/// Read an entry from a zip archive. Returns None if missing.
pub fn read_zip_entry(path: &Path, entry: &str) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).ok()?;
    let mut e = archive.by_name(entry).ok()?;
    let mut buf = Vec::new();
    e.read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// List all entry names in a zip.
pub fn zip_entry_names(path: &Path) -> Vec<String> {
    let file = std::fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).unwrap();
    (0..archive.len())
        .filter_map(|i| {
            archive
                .by_index_raw(i)
                .ok()
                .map(|e| e.name().to_string())
        })
        .collect()
}

// Count occurrences of `needle` across every *.xml entry in a zip.
pub fn count_needle_in_xml_entries(path: &Path, needle: &str) -> usize {
    let file = std::fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).unwrap();
    let mut total = 0usize;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        if !entry.name().ends_with(".xml") && !entry.name().ends_with(".rels") {
            continue;
        }
        let mut s = String::new();
        if entry.read_to_string(&mut s).is_ok() {
            total += s.matches(needle).count();
        }
    }
    total
}
