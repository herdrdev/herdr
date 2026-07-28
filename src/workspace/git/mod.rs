mod config;
#[cfg(test)]
mod config_tests;
mod discovery;
mod status;
#[cfg(test)]
pub(super) mod test_support;

pub(crate) use self::discovery::automatic_workspace_label;

pub use self::{
    discovery::{
        derive_label_from_cwd, fallback_label_from_cwd, git_branch, git_space_metadata,
        GitSpaceMetadata,
    },
    status::{
        git_status_cache_key, git_status_cache_key_for_space,
        git_status_snapshot_for_cwd_with_demand, GitStatusCacheEntry, GitStatusRefreshDemand,
    },
};

#[cfg(test)]
pub(super) use self::status::git_ahead_behind;
