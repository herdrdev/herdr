use serde::{Deserialize, Serialize};
use std::path::Path;

/// Reload the selected session's registry. Omission reuses its configured source.
/// This does not install integrations or grant report authority.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegistryReloadParams {
    /// Absolute local source directory (at most 4096 UTF-8 bytes), not a URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Explicit remote check/update. Never starts an automatic update schedule.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegistryUpdateParams {
    #[serde(default)]
    pub(crate) channel: crate::agents::remote::Channel,
}

impl RegistryReloadParams {
    pub(crate) fn source_path(&self) -> Result<Option<&Path>, String> {
        self.source
            .as_deref()
            .map(|source| {
                if source.is_empty() || source.len() > 4096 || source.contains('\0') {
                    return Err(
                        "registry source must be a nonempty local path of at most 4096 bytes"
                            .into(),
                    );
                }
                let path = Path::new(source);
                if !path.is_absolute() {
                    return Err("registry source must be an absolute local directory path".into());
                }
                Ok(path)
            })
            .transpose()
    }
}
