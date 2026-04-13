/// A single metadata key-value pair.
#[derive(Debug, Clone)]
pub struct MetadataItem {
    pub key: String,
    pub value: String,
}

/// Metadata for a single sub-file (e.g., a file within an archive).
#[derive(Debug, Clone)]
pub struct MetadataGroup {
    pub filename: String,
    pub items: Vec<MetadataItem>,
}

/// All metadata found in a file. May contain one or multiple groups
/// (e.g., DOCX has multiple XML files with metadata inside).
#[derive(Debug, Clone, Default)]
pub struct MetadataSet {
    pub groups: Vec<MetadataGroup>,
}

impl MetadataSet {
    #[must_use]
    pub fn total_count(&self) -> usize {
        self.groups.iter().map(|g| g.items.len()).sum()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.groups.iter().all(|g| g.items.is_empty())
    }
}
