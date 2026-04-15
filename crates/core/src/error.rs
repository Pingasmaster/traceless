use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("Unsupported file format: {mime_type}")]
    UnsupportedFormat { mime_type: String },

    #[error("Failed to read file {}: {source}", path.display())]
    ReadError {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to parse metadata from {}: {detail}", path.display())]
    ParseError { path: PathBuf, detail: String },

    #[error("Failed to clean metadata from {}: {detail}", path.display())]
    CleanError { path: PathBuf, detail: String },

    #[error("File not found: {}", path.display())]
    NotFound { path: PathBuf },

    #[error("External tool not found: {tool}")]
    ToolNotFound { tool: String },

    #[error("External tool failed: {tool}: {detail}")]
    ToolFailed { tool: String, detail: String },

    #[error("File {} is {size} bytes, exceeds the {limit}-byte input cap", path.display())]
    FileTooLarge {
        path: PathBuf,
        size: u64,
        limit: u64,
    },
}
