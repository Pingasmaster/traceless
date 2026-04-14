//! `.torrent` (bencode) metadata cleaner.
//!
//! A .torrent file is a bencoded dictionary. The only keys required by
//! the BitTorrent spec are `announce`, `announce-list`, and `info`;
//! everything else (`created by`, `creation date`, `comment`, `encoding`,
//! `publisher`, `publisher-url`, `private`, …) is optional metadata
//! that can identify who built the torrent and when.
//!
//! We port mat2's allowlist approach: keep `announce` / `announce-list`
//! / `info`, discard everything else. The bencode parser is a direct
//! Rust port of `libmat2/torrent.py::_BencodeHandler` — self-contained,
//! no external crates.
//!
//! Reference spec: <https://wiki.theory.org/BitTorrentSpecification#Bencoding>

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::error::CoreError;
use crate::metadata::{MetadataGroup, MetadataItem, MetadataSet};

use super::FormatHandler;

pub struct TorrentHandler;

const ALLOWLIST: &[&[u8]] = &[b"announce", b"announce-list", b"info"];

impl FormatHandler for TorrentHandler {
    fn read_metadata(&self, path: &Path) -> Result<MetadataSet, CoreError> {
        let bytes = fs::read(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        let (value, rest) = decode(&bytes).map_err(|e| CoreError::ParseError {
            path: path.to_path_buf(),
            detail: format!("bencode: {e}"),
        })?;
        if !rest.is_empty() {
            return Err(CoreError::ParseError {
                path: path.to_path_buf(),
                detail: "trailing data after bencoded dictionary".to_string(),
            });
        }
        let BencodeValue::Dict(dict) = value else {
            return Err(CoreError::ParseError {
                path: path.to_path_buf(),
                detail: "torrent file must decode to a dictionary".to_string(),
            });
        };

        let mut items = Vec::new();
        for (key, value) in &dict {
            if ALLOWLIST.contains(&key.as_slice()) {
                continue;
            }
            let key_str = String::from_utf8_lossy(key).to_string();
            items.push(MetadataItem {
                key: key_str,
                value: value.display(),
            });
        }

        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut set = MetadataSet::default();
        if !items.is_empty() {
            set.groups.push(MetadataGroup { filename, items });
        }
        Ok(set)
    }

    fn clean_metadata(&self, path: &Path, output_path: &Path) -> Result<(), CoreError> {
        let bytes = fs::read(path).map_err(|e| CoreError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        let (value, rest) = decode(&bytes).map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("bencode: {e}"),
        })?;
        if !rest.is_empty() {
            return Err(CoreError::CleanError {
                path: path.to_path_buf(),
                detail: "trailing data after bencoded dictionary".to_string(),
            });
        }
        let BencodeValue::Dict(dict) = value else {
            return Err(CoreError::CleanError {
                path: path.to_path_buf(),
                detail: "torrent file must decode to a dictionary".to_string(),
            });
        };

        let mut cleaned: BTreeMap<Vec<u8>, BencodeValue> = BTreeMap::new();
        for (k, v) in dict {
            if ALLOWLIST.contains(&k.as_slice()) {
                cleaned.insert(k, v);
            }
        }

        let encoded = encode(&BencodeValue::Dict(cleaned));
        fs::write(output_path, encoded).map_err(|e| CoreError::CleanError {
            path: path.to_path_buf(),
            detail: format!("Failed to write cleaned torrent: {e}"),
        })?;
        Ok(())
    }

    fn supported_mime_types(&self) -> &[&str] {
        &["application/x-bittorrent"]
    }
}

// ============================================================
// Bencode value type + parser + encoder
// ============================================================

#[derive(Debug, Clone)]
pub enum BencodeValue {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<Self>),
    Dict(BTreeMap<Vec<u8>, Self>),
}

impl BencodeValue {
    fn display(&self) -> String {
        match self {
            Self::Int(n) => n.to_string(),
            Self::Bytes(b) => match std::str::from_utf8(b) {
                Ok(s) => s.to_string(),
                Err(_) => format!("<{} bytes of binary data>", b.len()),
            },
            Self::List(items) => format!("[{} items]", items.len()),
            Self::Dict(map) => format!("{{{} keys}}", map.len()),
        }
    }
}

