//! Presentation metadata loaded from an agent package.

pub(crate) use super::source::{
    SoundDefault as SoundDefaultPolicy, SoundDefinition as SoundProfile,
};

impl SoundProfile {
    pub(crate) fn config_key(&self) -> &str {
        &self.key
    }

    pub(crate) fn default_policy(&self) -> SoundDefaultPolicy {
        self.default
    }
}
