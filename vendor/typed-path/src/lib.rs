#![cfg_attr(feature = "std", doc = include_str!("../README.md"))]
#![cfg_attr(not(feature = "std"), no_std)]
// NOTE (traceless vendored patch): upstream 0.12.3 gates this on the
// nightly `feature(wasip2)` lang feature, which current stable rustc
// (1.96) rejects as an unknown feature on the stable channel ("E0635:
// unknown feature `wasip2`"). The `std::os::wasi` API it unlocked is now
// part of stable std for `wasm32-wasip2`, and the only uses of it in
// this crate live in `#[cfg(test)]` modules we never compile. Removing
// the attribute lets the crate build on `wasm32-wasip2` with no loss of
// non-test functionality. See traceless CLAUDE.md "research-and-migrate"
// dependency policy.

#[doc = include_str!("../README.md")]
#[cfg(all(doctest, feature = "std"))]
pub struct ReadmeDoctests;

extern crate alloc;

mod no_std_compat {
    #[allow(unused_imports)]
    pub use alloc::{
        boxed::Box,
        string::{String, ToString},
        vec,
        vec::Vec,
    };
}

#[macro_use]
mod common;
#[cfg(all(not(target_family = "wasm"), any(windows, unix)))]
mod native;
#[cfg(all(not(target_family = "wasm"), any(windows, unix)))]
mod platform;
mod typed;
mod unix;
#[cfg(all(feature = "std", not(target_family = "wasm")))]
pub mod utils;
mod windows;

mod private {
    /// Used to mark traits as sealed to prevent implements from others outside of this crate
    pub trait Sealed {}
}

pub use common::*;
#[cfg(all(not(target_family = "wasm"), any(windows, unix)))]
pub use native::*;
#[cfg(all(not(target_family = "wasm"), any(windows, unix)))]
pub use platform::*;
pub use typed::*;
pub use unix::*;
pub use windows::*;

/// Contains constants associated with different path formats.
pub mod constants {
    use super::unix::constants as unix_constants;
    use super::windows::constants as windows_constants;

    /// Contains constants associated with Unix paths.
    pub mod unix {
        pub use super::unix_constants::*;
    }

    /// Contains constants associated with Windows paths.
    pub mod windows {
        pub use super::windows_constants::*;
    }
}
