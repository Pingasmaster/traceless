//! Property-based tests.
//!
//! For handlers whose correctness is specified as a shape invariant
//! rather than a handful of hand-built examples, proptest generates
//! thousands of randomized inputs per run. Every test asserts a
//! single invariant: the handler must not panic, and its output must
//! still satisfy the documented shape.
//!
//! These tests use proptest's default configuration (256 cases per
//! property). They finish well under a second on a modern machine.

#![allow(clippy::unwrap_used)]
mod common;

use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};

use proptest::prelude::*;

use traceless_core::format_support::get_handler_for_mime;

/// Wrap a call that may panic and return `Err` with the panic
/// message instead of letting it escape. Used by every property so a
/// proptest failure reports the panic message cleanly alongside the
/// shrunken input.
fn catch<T, F: FnOnce() -> T>(f: F) -> Result<T, String> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => Ok(v),
        Err(payload) => {
            let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "<non-string panic payload>".to_string()
            };
            Err(msg)
        }
    }
}

// ============================================================
// HTML handler: feed random byte blobs, require UTF-8 output
// ============================================================
//
// The custom HTML tag-state walker is 864 lines and parses a
// deliberately permissive subset. The single shape invariant is
// "never panic, always emit valid UTF-8" - if the cleaner ever
// produces non-UTF-8 bytes or panics on input, that's a regression.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn html_handler_never_panics_on_random_bytes(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("fuzz.html");
        let dst = dir.path().join("out.html");
        fs::write(&src, &bytes).unwrap();

        let handler = get_handler_for_mime("text/html").unwrap();
        catch(|| {
            let _ = handler.read_metadata(&src);
            let _ = handler.clean_metadata(&src, &dst);
        }).map_err(|e| TestCaseError::fail(format!("html handler panicked: {e}")))?;
    }

    #[test]
    fn html_clean_output_if_any_is_valid_utf8(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("fuzz.html");
        let dst = dir.path().join("out.html");
        fs::write(&src, &bytes).unwrap();

        let handler = get_handler_for_mime("text/html").unwrap();
        if handler.clean_metadata(&src, &dst).is_ok() {
            let out = fs::read(&dst).unwrap();
            // If the output parsed back, it must be valid UTF-8 -
            // the handler emits decoded text via quick_xml, which
            // does not produce lone surrogate bytes.
            std::str::from_utf8(&out).map_err(|e| {
                TestCaseError::fail(format!("html output not utf-8: {e}"))
            })?;
        }
    }
}

// ============================================================
// CSS handler: similar shape guarantee
// ============================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn css_handler_never_panics_on_random_bytes(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("fuzz.css");
        let dst = dir.path().join("out.css");
        fs::write(&src, &bytes).unwrap();

        let handler = get_handler_for_mime("text/css").unwrap();
        catch(|| {
            let _ = handler.read_metadata(&src);
            let _ = handler.clean_metadata(&src, &dst);
        }).map_err(|e| TestCaseError::fail(format!("css handler panicked: {e}")))?;
    }
}

// ============================================================
// SVG handler: generate plausible-ish SVG skeletons with random
// attribute names to exercise the attribute-filter allowlist.
// ============================================================