#[derive(Debug)]
pub enum BencodeError {
    Eof,
    BadPrefix,
    InvalidInt,
    InvalidString,
    InvalidDictKey,
    TrailingGarbage,
    DepthExceeded,
}

impl std::fmt::Display for BencodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Eof => write!(f, "unexpected EOF"),
            Self::BadPrefix => write!(f, "bad bencode prefix byte"),
            Self::InvalidInt => write!(f, "invalid integer"),
            Self::InvalidString => write!(f, "invalid string"),
            Self::InvalidDictKey => write!(f, "invalid dictionary key"),
            Self::TrailingGarbage => write!(f, "trailing garbage"),
            Self::DepthExceeded => write!(f, "nesting depth exceeded"),
        }
    }
}

/// Maximum allowed nesting depth for bencode lists/dicts. An adversarial
/// `llll...` or `dddd...` input with thousands of levels would otherwise
/// recurse deep enough to blow the worker thread's stack; 256 is well
/// above anything a legal torrent file needs and well below the point
/// where a default 2 MiB thread stack runs out.
const MAX_DEPTH: usize = 256;

/// Decode a single bencode value from the start of `input`. Returns
/// `(value, remainder)` — if `remainder` isn't empty the caller can
/// decide whether to reject.
pub fn decode(input: &[u8]) -> Result<(BencodeValue, &[u8]), BencodeError> {
    decode_at(input, 0)
}

fn decode_at(input: &[u8], depth: usize) -> Result<(BencodeValue, &[u8]), BencodeError> {
    if input.is_empty() {
        return Err(BencodeError::Eof);
    }
    match input[0] {
        b'i' => decode_int(input),
        b'l' => decode_list(input, depth),
        b'd' => decode_dict(input, depth),
        b'0'..=b'9' => decode_bytes(input),
        _ => Err(BencodeError::BadPrefix),
    }
}

fn decode_int(input: &[u8]) -> Result<(BencodeValue, &[u8]), BencodeError> {
    // Format: `i<integer>e`. No leading zeros, no `-0`.
    debug_assert_eq!(input[0], b'i');
    let end = input
        .iter()
        .position(|&b| b == b'e')
        .ok_or(BencodeError::Eof)?;
    let body = &input[1..end];
    if body.is_empty() {
        return Err(BencodeError::InvalidInt);
    }
    // Reject "-0" and leading zeros (except the literal "0").
    if body == b"-0" {
        return Err(BencodeError::InvalidInt);
    }
    if body.len() > 1 && body[0] == b'0' {
        return Err(BencodeError::InvalidInt);
    }
    if body.len() > 2 && body[0] == b'-' && body[1] == b'0' {
        return Err(BencodeError::InvalidInt);
    }
    let s = std::str::from_utf8(body).map_err(|_| BencodeError::InvalidInt)?;
    let n = s.parse::<i64>().map_err(|_| BencodeError::InvalidInt)?;
    Ok((BencodeValue::Int(n), &input[end + 1..]))
}

fn decode_bytes(input: &[u8]) -> Result<(BencodeValue, &[u8]), BencodeError> {
    // Format: `<len>:<bytes>`. Length must not have leading zeros
    // (except "0:").
    let colon = input
        .iter()
        .position(|&b| b == b':')
        .ok_or(BencodeError::InvalidString)?;
    let len_bytes = &input[..colon];
    if len_bytes.is_empty() {
        return Err(BencodeError::InvalidString);
    }
    if len_bytes.len() > 1 && len_bytes[0] == b'0' {
        return Err(BencodeError::InvalidString);
    }
    let len_str = std::str::from_utf8(len_bytes).map_err(|_| BencodeError::InvalidString)?;
    let len: usize = len_str.parse().map_err(|_| BencodeError::InvalidString)?;
    let start = colon + 1;
    let end = start.checked_add(len).ok_or(BencodeError::Eof)?;
    if end > input.len() {
        return Err(BencodeError::Eof);
    }
    let bytes = input[start..end].to_vec();
    Ok((BencodeValue::Bytes(bytes), &input[end..]))
}

fn decode_list(input: &[u8], depth: usize) -> Result<(BencodeValue, &[u8]), BencodeError> {
    if depth >= MAX_DEPTH {
        return Err(BencodeError::DepthExceeded);
    }
    debug_assert_eq!(input[0], b'l');
    let mut rest = &input[1..];
    let mut out = Vec::new();
    while !rest.is_empty() && rest[0] != b'e' {
        let (v, r) = decode_at(rest, depth + 1)?;
        out.push(v);
        rest = r;
    }
    if rest.is_empty() {
        return Err(BencodeError::Eof);
    }
    Ok((BencodeValue::List(out), &rest[1..]))
}

