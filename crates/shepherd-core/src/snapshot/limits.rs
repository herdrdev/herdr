//! Snapshot limits (`[shepherd.snapshots]` in `config.toml`).

use crate::config::SnapshotsConfig;

/// Effective limits for one snapshot operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotLimits {
    /// Snapshots kept per agent; the oldest are pruned after every capture.
    pub max_per_agent: usize,
    /// Files larger than this are left out of a capture (and listed as omitted).
    pub max_file_bytes: u64,
    /// A capture whose included files add up to more than this fails.
    pub max_capture_bytes: u64,
}

impl Default for SnapshotLimits {
    fn default() -> Self {
        SnapshotsConfig::default().into()
    }
}

impl From<SnapshotsConfig> for SnapshotLimits {
    fn from(c: SnapshotsConfig) -> Self {
        Self {
            max_per_agent: c.max_per_agent,
            max_file_bytes: c.max_file_bytes,
            max_capture_bytes: c.max_capture_bytes,
        }
    }
}

/// Human-readable problems with a snapshots configuration (empty when valid).
pub fn validate(c: &SnapshotsConfig) -> Vec<String> {
    let mut issues = Vec::new();
    if c.max_per_agent == 0 {
        issues.push("snapshots.max_per_agent must be at least 1".into());
    }
    if c.max_file_bytes == 0 {
        issues.push("snapshots.max_file_bytes must be at least 1".into());
    }
    if c.max_capture_bytes < c.max_file_bytes {
        issues.push("snapshots.max_capture_bytes must be at least snapshots.max_file_bytes".into());
    }
    issues
}
