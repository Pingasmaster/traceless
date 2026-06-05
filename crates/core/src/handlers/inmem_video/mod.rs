//! Pure-Rust, fully in-memory metadata strippers for the container
//! formats the native path handled via `ffmpeg`/`ffprobe` (which cannot
//! cross-compile to `wasm32-wasip2`: subprocess + filesystem).
//!
//! Each submodule exposes `pub(crate) fn strip(&[u8]) -> Result<Vec<u8>,
//! CoreError>` that mirrors `ffmpeg -map 0 -c copy -map_metadata -1
//! -map_chapters -1 -disposition 0 -fflags +bitexact`: it removes every
//! container/global/track tag, chapters, and attachments while keeping the
//! audio/video streams byte-for-byte (no transcode). Parse failures map to
//! [`crate::error::CoreError::ParseError`]; a malformed/hostile input never
//! panics, hangs, or allocates unboundedly.
//!
//! `redundant_pub_crate`: this whole tree is intentionally crate-internal
//! (`pub(crate) mod inmem_video`), and each stripper is `pub(crate) fn
//! strip` so the dispatch in `inmem.rs` can reach it; the explicit
//! `pub(crate)` documents that contract at every site, so we keep it.
#![allow(clippy::redundant_pub_crate)]
//!
//! Routed by [`crate::inmem`]:
//! - [`isobmff`] - MP4/QuickTime + the MP4-family audio (m4a/aac)
//! - [`mkv`]     - Matroska/WebM
//! - [`avi`]     - AVI (RIFF)
//! - [`flv`]     - FLV
//! - [`asf`]     - ASF/WMV
//! - [`ogg`]     - Ogg (Vorbis/Opus/Theora comment headers)

pub(crate) mod asf;
pub(crate) mod avi;
pub(crate) mod flv;
pub(crate) mod isobmff;
pub(crate) mod mkv;
pub(crate) mod ogg;
