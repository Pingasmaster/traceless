#![forbid(unsafe_code)]
// Transitive dependencies (cpufeatures via lopdf's aes/sha2/chacha20, hashbrown via
// indexmap through cxx-qt/gtk4/zip/lopdf) pull multiple versions that we cannot
// align from this repo. See CLAUDE.md for the waiver rationale.
#![allow(clippy::multiple_crate_versions)]

pub mod error;
pub mod file;
pub mod file_store;
pub mod format_support;
pub mod handlers;
pub mod metadata;

pub use error::CoreError;
pub use file::{FileEntry, FileId, FileState};
pub use file_store::{collect_paths, FileStore, FileStoreEvent};
pub use metadata::{MetadataGroup, MetadataItem, MetadataSet};
