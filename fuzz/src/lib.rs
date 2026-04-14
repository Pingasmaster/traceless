//! Shared helper for every fuzz target in this crate.
//!
//! Each `fuzz_targets/handler_*.rs` binary calls `fuzz_handler` with
//! its format's MIME and extension; the function writes `data` to a
//! tempfile, invokes both `read_metadata` and `clean_metadata`, and
//! discards the results. Any panic propagates up to the fuzz runner.

use std::path::PathBuf;
use traceless_core::format_support::get_handler_for_mime;

#[inline]
pub fn fuzz_handler(mime: &str, ext: &str, data: &[u8]) {
    let Some(handler) = get_handler_for_mime(mime) else {
        return;
    };
    let Ok(dir) = tempfile::tempdir() else {
        return;
    };
    let src: PathBuf = dir.path().join(format!("f.{ext}"));
    if std::fs::write(&src, data).is_err() {
        return;
    }
    let _ = handler.read_metadata(&src);
    let dst = dir.path().join(format!("o.{ext}"));
    let _ = handler.clean_metadata(&src, &dst);
}