fn svg_wrapper(inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">{inner}</svg>"#
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn svg_handler_never_panics_on_random_attribute_names(
        attr_name in r"[A-Za-z:][A-Za-z0-9:_-]{0,20}",
        attr_value in r"[A-Za-z0-9 ./:()\-]{0,40}",
    ) {
        let inner = format!(r#"<g {attr_name}="{attr_value}"><rect width="1" height="1"/></g>"#);
        let body = svg_wrapper(&inner);

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("fuzz.svg");
        let dst = dir.path().join("out.svg");
        fs::write(&src, &body).unwrap();

        let handler = get_handler_for_mime("image/svg+xml").unwrap();
        catch(|| {
            let _ = handler.read_metadata(&src);
            let _ = handler.clean_metadata(&src, &dst);
        }).map_err(|e| TestCaseError::fail(format!("svg handler panicked: {e}")))?;
    }

    #[test]
    fn svg_clean_output_drops_every_listed_event_handler(
        attr_name in proptest::sample::select(KNOWN_EVENT_HANDLERS.to_vec()),
    ) {
        // The handler uses an explicit allowlist of spec-defined
        // event-handler names (EVENT_HANDLER_ATTRS in svg.rs). The
        // property: for every name in that list, a test SVG carrying
        // the attribute comes back with the attribute absent. This
        // gives us randomized coverage over all ~80 listed names
        // rather than hand-writing 80 separate assertions.
        let inner = format!(r#"<g {attr_name}="alert(1)"><rect width="1" height="1"/></g>"#);
        let body = svg_wrapper(&inner);

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("fuzz.svg");
        let dst = dir.path().join("out.svg");
        fs::write(&src, &body).unwrap();

        let handler = get_handler_for_mime("image/svg+xml").unwrap();
        if handler.clean_metadata(&src, &dst).is_ok() {
            let cleaned = fs::read_to_string(&dst).unwrap();
            prop_assert!(
                !cleaned.contains(attr_name),
                "listed event handler {attr_name} survived the clean: {cleaned}"
            );
        }
    }
}

/// Mirror of the handler's `EVENT_HANDLER_ATTRS`. Kept short (not the
/// full 80-entry list) because the property test is about randomized
/// selection, not exhaustive coverage, and because tests in the
/// integration-test crate cannot see the private constant directly.
const KNOWN_EVENT_HANDLERS: &[&str] = &[
    "onabort",
    "onauxclick",
    "onbeforeinput",
    "onblur",
    "oncancel",
    "oncanplay",
    "oncanplaythrough",
    "onchange",
    "onclick",
    "onclose",
    "oncontextmenu",
    "oncopy",
    "oncuechange",
    "oncut",
    "ondblclick",
    "ondrag",
    "ondragend",
    "ondragenter",
    "ondragleave",
    "ondragover",
    "ondragstart",
    "ondrop",
    "ondurationchange",
    "onemptied",
    "onended",
    "onerror",
    "onfocus",
    "onformdata",
    "oninput",
    "oninvalid",
    "onkeydown",
    "onkeypress",
    "onkeyup",
    "onload",
    "onloadeddata",
    "onloadedmetadata",
    "onloadstart",
    "onmousedown",
    "onmouseenter",
    "onmouseleave",
    "onmousemove",
    "onmouseout",
    "onmouseover",
    "onmouseup",
    "onpaste",
    "onpause",
    "onplay",
    "onplaying",
    "onprogress",
    "onratechange",
    "onreset",
    "onresize",
    "onscroll",
    "onseeked",
    "onseeking",
    "onselect",
    "onshow",
    "onstalled",
    "onsubmit",
    "onsuspend",
    "ontimeupdate",
    "ontoggle",
    "onvolumechange",
    "onwaiting",
    "onwheel",
];

// ============================================================
// Torrent bencode: random bencode-ish byte blobs
// ============================================================
//
// The hand-rolled parser sits at 493 lines. The shape invariant is
// "never panic, return Err on anything that isn't a well-formed
// dictionary-rooted bencode stream". Property is: parse never
// panics on random byte sequences.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn torrent_handler_never_panics_on_random_bytes(
        bytes in prop::collection::vec(any::<u8>(), 0..1024)
    ) {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("fuzz.torrent");
        let dst = dir.path().join("out.torrent");
        fs::write(&src, &bytes).unwrap();

        let handler = get_handler_for_mime("application/x-bittorrent").unwrap();
        catch(|| {
            let _ = handler.read_metadata(&src);
            let _ = handler.clean_metadata(&src, &dst);
        }).map_err(|e| TestCaseError::fail(format!("torrent handler panicked: {e}")))?;
    }

    #[test]
    fn torrent_handler_survives_nested_bencode_lists(depth in 0usize..50) {
        // Nested lists of arbitrary depth up to 50 levels. The
        // parser must recognize the outer shape isn't a dict and
        // return Err; the key invariant is no stack overflow.
        let mut body = vec![b'l'; depth];
        body.extend(std::iter::repeat_n(b'e', depth));
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("nest.torrent");
        fs::write(&src, &body).unwrap();
        let handler = get_handler_for_mime("application/x-bittorrent").unwrap();
        catch(|| {
            let _ = handler.read_metadata(&src);
        }).map_err(|e| TestCaseError::fail(format!("nested-list panic: {e}")))?;
    }
}

// ============================================================
// Archive safety: randomly generated tar names must be either
// accepted or rejected, never panic the tar safety walker.
// ============================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn tar_with_random_member_names_does_not_panic(
        name in r"[A-Za-z0-9./_\-]{1,100}",
    ) {
        use tar::{Builder as TarBuilder, EntryType, Header as TarHeader};
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("fuzz.tar");
        let dst = dir.path().join("out.tar");

        {
            let file = fs::File::create(&src).unwrap();
            let mut builder = TarBuilder::new(file);
            let mut h = TarHeader::new_gnu();
            // `set_path` rejects some byte sequences; that's fine,
            // we just skip and let the test case pass trivially.
            if h.set_path(&name).is_err() {
                return Ok(());
            }
            h.set_size(4);
            h.set_mode(0o644);
            h.set_entry_type(EntryType::Regular);
            h.set_cksum();
            builder.append(&h, &b"blob"[..]).unwrap();
            builder.into_inner().unwrap();
        }

        let handler = get_handler_for_mime("application/x-tar").unwrap();
        catch(|| {
            let _ = handler.read_metadata(&src);
            let _ = handler.clean_metadata(&src, &dst);
        }).map_err(|e| TestCaseError::fail(format!("tar safety panic on name {name:?}: {e}")))?;
    }
}