fn decode_dict(input: &[u8], depth: usize) -> Result<(BencodeValue, &[u8]), BencodeError> {
    if depth >= MAX_DEPTH {
        return Err(BencodeError::DepthExceeded);
    }
    debug_assert_eq!(input[0], b'd');
    let mut rest = &input[1..];
    let mut map: BTreeMap<Vec<u8>, BencodeValue> = BTreeMap::new();
    while !rest.is_empty() && rest[0] != b'e' {
        let (k_val, r1) = decode_bytes(rest)?;
        let BencodeValue::Bytes(key) = k_val else {
            return Err(BencodeError::InvalidDictKey);
        };
        let (v, r2) = decode_at(r1, depth + 1)?;
        map.insert(key, v);
        rest = r2;
    }
    if rest.is_empty() {
        return Err(BencodeError::Eof);
    }
    Ok((BencodeValue::Dict(map), &rest[1..]))
}

pub fn encode(value: &BencodeValue) -> Vec<u8> {
    let mut out = Vec::new();
    encode_into(value, &mut out);
    out
}

fn encode_into(value: &BencodeValue, out: &mut Vec<u8>) {
    match value {
        BencodeValue::Int(n) => {
            out.push(b'i');
            out.extend_from_slice(n.to_string().as_bytes());
            out.push(b'e');
        }
        BencodeValue::Bytes(b) => {
            out.extend_from_slice(b.len().to_string().as_bytes());
            out.push(b':');
            out.extend_from_slice(b);
        }
        BencodeValue::List(items) => {
            out.push(b'l');
            for item in items {
                encode_into(item, out);
            }
            out.push(b'e');
        }
        BencodeValue::Dict(map) => {
            out.push(b'd');
            // BTreeMap iterates in lexicographic order, which is the
            // bencode canonical ordering.
            for (k, v) in map {
                encode_into(&BencodeValue::Bytes(k.clone()), out);
                encode_into(v, out);
            }
            out.push(b'e');
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_dirty_torrent(path: &Path) {
        // d8:announce8:test.com7:comment6:secret10:created by13:mktorrent 1.04:infod4:name4:test12:piece lengthi16384eee
        let mut torrent = BTreeMap::new();
        torrent.insert(
            b"announce".to_vec(),
            BencodeValue::Bytes(b"http://test.com/announce".to_vec()),
        );
        torrent.insert(
            b"comment".to_vec(),
            BencodeValue::Bytes(b"secret-comment".to_vec()),
        );
        torrent.insert(
            b"created by".to_vec(),
            BencodeValue::Bytes(b"mktorrent 1.0".to_vec()),
        );
        torrent.insert(b"creation date".to_vec(), BencodeValue::Int(1_522_397_702));
        torrent.insert(b"encoding".to_vec(), BencodeValue::Bytes(b"UTF-8".to_vec()));

        let mut info = BTreeMap::new();
        info.insert(b"name".to_vec(), BencodeValue::Bytes(b"test-file".to_vec()));
        info.insert(b"piece length".to_vec(), BencodeValue::Int(16384));
        info.insert(b"pieces".to_vec(), BencodeValue::Bytes(vec![0u8; 20]));
        info.insert(b"length".to_vec(), BencodeValue::Int(100));
        torrent.insert(b"info".to_vec(), BencodeValue::Dict(info));

        fs::write(path, encode(&BencodeValue::Dict(torrent))).unwrap();
    }

    #[test]
    fn torrent_read_surfaces_leaks() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("dirty.torrent");
        make_dirty_torrent(&src);
        let h = TorrentHandler;
        let meta = h.read_metadata(&src).unwrap();
        let dump = format!("{meta:?}");
        assert!(dump.contains("secret-comment"));
        assert!(dump.contains("mktorrent"));
        assert!(dump.contains("creation date"));
        // allowlisted fields must NOT show up
        assert!(!dump.contains("announce"));
    }

    #[test]
    fn torrent_clean_drops_non_allowlisted_keys() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("dirty.torrent");
        let dst = dir.path().join("clean.torrent");
        make_dirty_torrent(&src);
        let h = TorrentHandler;
        h.clean_metadata(&src, &dst).unwrap();
        let out = fs::read(&dst).unwrap();

        assert!(find_bytes(&out, b"secret-comment").is_none());
        assert!(find_bytes(&out, b"mktorrent").is_none());
        assert!(find_bytes(&out, b"creation date").is_none());
        assert!(find_bytes(&out, b"encoding").is_none());

        // Allowlisted survives
        assert!(find_bytes(&out, b"announce").is_some());
        assert!(find_bytes(&out, b"4:info").is_some());
        assert!(find_bytes(&out, b"test-file").is_some());
    }

    #[test]
    fn torrent_rejects_malformed_bencode() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("bad.torrent");
        fs::write(&src, b"not bencode").unwrap();
        let h = TorrentHandler;
        assert!(h.read_metadata(&src).is_err());
    }

    #[test]
    fn torrent_rejects_trailing_garbage() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("bad.torrent");
        fs::write(&src, b"de trailing").unwrap();
        let h = TorrentHandler;
        assert!(h.read_metadata(&src).is_err());
    }

    #[test]
    fn bencode_rejects_negative_zero() {
        assert!(decode(b"i-0e").is_err());
    }

    #[test]
    fn bencode_rejects_leading_zero_int() {
        assert!(decode(b"i01e").is_err());
    }

    #[test]
    fn bencode_rejects_leading_zero_string_length() {
        assert!(decode(b"01:a").is_err());
    }

    #[test]
    fn bencode_rejects_length_overflow() {
        // Crafted string length near usize::MAX must not panic when the
        // subsequent slice arithmetic wraps; it must return an error.
        let max = usize::MAX.to_string();
        let mut input = max.into_bytes();
        input.extend_from_slice(b":x");
        assert!(decode(&input).is_err());
    }

    #[test]
    fn bencode_rejects_deeply_nested_lists() {
        // Regression: a bencoded `l`*N + `i0e` + `e`*N recurses once
        // per nesting level, which used to blow the worker thread's
        // stack for large N. The decoder now rejects anything beyond
        // MAX_DEPTH with BencodeError::DepthExceeded.
        let n = MAX_DEPTH + 50;
        let mut input = vec![b'l'; n];
        input.extend_from_slice(b"i0e");
        input.extend(std::iter::repeat_n(b'e', n));
        match decode(&input) {
            Err(BencodeError::DepthExceeded) => {}
            other => panic!("expected DepthExceeded, got {other:?}"),
        }
    }

    #[test]
    fn bencode_rejects_deeply_nested_dicts() {
        // Same regression via `d1:xd1:x...d1:x0:e...ee` - dict values
        // are bencoded recursively through the same dispatch.
        let n = MAX_DEPTH + 50;
        let mut input = Vec::new();
        for _ in 0..n {
            input.extend_from_slice(b"d1:x");
        }
        input.extend_from_slice(b"0:");
        input.extend(std::iter::repeat_n(b'e', n));
        match decode(&input) {
            Err(BencodeError::DepthExceeded) => {}
            other => panic!("expected DepthExceeded, got {other:?}"),
        }
    }

    #[test]
    fn bencode_accepts_just_under_depth_limit() {
        // Sanity: a nesting depth of MAX_DEPTH - 1 must still decode.
        // Pinning this guarantees we don't silently tighten the cap
        // and break legitimate inputs.
        let n = MAX_DEPTH - 1;
        let mut input = vec![b'l'; n];
        input.extend_from_slice(b"i0e");
        input.extend(std::iter::repeat_n(b'e', n));
        decode(&input).expect("nesting just under MAX_DEPTH must decode");
    }

    #[test]
    fn bencode_round_trip_dict() {
        let mut m = BTreeMap::new();
        m.insert(b"a".to_vec(), BencodeValue::Int(1));
        m.insert(b"b".to_vec(), BencodeValue::Bytes(b"hello".to_vec()));
        let v = BencodeValue::Dict(m);
        let encoded = encode(&v);
        let (decoded, rest) = decode(&encoded).unwrap();
        assert!(rest.is_empty());
        let BencodeValue::Dict(d) = decoded else {
            panic!("expected dict");
        };
        assert_eq!(d.len(), 2);
    }

    fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|w| w == needle)
    }
}
