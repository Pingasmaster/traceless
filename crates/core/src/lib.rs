#![forbid(unsafe_code)]
// Transitive dependencies (cpufeatures via lopdf's aes/sha2/chacha20, hashbrown via
// indexmap through cxx-qt/gtk4/zip/lopdf) pull multiple versions that we cannot
// align from this repo. See CLAUDE.md for the waiver rationale.
#![allow(clippy::multiple_crate_versions)]

pub mod config;
pub mod error;
pub mod file;
pub mod file_store;
pub mod format_support;
pub mod handlers;
pub mod metadata;
mod worker_pool;

pub use config::{
    LimitsGuard, PolicyGuard, UnknownMemberPolicy, archive_unknown_policy, limits_disabled,
    set_archive_unknown_policy, set_limits_disabled,
};
pub use error::CoreError;
pub use file::{FileEntry, FileId, FileState};
pub use file_store::{FileStore, FileStoreEvent, HANDLER_WALL_CLOCK_CAP, collect_paths};
pub use handlers::MAX_INPUT_FILE_BYTES;
pub use handlers::archive::{
    MAX_ARCHIVE_TOTAL_DECOMPRESSED_BYTES, MAX_ENTRY_DECOMPRESSED_BYTES, MAX_TAR_DECOMPRESSED_BYTES,
};
pub use metadata::{MetadataGroup, MetadataItem, MetadataSet};
