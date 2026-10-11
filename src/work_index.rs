use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::schema::AgentStatus;
use crate::config::{MissiveConfig, WorkIndexConfig};
use crate::work_context::{
    linear_ticket_url, normalize_repo_slug, normalize_ticket_id, repo_slug_from_pr_url,
    repo_slugs_match, PaneWorkContext, PaneWorkRole,
};

mod cache;

use cache::{CachePolicy, Provider, ProviderCache, ProviderOutput};

pub(crate) const WORK_INDEX_BATCH_TIMEOUT: Duration = Duration::from_secs(90);
pub(crate) const WORK_INDEX_TARGET_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const WORK_ITEM_DETAIL_CACHE_CAPACITY: usize = 16;
const LINEAR_LIST_CACHE_TTL: Duration = Duration::from_secs(10 * 60);
const LINEAR_FULL_LIST_INTERVAL: Duration = Duration::from_secs(60 * 60);
const LINEAR_CYCLE_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const VIEWER_IDENTITY_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const GITHUB_LIST_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const GITHUB_PR_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const WORK_ITEM_DETAIL_PROVIDER_CACHE_TTL: Duration = Duration::from_secs(10 * 60);
const MISSIVE_LIST_CACHE_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WorkIndexCacheBypass {
    pub(crate) github: bool,
    pub(crate) linear: bool,
    pub(crate) missive: bool,
}
pub(crate) const PR_DETAIL_FOCUS_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkIndexPane {
    pub(crate) work_context: PaneWorkContext,
    pub(crate) name: Option<String>,
    pub(crate) agent: Option<String>,
    pub(crate) display_agent: Option<String>,
    pub(crate) agent_status: AgentStatus,
    pub(crate) workspace_id: String,
    pub(crate) tab_id: String,
    pub(crate) pane_id: String,
}

pub(crate) type WorkIndexContextFingerprint = Vec<(String, String, String, PaneWorkContext)>;

impl WorkIndexPane {
    pub(crate) fn has_indexable_context(&self) -> bool {
        self.work_context.repo.is_some()
            || !self.work_context.pr_urls.is_empty()
            || !self.work_context.ticket_ids.is_empty()
            || !self.work_context.missive_urls.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkIndexRefreshInFlight {
    pub(crate) generation: u64,
    pub(crate) deadline: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkItemDetailRefreshInFlight {
    pub(crate) keys: Vec<crate::app::state::WorkItemKey>,
    pub(crate) generation: u64,
    pub(crate) deadline: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkItemCheckSummary {
    pub(crate) failing: usize,
    pub(crate) total: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkItemComment {
    pub(crate) author: Option<String>,
    pub(crate) body: String,
    pub(crate) created_at: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkItemAction {
    pub(crate) name: String,
    pub(crate) state: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkItemFile {
    pub(crate) path: String,
    pub(crate) additions: u64,
    pub(crate) deletions: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkItemCommit {
    pub(crate) short_id: String,
    pub(crate) subject: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkItemTimelineEvent {
    pub(crate) kind: String,
    pub(crate) actor: Option<String>,
    pub(crate) summary: String,
    pub(crate) created_at: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReviewThreadComment {
    pub(crate) path: String,
    pub(crate) line: Option<u64>,
    pub(crate) body: String,
    pub(crate) author: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnresolvedReviewThreads {
    pub(crate) count: usize,
    pub(crate) comments: Vec<ReviewThreadComment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkItemDetail {
    pub(crate) number: Option<u64>,
    pub(crate) title: Option<String>,
    pub(crate) body: Option<String>,
    pub(crate) author: Option<String>,
    pub(crate) base_ref_name: Option<String>,
    pub(crate) head_ref_name: Option<String>,
    pub(crate) created_at: Option<SystemTime>,
    pub(crate) updated_at: Option<SystemTime>,
    pub(crate) labels: Vec<String>,
    pub(crate) url: Option<String>,
    pub(crate) review_decision: Option<String>,
    pub(crate) is_draft: Option<bool>,
    pub(crate) reviewers: Vec<String>,
    pub(crate) mergeable: Option<String>,
    pub(crate) merge_state_status: Option<String>,
    #[serde(default)]
    pub(crate) auto_merge_enabled: bool,
    pub(crate) head_sha: Option<String>,
    pub(crate) checks: Option<WorkItemCheckSummary>,
    pub(crate) comments: Vec<WorkItemComment>,
    pub(crate) actions: Vec<WorkItemAction>,
    pub(crate) files: Vec<WorkItemFile>,
    pub(crate) commits: Vec<WorkItemCommit>,
    #[serde(default)]
    pub(crate) timeline: Vec<WorkItemTimelineEvent>,
    #[serde(default)]
    pub(crate) timeline_unavailable: Option<String>,
    #[serde(default)]
    pub(crate) collaborators: Vec<String>,
    #[serde(default)]
    pub(crate) collaborators_unavailable: Option<String>,
    /// GitHub's `gh pr view --json` payload does not expose review threads.
    /// Keep the absence explicit rather than substituting review count data.
    pub(crate) unresolved_review_threads: Option<usize>,
    pub(crate) unavailable: Option<String>,
    pub(crate) observed_at: SystemTime,
}

impl WorkItemDetail {
    /// An otherwise blank detail, for a source that fills only some fields.
    pub(crate) fn empty() -> Self {
        let mut detail = Self::unavailable(String::new());
        detail.unavailable = None;
        detail
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            number: None,
            title: None,
            body: None,
            author: None,
            base_ref_name: None,
            head_ref_name: None,
            created_at: None,
            updated_at: None,
            labels: Vec::new(),
            url: None,
            review_decision: None,
            is_draft: None,
            reviewers: Vec::new(),
            mergeable: None,
            merge_state_status: None,
            auto_merge_enabled: false,
            head_sha: None,
            checks: None,
            comments: Vec::new(),
            actions: Vec::new(),
            files: Vec::new(),
            commits: Vec::new(),
            timeline: Vec::new(),
            timeline_unavailable: None,
            collaborators: Vec::new(),
            collaborators_unavailable: None,
            unresolved_review_threads: None,
            unavailable: Some(message.into()),
            observed_at: SystemTime::now(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct WorkItemDetailCache {
    entries: HashMap<crate::app::state::WorkItemKey, CachedWorkItemDetail>,
    order: VecDeque<crate::app::state::WorkItemKey>,
}

#[derive(Clone, Debug)]
struct CachedWorkItemDetail {
    detail: WorkItemDetail,
    refreshed_at: Option<Instant>,
}

impl WorkItemDetailCache {
    pub(crate) fn from_snapshot(snapshot: Option<&Snapshot>) -> Self {
        let mut cache = Self::default();
        for item in snapshot.into_iter().flat_map(|snapshot| &snapshot.items) {
            let (Some(number), Some(detail)) = (item.pr_number, item.cached_pr_detail.clone())
            else {
                continue;
            };
            // A detail persisted before comments were ordered still carries
            // the provider's oldest-first list. Order it on the way in, so the
            // render and the key handler agree before the first refresh lands.
            // Pull-request comments are flat, so there are no threads to keep
            // together here.
            let mut detail = detail;
            detail
                .comments
                .sort_by_key(|comment| newest_first_key(comment.created_at));
            cache.insert_stale(
                crate::app::state::WorkItemKey {
                    repo: item.repo.clone(),
                    pr_number: Some(number),
                    pr_url: item.pr_url.clone(),
                    ticket_id: None,
                },
                detail,
            );
        }
        cache
    }

    pub(crate) fn get(&self, key: &crate::app::state::WorkItemKey) -> Option<&WorkItemDetail> {
        self.entries.get(key).map(|cached| &cached.detail)
    }

    /// Forget one entry, so the next refresh re-reads it. Used after a write,
    /// where the cached copy is known to be stale the moment it succeeds.
    pub(crate) fn remove(&mut self, key: &crate::app::state::WorkItemKey) {
        self.entries.remove(key);
        self.order.retain(|entry| entry != key);
    }

    pub(crate) fn insert(&mut self, key: crate::app::state::WorkItemKey, detail: WorkItemDetail) {
        self.insert_at(key, detail, Instant::now());
    }

    fn insert_stale(&mut self, key: crate::app::state::WorkItemKey, detail: WorkItemDetail) {
        let lookup = key.clone();
        self.insert_at(key, detail, Instant::now());
        if let Some(cached) = self.entries.get_mut(&lookup) {
            cached.refreshed_at = None;
        }
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    fn insert_at(
        &mut self,
        key: crate::app::state::WorkItemKey,
        detail: WorkItemDetail,
        refreshed_at: Instant,
    ) {
        self.order.retain(|existing| existing != &key);
        self.order.push_back(key.clone());
        self.entries.insert(
            key,
            CachedWorkItemDetail {
                detail,
                refreshed_at: Some(refreshed_at),
            },
        );
        while self.entries.len() > WORK_ITEM_DETAIL_CACHE_CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    fn is_fresh(
        &self,
        key: &crate::app::state::WorkItemKey,
        now: Instant,
        interval: Duration,
    ) -> bool {
        self.entries.get(key).is_some_and(|cached| {
            cached.refreshed_at.is_some_and(|refreshed_at| {
                now.checked_duration_since(refreshed_at)
                    .is_some_and(|age| age < interval)
            })
        })
    }

    #[cfg(all(test, unix))]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkItemPane {
    pub pane_id: String,
    /// Human-facing agent label (`cc·opus·high`), not the pane id: the PR
    /// projection shows who owns the work, and a pane id names nobody.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    pub workspace_id: String,
    pub tab_id: String,
    pub role: Option<PaneWorkRole>,
    pub active_owner: bool,
    pub agent_status: AgentStatus,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkItemSource {
    pub github: bool,
    pub linear: bool,
    pub pane: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkItem {
    pub repo: String,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    pub pr_title: Option<String>,
    pub pr_state: Option<String>,
    pub draft: bool,
    pub review_decision: Option<String>,
    #[serde(default)]
    pub created_at: Option<SystemTime>,
    #[serde(default)]
    pub updated_at: Option<SystemTime>,
    #[serde(default)]
    pub additions: u64,
    #[serde(default)]
    pub deletions: u64,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub assignees: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub check_state: PrCheckState,
    #[serde(default)]
    pub audience: PrAudience,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cached_pr_detail: Option<WorkItemDetail>,
    pub ticket_ids: Vec<String>,
    pub ticket_title: Option<String>,
    pub ticket_state: Option<String>,
    #[serde(default)]
    pub ticket_details: Vec<WorkTicket>,
    pub branch: Option<String>,
    pub preview_urls: Vec<String>,
    pub panes: Vec<WorkItemPane>,
    pub source: WorkItemSource,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PrCheckState {
    Passing,
    Failing,
    Pending,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PrAudience {
    Authored,
    Other,
    #[default]
    Unclassified,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkPerson {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkTicket {
    pub(crate) identifier: String,
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) state: Option<String>,
    pub(crate) assignee: Option<String>,
    #[serde(default)]
    pub(crate) creator: Option<WorkPerson>,
    #[serde(default)]
    pub(crate) priority: Option<u8>,
    #[serde(default)]
    pub(crate) cycle: Option<String>,
    #[serde(default)]
    pub(crate) group: TicketGroup,
    pub(crate) created_at: Option<SystemTime>,
    pub(crate) updated_at: Option<SystemTime>,
    pub(crate) branch: Option<String>,
    pub(crate) labels: Vec<String>,
    pub(crate) url: Option<String>,
    pub(crate) parent: Option<String>,
    pub(crate) relations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MissiveUser {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) email: Option<String>,
    #[serde(default)]
    pub(crate) is_me: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MissiveTeam {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) organization: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MissiveEntry {
    pub(crate) id: String,
    pub(crate) author: Option<String>,
    pub(crate) preview: String,
    pub(crate) created_at: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MissiveConversation {
    pub(crate) id: String,
    pub(crate) subject: String,
    pub(crate) app_url: String,
    pub(crate) web_url: String,
    #[serde(default)]
    pub(crate) team: Option<MissiveTeam>,
    pub(crate) assignees: Vec<MissiveUser>,
    pub(crate) last_activity_at: Option<SystemTime>,
    pub(crate) closed: bool,
    #[serde(default)]
    pub(crate) labels: Vec<String>,
    #[serde(default)]
    pub(crate) pane_bound: bool,
    #[serde(default)]
    pub(crate) messages: Vec<MissiveEntry>,
    #[serde(default)]
    pub(crate) notes: Vec<MissiveEntry>,
    #[serde(default)]
    pub(crate) drafts: Vec<MissiveEntry>,
    #[serde(default)]
    pub(crate) posts: Vec<MissiveEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkIndexSource {
    Github,
    Linear,
    Missive,
}

impl WorkIndexSource {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Github => "GitHub",
            Self::Linear => "Linear",
            Self::Missive => "Missive",
        }
    }

    const fn provider(self) -> Provider {
        match self {
            Self::Github => Provider::Github,
            Self::Linear => Provider::Linear,
            Self::Missive => Provider::Missive,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkIndexUnavailable {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    github: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    linear: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    missive: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    github_rate_limited_until: Option<SystemTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    linear_rate_limited_until: Option<SystemTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    missive_rate_limited_until: Option<SystemTime>,
}

impl WorkIndexUnavailable {
    pub(crate) fn only(source: WorkIndexSource, reason: impl Into<String>) -> Self {
        let mut unavailable = Self::default();
        let reason = Self::normalized_reason(source, reason.into());
        match source {
            WorkIndexSource::Github => unavailable.github = Some(reason),
            WorkIndexSource::Linear => unavailable.linear = Some(reason),
            WorkIndexSource::Missive => unavailable.missive = Some(reason),
        }
        unavailable
    }

    fn record(&mut self, source: WorkIndexSource, reason: impl Into<String>) {
        if self.is_empty() {
            *self = Self::only(source, reason);
            return;
        }
        let destination = match source {
            WorkIndexSource::Github => &mut self.github,
            WorkIndexSource::Linear => &mut self.linear,
            WorkIndexSource::Missive => &mut self.missive,
        };
        if destination.is_none() {
            *destination = Some(Self::normalized_reason(source, reason.into()));
        }
    }

    fn replace_reason(&mut self, source: WorkIndexSource, reason: String) {
        match source {
            WorkIndexSource::Github => self.github = Some(reason),
            WorkIndexSource::Linear => self.linear = Some(reason),
            WorkIndexSource::Missive => self.missive = Some(reason),
        }
    }

    fn normalized_reason(source: WorkIndexSource, reason: String) -> String {
        reason
            .strip_prefix(source.label())
            .map(|reason| reason.trim_start_matches([' ', ':']).to_string())
            .unwrap_or(reason)
    }

    pub(crate) fn reason(&self, source: WorkIndexSource) -> Option<&str> {
        match source {
            WorkIndexSource::Github => self.github.as_deref(),
            WorkIndexSource::Linear => self.linear.as_deref(),
            WorkIndexSource::Missive => self.missive.as_deref(),
        }
    }

    fn record_rate_limit_until(&mut self, source: WorkIndexSource, until: SystemTime) {
        match source {
            WorkIndexSource::Github => self.github_rate_limited_until = Some(until),
            WorkIndexSource::Linear => self.linear_rate_limited_until = Some(until),
            WorkIndexSource::Missive => self.missive_rate_limited_until = Some(until),
        }
    }

    fn rate_limited_until(&self, source: WorkIndexSource) -> Option<SystemTime> {
        match source {
            WorkIndexSource::Github => self.github_rate_limited_until,
            WorkIndexSource::Linear => self.linear_rate_limited_until,
            WorkIndexSource::Missive => self.missive_rate_limited_until,
        }
    }

    pub(crate) fn short_reason(&self, source: WorkIndexSource, now: SystemTime) -> Option<String> {
        let reason = self.reason(source)?;
        let lower = reason.to_ascii_lowercase();
        if lower.contains("rate limit") || lower.contains("http 429") {
            let retry = self.rate_limited_until(source).and_then(|until| {
                until.duration_since(now).ok().map(|duration| {
                    let minutes = duration.as_secs().saturating_add(59) / 60;
                    format!(" · retry in {}m", minutes.max(1))
                })
            });
            return Some(format!("rate limited{}", retry.unwrap_or_default()));
        }
        if lower.contains("timed out") {
            return Some(format!(
                "timed out ({}s)",
                WORK_INDEX_TARGET_TIMEOUT.as_secs()
            ));
        }
        if source == WorkIndexSource::Missive
            && (lower.contains("team not configured") || lower.contains("team is not configured"))
        {
            return Some("team not configured".to_string());
        }
        if let Some(group) = ["Triage", "Assigned", "DoneThisCycle"]
            .into_iter()
            .find(|group| reason.starts_with(group))
        {
            return Some(format!("{group} query failed (see log)"));
        }
        Some("query failed (see log)".to_string())
    }

    pub(crate) fn summary(&self) -> String {
        [
            WorkIndexSource::Github,
            WorkIndexSource::Linear,
            WorkIndexSource::Missive,
        ]
        .into_iter()
        .filter_map(|source| {
            self.reason(source)
                .map(|reason| format!("{}: {reason}", source.label()))
        })
        .collect::<Vec<_>>()
        .join("; ")
    }

    fn is_empty(&self) -> bool {
        self.github.is_none() && self.linear.is_none() && self.missive.is_none()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TicketGroup {
    #[default]
    Assigned,
    Triage,
    DoneThisCycle,
}

impl TicketGroup {
    const fn label(self) -> &'static str {
        match self {
            Self::Assigned => "Assigned",
            Self::Triage => "Triage",
            Self::DoneThisCycle => "DoneThisCycle",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    pub items: Vec<WorkItem>,
    #[serde(default)]
    pub conversations: Vec<MissiveConversation>,
    /// Session-only Missive identity directory. The app reuses it across
    /// refreshes, but the disk cache must not revive a previous token owner's
    /// identity in a later session.
    #[serde(skip)]
    pub missive_users: Vec<MissiveUser>,
    pub unavailable: Option<WorkIndexUnavailable>,
    pub observed_at: SystemTime,
}

impl Snapshot {
    fn cache_pr_detail(
        &mut self,
        key: &crate::app::state::WorkItemKey,
        detail: &WorkItemDetail,
    ) -> bool {
        let Some(number) = key.pr_number else {
            return false;
        };
        let Some(item) = self
            .items
            .iter_mut()
            .find(|item| item.pr_number == Some(number) && repo_slugs_match(&item.repo, &key.repo))
        else {
            return false;
        };
        item.cached_pr_detail = Some(detail.clone());
        true
    }

    pub(crate) fn unavailable_reason(&self, source: WorkIndexSource) -> Option<&str> {
        self.unavailable
            .as_ref()
            .and_then(|unavailable| unavailable.reason(source))
    }

    pub(crate) fn unavailable_summary(&self) -> Option<String> {
        self.unavailable
            .as_ref()
            .map(WorkIndexUnavailable::summary)
            .filter(|summary| !summary.is_empty())
    }

    pub(crate) fn short_unavailable_reason(
        &self,
        source: WorkIndexSource,
        now: SystemTime,
    ) -> Option<String> {
        self.unavailable
            .as_ref()
            .and_then(|unavailable| unavailable.short_reason(source, now))
    }
}

/// Provider identities and assignable users observed by the work-index job.
/// This is session runtime state, not part of the persisted work-index cache.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WorkIndexSession {
    pub(crate) linear: ProviderDirectory,
    pub(crate) github: ProviderDirectory,
    pub(crate) missive: ProviderDirectory,
    linear_query_identity: Option<String>,
    missive_users: Vec<MissiveUser>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProviderDirectory {
    pub(crate) viewer: Option<String>,
    pub(crate) assignees: Vec<String>,
    resolved: bool,
}

impl WorkIndexSession {
    pub(crate) fn linear_viewer_identity(&self) -> Option<&str> {
        self.linear_query_identity.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn set_linear_viewer_identity_for_test(&mut self, identity: impl Into<String>) {
        self.linear_query_identity = Some(identity.into());
    }
}

#[derive(Debug, Clone)]
struct GithubPullRequest {
    repo: String,
    number: u64,
    url: String,
    title: String,
    body: String,
    branch: String,
    draft: bool,
    state: String,
    review_decision: Option<String>,
    created_at: Option<SystemTime>,
    updated_at: Option<SystemTime>,
    additions: u64,
    deletions: u64,
    author: Option<String>,
    assignees: Vec<String>,
    labels: Vec<String>,
    check_state: PrCheckState,
    audience: PrAudience,
}

type LinearTicket = WorkTicket;

#[derive(Debug, Clone)]
struct Attachment {
    ticket_id: String,
    repo: String,
    number: u64,
    url: String,
    title: Option<String>,
    state: Option<String>,
    draft: bool,
    branch: Option<String>,
    preview_urls: Vec<String>,
}

#[derive(Debug)]
enum RefreshError {
    TimedOut,
    Failed(String),
}

fn run_provider_command(
    cache: Option<&ProviderCache>,
    source: WorkIndexSource,
    command: std::process::Command,
    ttl: Duration,
    bypass: bool,
    deadline: Instant,
) -> io::Result<ProviderOutput> {
    if let Some(cache) = cache {
        let program = Path::new(command.get_program()).to_path_buf();
        let argv = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        return cache.run(
            source.provider(),
            &program,
            &argv,
            CachePolicy { ttl, bypass },
            deadline,
        );
    }
    crate::noninteractive_process::output_with_deadline(command, deadline).map(|output| {
        ProviderOutput {
            status: output.status,
            stdout: output.stdout,
            stderr: output.stderr,
        }
    })
}

const MISSIVE_API_BASE: &str = "https://public.missiveapp.com/v1";

#[derive(Clone, Debug, PartialEq, Eq)]
enum MissiveRequest {
    Conversations { team: String },
    Conversation { id: String },
    ConversationMessages { id: String },
    Message { id: String },
    ConversationDrafts { id: String },
    ConversationPosts { id: String },
    ConversationNotes { id: String },
    Users { organization: Option<String> },
}

impl MissiveRequest {
    const fn method(&self) -> &'static str {
        "GET"
    }

    fn url(&self) -> String {
        match self {
            Self::Conversations { team } => format!(
                "{MISSIVE_API_BASE}/conversations?team_all={}",
                percent_encode(team)
            ),
            Self::Conversation { id } => {
                format!("{MISSIVE_API_BASE}/conversations/{}", percent_encode(id))
            }
            Self::ConversationMessages { id } => format!(
                "{MISSIVE_API_BASE}/conversations/{}/messages",
                percent_encode(id)
            ),
            Self::Message { id } => {
                format!("{MISSIVE_API_BASE}/messages/{}", percent_encode(id))
            }
            Self::ConversationDrafts { id } => format!(
                "{MISSIVE_API_BASE}/conversations/{}/drafts",
                percent_encode(id)
            ),
            Self::ConversationPosts { id } => format!(
                "{MISSIVE_API_BASE}/conversations/{}/posts",
                percent_encode(id)
            ),
            Self::ConversationNotes { id } => format!(
                "{MISSIVE_API_BASE}/conversations/{}/comments",
                percent_encode(id)
            ),
            Self::Users { organization } => organization.as_ref().map_or_else(
                || format!("{MISSIVE_API_BASE}/users"),
                |organization| {
                    format!(
                        "{MISSIVE_API_BASE}/users?organization={}",
                        percent_encode(organization)
                    )
                },
            ),
        }
    }

    const fn label(&self) -> &'static str {
        match self {
            Self::Conversations { .. } => "conversation list",
            Self::Conversation { .. } => "conversation",
            Self::ConversationMessages { .. } => "conversation messages",
            Self::Message { .. } => "message",
            Self::ConversationDrafts { .. } => "conversation drafts",
            Self::ConversationPosts { .. } => "conversation posts",
            Self::ConversationNotes { .. } => "conversation comments",
            Self::Users { .. } => "users",
        }
    }
}

fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

fn run_missive_get(
    request: &MissiveRequest,
    config: &MissiveConfig,
    program: &Path,
    deadline: Instant,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> Result<Value, RefreshError> {
    let token = std::env::var(&config.token_env).map_err(|_| {
        RefreshError::Failed(format!(
            "Missive token environment variable {} is not set",
            config.token_env
        ))
    })?;
    if token.trim().is_empty() {
        return Err(RefreshError::Failed(format!(
            "Missive token environment variable {} is empty",
            config.token_env
        )));
    }
    let mut command = crate::noninteractive_process::command(program);
    command.args([
        "--fail-with-body",
        "--silent",
        "--show-error",
        "--request",
        request.method(),
        "--header",
        &format!("Authorization: Bearer {token}"),
        &request.url(),
    ]);
    let output = run_provider_command(
        cache,
        WorkIndexSource::Missive,
        command,
        MISSIVE_LIST_CACHE_TTL,
        bypass,
        deadline,
    )
    .map_err(|error| {
        if error.kind() == io::ErrorKind::TimedOut {
            RefreshError::TimedOut
        } else {
            RefreshError::Failed(format!("Missive {} GET could not run", request.label()))
        }
    })?;
    if !output.status.success() {
        return Err(RefreshError::Failed(format!(
            "Missive {} GET failed with status {}",
            request.label(),
            output.status
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| RefreshError::Failed("Missive GET returned invalid JSON".into()))
}

fn value_array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn missive_user(value: &Value) -> Option<MissiveUser> {
    let id = value.get("id")?.as_str()?.to_string();
    let email = value_text(value.get("email"));
    let name = value_text(value.get("name"))
        .or_else(|| value_text(value.get("display_name")))
        .or_else(|| email.clone())
        .unwrap_or_else(|| id.clone());
    Some(MissiveUser {
        id,
        name,
        email,
        is_me: value
            .get("me")
            .or_else(|| value.get("is_me"))
            .or_else(|| value.get("current"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn missive_team(value: &Value) -> Option<MissiveTeam> {
    let id = value.get("id")?.as_str()?.to_string();
    Some(MissiveTeam {
        name: value_text(value.get("name")).unwrap_or_else(|| id.clone()),
        organization: value_text(value.get("organization")),
        id,
    })
}

fn missive_time(value: Option<&Value>) -> Option<SystemTime> {
    if let Some(seconds) = value.and_then(Value::as_u64) {
        return Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds));
    }
    value
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_system_time)
}

fn missive_preview(value: &Value) -> String {
    let text = ["preview", "body", "text", "markdown", "subject"]
        .into_iter()
        .find_map(|key| value_text(value.get(key)))
        .or_else(|| {
            value
                .get("notification")
                .and_then(|notification| value_text(notification.get("body")))
        })
        .or_else(|| {
            value
                .get("notification")
                .and_then(|notification| value_text(notification.get("title")))
        })
        .unwrap_or_default();
    let mut plain = String::with_capacity(text.len().min(240));
    let mut in_tag = false;
    for character in text.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => plain.push(character),
            _ => {}
        }
        if plain.chars().count() >= 240 {
            break;
        }
    }
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn missive_author(value: &Value) -> Option<String> {
    value
        .get("author")
        .or_else(|| value.get("from_field"))
        .and_then(|author| {
            value_text(author.get("name")).or_else(|| value_text(author.get("address")))
        })
        .or_else(|| value_text(value.get("username")))
}

fn missive_entry(value: &Value) -> Option<MissiveEntry> {
    Some(MissiveEntry {
        id: value_text(value.get("id")).unwrap_or_default(),
        author: missive_author(value),
        preview: missive_preview(value),
        created_at: missive_time(
            value
                .get("delivered_at")
                .or_else(|| value.get("created_at")),
        ),
    })
}

fn parse_missive_entries(value: &Value, key: &str) -> Vec<MissiveEntry> {
    value_array(value, key)
        .iter()
        .filter_map(missive_entry)
        .collect()
}

fn parse_missive_conversation_for_user(
    value: &Value,
    current_user_id: Option<&str>,
) -> Option<MissiveConversation> {
    let id = value.get("id")?.as_str()?.to_string();
    let fallback_url = format!("https://mail.missiveapp.com/#inbox/conversations/{id}");
    let web_url = value_text(value.get("web_url")).unwrap_or_else(|| fallback_url.clone());
    let app_url = value_text(value.get("app_url")).unwrap_or_else(|| web_url.clone());
    let closed_for_user = current_user_id.is_some_and(|current_user_id| {
        value_array(value.get("users").unwrap_or(&Value::Null), "users")
            .iter()
            .find(|user| user.get("id").and_then(Value::as_str) == Some(current_user_id))
            .and_then(|user| user.get("closed"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    });
    Some(MissiveConversation {
        subject: value_text(value.get("subject"))
            .or_else(|| value_text(value.get("latest_message_subject")))
            .unwrap_or_else(|| "(no subject)".into()),
        assignees: value_array(value.get("assignees").unwrap_or(&Value::Null), "users")
            .iter()
            .filter_map(missive_user)
            .collect(),
        team: value.get("team").and_then(missive_team),
        last_activity_at: missive_time(value.get("last_activity_at")),
        closed: value
            .get("closed")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| {
                value
                    .get("closed_at")
                    .is_some_and(|closed| !closed.is_null())
            })
            || closed_for_user,
        labels: value_array(value.get("labels").unwrap_or(&Value::Null), "labels")
            .iter()
            .filter_map(|label| {
                label
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| value_text(label.get("name")))
            })
            .collect(),
        pane_bound: false,
        id,
        app_url,
        web_url,
        messages: Vec::new(),
        notes: Vec::new(),
        drafts: Vec::new(),
        posts: Vec::new(),
    })
}

fn parse_missive_conversations_for_user(
    value: &Value,
    current_user_id: Option<&str>,
) -> Vec<MissiveConversation> {
    value_array(value, "conversations")
        .iter()
        .filter_map(|value| parse_missive_conversation_for_user(value, current_user_id))
        .collect()
}

fn missive_resource<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value.get(key) {
        Some(Value::Array(values)) => values.first(),
        Some(value) => Some(value),
        None if value.is_object() => Some(value),
        None => None,
    }
}

fn missive_conversation_id(url: &str) -> Option<&str> {
    url.split("/conversations/")
        .nth(1)
        .and_then(|tail| tail.split(['?', '#', '/']).next())
        .filter(|id| !id.is_empty())
}

fn fetch_missive_conversation_detail(
    id: &str,
    current_user_id: Option<&str>,
    config: &MissiveConfig,
    program: &Path,
    batch_deadline: Instant,
    target_timeout: Duration,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> Result<MissiveConversation, RefreshError> {
    let get = |request| {
        run_missive_get(
            &request,
            config,
            program,
            target_deadline(batch_deadline, target_timeout),
            cache,
            bypass,
        )
    };
    let value = get(MissiveRequest::Conversation { id: id.into() })?;
    let object = missive_resource(&value, "conversations")
        .ok_or_else(|| RefreshError::Failed("Missive returned no conversation".into()))?;
    let mut conversation = parse_missive_conversation_for_user(object, current_user_id)
        .ok_or_else(|| RefreshError::Failed("Missive returned an invalid conversation".into()))?;
    let messages = get(MissiveRequest::ConversationMessages { id: id.into() })?;
    conversation.messages = parse_missive_entries(&messages, "messages");
    for message in &mut conversation.messages {
        if message.id.is_empty() {
            continue;
        }
        let detail = get(MissiveRequest::Message {
            id: message.id.clone(),
        })?;
        if let Some(hydrated) = missive_resource(&detail, "messages").and_then(missive_entry) {
            *message = hydrated;
        }
    }
    conversation.drafts = parse_missive_entries(
        &get(MissiveRequest::ConversationDrafts { id: id.into() })?,
        "drafts",
    );
    conversation.posts = parse_missive_entries(
        &get(MissiveRequest::ConversationPosts { id: id.into() })?,
        "posts",
    );
    conversation.notes = parse_missive_entries(
        &get(MissiveRequest::ConversationNotes { id: id.into() })?,
        "comments",
    );
    Ok(conversation)
}

fn fetch_missive_snapshot(
    config: &MissiveConfig,
    panes: &[WorkIndexPane],
    selected_conversation: Option<&str>,
    session_users: Option<&[MissiveUser]>,
    program: &Path,
    batch_deadline: Instant,
    target_timeout: Duration,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> Result<(Vec<MissiveConversation>, Vec<MissiveUser>), RefreshError> {
    let pane_ids = panes
        .iter()
        .flat_map(|pane| pane.work_context.missive_urls.iter())
        .filter_map(|url| missive_conversation_id(url).map(str::to_string))
        .collect::<HashSet<_>>();
    let configured = config
        .team
        .as_deref()
        .is_some_and(|team| !team.trim().is_empty());
    let token_available =
        std::env::var_os(&config.token_env).is_some_and(|value| !value.is_empty());
    if !configured && !pane_ids.is_empty() {
        return Err(RefreshError::Failed(
            "Missive team is not configured".into(),
        ));
    }
    if !token_available && !pane_ids.is_empty() {
        return Err(RefreshError::Failed(format!(
            "Missive token environment variable {} is not set",
            config.token_env
        )));
    }
    if !configured || !token_available {
        return Ok((Vec::new(), Vec::new()));
    }
    let team = config.team.clone().unwrap_or_default();
    let users = match session_users {
        Some(users) => users.to_vec(),
        None => {
            let users_value = run_missive_get(
                &MissiveRequest::Users {
                    organization: config.organization.clone(),
                },
                config,
                program,
                target_deadline(batch_deadline, target_timeout),
                cache,
                bypass,
            )?;
            value_array(&users_value, "users")
                .iter()
                .filter_map(missive_user)
                .collect::<Vec<_>>()
        }
    };
    let current_user_id = users
        .iter()
        .find(|user| user.is_me)
        .map(|user| user.id.as_str());
    let conversations_value = run_missive_get(
        &MissiveRequest::Conversations { team },
        config,
        program,
        target_deadline(batch_deadline, target_timeout),
        cache,
        bypass,
    )?;
    let mut conversations =
        parse_missive_conversations_for_user(&conversations_value, current_user_id);
    for conversation in &mut conversations {
        conversation.pane_bound = pane_ids.contains(&conversation.id);
    }
    let mut detail_ids = pane_ids.clone();
    if let Some(selected) = selected_conversation {
        detail_ids.insert(selected.to_string());
    } else if let Some(first) = conversations.first() {
        detail_ids.insert(first.id.clone());
    }
    for id in detail_ids {
        let detail = fetch_missive_conversation_detail(
            &id,
            current_user_id,
            config,
            program,
            batch_deadline,
            target_timeout,
            cache,
            bypass,
        );
        let mut detail = match detail {
            Ok(detail) => detail,
            Err(error) if pane_ids.contains(&id) => return Err(error),
            Err(_) => continue,
        };
        detail.pane_bound = pane_ids.contains(&id);
        if let Some(index) = conversations
            .iter()
            .position(|conversation| conversation.id == detail.id)
        {
            conversations[index] = detail;
        } else {
            conversations.push(detail);
        }
    }
    conversations.sort_by_key(|conversation| std::cmp::Reverse(conversation.last_activity_at));
    Ok((conversations, users))
}

fn work_index_repos(config: &WorkIndexConfig, panes: &[WorkIndexPane]) -> Vec<String> {
    let mut repos: Vec<String> = Vec::new();
    let candidates = config
        .repos
        .iter()
        .filter_map(|repo| normalize_repo_slug(repo).ok())
        .chain(
            panes
                .iter()
                .filter_map(|pane| pane.work_context.repo.as_deref())
                .filter_map(|repo| normalize_repo_slug(repo).ok()),
        )
        .chain(
            panes
                .iter()
                .flat_map(|pane| pane.work_context.pr_urls.iter())
                .filter_map(|url| repo_slug_from_pr_url(url)),
        );
    for candidate in candidates {
        if !repos.iter().any(|repo| repo_slugs_match(repo, &candidate)) {
            repos.push(candidate);
        }
    }
    repos
}

fn direct_pr_urls(
    panes: &[WorkIndexPane],
    day_links: Option<&crate::day::DayLinks>,
) -> Vec<String> {
    let mut urls = panes
        .iter()
        .flat_map(|pane| pane.work_context.pr_urls.iter().cloned())
        .chain(
            day_links
                .into_iter()
                .flat_map(|links| links.prs.iter().cloned()),
        )
        .collect::<Vec<_>>();
    // One pull request reaches here spelled more than one way: a pane's work
    // context and a day link can disagree on owner and repo case, and GitHub
    // rewrites that case itself. Each spelling that survives costs its own
    // provider read on every refresh.
    // Sort on a case-folded key so the spellings `dedup_by` must compare land
    // next to each other; a plain sort can leave an unrelated url between them.
    urls.sort_by(|left, right| {
        left.to_ascii_lowercase()
            .cmp(&right.to_ascii_lowercase())
            .then_with(|| left.cmp(right))
    });
    urls.dedup_by(|left, right| same_pull_request_url(left, right));
    urls
}

/// GitHub rewrites owner and repo case and may drop a trailing slash, so a
/// stored url and the reported one can differ while naming the same PR.
pub(crate) fn same_pull_request_url(left: &str, right: &str) -> bool {
    left.trim_end_matches('/')
        .eq_ignore_ascii_case(right.trim_end_matches('/'))
}

fn pane_pr_target(url: &str) -> Option<(String, u64)> {
    let repo = repo_slug_from_pr_url(url)?;
    let number = url.trim_end_matches('/').rsplit('/').next()?.parse().ok()?;
    Some((repo, number))
}

fn upsert_github(pull_requests: &mut Vec<GithubPullRequest>, pull_request: GithubPullRequest) {
    if let Some(index) = pull_requests
        .iter()
        .position(|existing| existing.url == pull_request.url)
    {
        pull_requests[index] = pull_request;
    } else {
        pull_requests.push(pull_request);
    }
}

fn directly_observed_ticket_ids(
    panes: &[WorkIndexPane],
    day_links: Option<&crate::day::DayLinks>,
) -> Vec<String> {
    let mut ids = panes
        .iter()
        .flat_map(|pane| pane.work_context.ticket_ids.iter())
        .chain(day_links.into_iter().flat_map(|links| links.tickets.iter()))
        .filter_map(|id| normalize_ticket_id(id).ok())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn previous_github(previous: Option<&Snapshot>) -> Vec<GithubPullRequest> {
    previous
        .into_iter()
        .flat_map(|snapshot| snapshot.items.iter())
        .filter(|item| item.source.github)
        .filter_map(|item| {
            Some(GithubPullRequest {
                repo: item.repo.clone(),
                number: item.pr_number?,
                url: item.pr_url.clone()?,
                title: item.pr_title.clone()?,
                body: item.ticket_ids.join(" "),
                branch: item.branch.clone().unwrap_or_default(),
                draft: item.draft,
                state: item.pr_state.clone().unwrap_or_else(|| "open".into()),
                review_decision: item.review_decision.clone(),
                created_at: item.created_at,
                updated_at: item.updated_at,
                additions: item.additions,
                deletions: item.deletions,
                author: item.author.clone(),
                assignees: item.assignees.clone(),
                labels: item.labels.clone(),
                check_state: item.check_state,
                audience: item.audience,
            })
        })
        .collect()
}

fn previous_linear(previous: Option<&Snapshot>) -> Vec<LinearTicket> {
    let mut tickets = Vec::new();
    for ticket in previous
        .into_iter()
        .flat_map(|snapshot| snapshot.items.iter())
        .filter(|item| item.source.linear)
        .flat_map(|item| item.ticket_details.iter())
    {
        if !tickets
            .iter()
            .any(|existing: &LinearTicket| existing.identifier == ticket.identifier)
        {
            tickets.push(ticket.clone());
        }
    }
    tickets
}

fn previous_missive(
    previous: Option<&Snapshot>,
    panes: &[WorkIndexPane],
) -> Vec<MissiveConversation> {
    let pane_ids = panes
        .iter()
        .flat_map(|pane| pane.work_context.missive_urls.iter())
        .filter_map(|url| missive_conversation_id(url))
        .collect::<HashSet<_>>();
    previous
        .map(|snapshot| {
            snapshot
                .conversations
                .iter()
                .cloned()
                .map(|mut conversation| {
                    conversation.pane_bound = pane_ids.contains(conversation.id.as_str());
                    conversation
                })
                .collect()
        })
        .unwrap_or_default()
}

fn carry_cached_pr_details(items: &mut [WorkItem], previous: Option<&Snapshot>) {
    let Some(previous) = previous else {
        return;
    };
    for item in items {
        let Some(number) = item.pr_number else {
            continue;
        };
        item.cached_pr_detail = previous
            .items
            .iter()
            .find(|previous| {
                previous.pr_number == Some(number) && repo_slugs_match(&previous.repo, &item.repo)
            })
            .and_then(|previous| previous.cached_pr_detail.clone());
    }
}

#[cfg(all(test, unix))]
pub(crate) fn refresh_work_index(
    config: &WorkIndexConfig,
    panes: &[WorkIndexPane],
    now: Instant,
    batch_deadline: Instant,
    target_timeout: Duration,
    gh_program: &Path,
    linearis_program: &Path,
) -> Snapshot {
    refresh_work_index_with_missive(
        config,
        &MissiveConfig::default(),
        panes,
        WorkIndexRefreshContext::default(),
        now,
        batch_deadline,
        target_timeout,
        gh_program,
        linearis_program,
        Path::new("curl"),
    )
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct WorkIndexRefreshContext<'a> {
    pub(crate) selected_missive: Option<&'a str>,
    pub(crate) session_missive_users: Option<&'a [MissiveUser]>,
    pub(crate) previous: Option<&'a Snapshot>,
    pub(crate) linear_assignee: Option<&'a str>,
    pub(crate) selected_ticket: Option<&'a str>,
    pub(crate) day_links: Option<&'a crate::day::DayLinks>,
    provider_cache: Option<&'a ProviderCache>,
    pub(crate) cache_bypass: WorkIndexCacheBypass,
}

pub(crate) fn refresh_work_index_with_missive(
    config: &WorkIndexConfig,
    missive: &MissiveConfig,
    panes: &[WorkIndexPane],
    context: WorkIndexRefreshContext<'_>,
    now: Instant,
    batch_deadline: Instant,
    target_timeout: Duration,
    gh_program: &Path,
    linearis_program: &Path,
    curl_program: &Path,
) -> Snapshot {
    let WorkIndexRefreshContext {
        selected_missive,
        session_missive_users,
        previous,
        linear_assignee,
        selected_ticket,
        day_links,
        provider_cache,
        cache_bypass,
    } = context;
    if !config.enabled {
        return Snapshot {
            items: Vec::new(),
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: SystemTime::now(),
        };
    }

    let repos = work_index_repos(config, panes);
    let mut degraded = WorkIndexUnavailable::default();
    let previous_github = previous_github(previous);
    let mut github = Vec::new();
    for url in direct_pr_urls(panes, day_links) {
        let Some((repo, number)) = pane_pr_target(&url) else {
            degraded.record(
                WorkIndexSource::Github,
                "direct pull request URL is invalid",
            );
            continue;
        };
        match fetch_github_pull_request(
            &repo,
            number,
            gh_program,
            target_deadline(batch_deadline, target_timeout),
            provider_cache,
            cache_bypass.github,
        ) {
            Ok(pull_request) => upsert_github(&mut github, pull_request),
            Err(RefreshError::TimedOut) => {
                degraded.record(
                    WorkIndexSource::Github,
                    "pull request observation timed out",
                );
                if let Some(pull_request) = previous_github
                    .iter()
                    .find(|pull_request| same_pull_request_url(&pull_request.url, &url))
                {
                    upsert_github(&mut github, pull_request.clone());
                }
            }
            Err(RefreshError::Failed(message)) => {
                degraded.record(WorkIndexSource::Github, message);
                if let Some(pull_request) = previous_github
                    .iter()
                    .find(|pull_request| same_pull_request_url(&pull_request.url, &url))
                {
                    upsert_github(&mut github, pull_request.clone());
                }
            }
        }
    }
    for repo in &repos {
        match fetch_github_pull_requests(
            repo,
            gh_program,
            target_deadline(batch_deadline, target_timeout),
            provider_cache,
            cache_bypass.github,
        ) {
            Ok(mut values) => {
                let authored = match fetch_github_pr_numbers(
                    repo,
                    gh_program,
                    &["--author", "@me"],
                    target_deadline(batch_deadline, target_timeout),
                    provider_cache,
                    cache_bypass.github,
                ) {
                    Ok(numbers) => numbers,
                    Err(RefreshError::TimedOut) => {
                        degraded.record(WorkIndexSource::Github, "audience observation timed out");
                        HashSet::new()
                    }
                    Err(RefreshError::Failed(message)) => {
                        degraded.record(WorkIndexSource::Github, message);
                        HashSet::new()
                    }
                };
                let others = match fetch_github_pr_numbers(
                    repo,
                    gh_program,
                    &["--search", "review-requested:@me OR mentions:@me"],
                    target_deadline(batch_deadline, target_timeout),
                    provider_cache,
                    cache_bypass.github,
                ) {
                    Ok(numbers) => numbers,
                    Err(RefreshError::TimedOut) => {
                        degraded.record(WorkIndexSource::Github, "audience observation timed out");
                        HashSet::new()
                    }
                    Err(RefreshError::Failed(message)) => {
                        degraded.record(WorkIndexSource::Github, message);
                        HashSet::new()
                    }
                };
                for value in &mut values {
                    value.audience = if authored.contains(&value.number) {
                        PrAudience::Authored
                    } else if others.contains(&value.number) {
                        PrAudience::Other
                    } else {
                        PrAudience::Unclassified
                    };
                }
                for value in values {
                    upsert_github(&mut github, value);
                }
            }
            Err(RefreshError::TimedOut) => {
                degraded.record(WorkIndexSource::Github, "list observation timed out");
                for pull_request in previous_github
                    .iter()
                    .filter(|pull_request| repo_slugs_match(&pull_request.repo, repo))
                    .cloned()
                {
                    upsert_github(&mut github, pull_request);
                }
            }
            Err(RefreshError::Failed(message)) => {
                degraded.record(WorkIndexSource::Github, message);
                for pull_request in previous_github
                    .iter()
                    .filter(|pull_request| repo_slugs_match(&pull_request.repo, repo))
                    .cloned()
                {
                    upsert_github(&mut github, pull_request);
                }
            }
        }
    }

    let previous_tickets = previous_linear(previous);
    let mut listed_ticket_ids = HashSet::new();
    let mut tickets = match config.linear_team.as_deref() {
        Some(team) if !team.trim().is_empty() => match fetch_linear_tickets(
            team,
            linear_assignee,
            linearis_program,
            target_deadline(batch_deadline, target_timeout),
            provider_cache,
            cache_bypass.linear,
        ) {
            Ok(observation) => {
                let mut tickets = observation.tickets;
                for failure in &observation.group_failures {
                    for ticket in previous_tickets
                        .iter()
                        .filter(|ticket| ticket.group == failure.group)
                    {
                        if !tickets
                            .iter()
                            .any(|current| current.identifier == ticket.identifier)
                        {
                            tickets.push(ticket.clone());
                        }
                    }
                }
                if !observation.group_failures.is_empty() {
                    degraded.record(
                        WorkIndexSource::Linear,
                        observation
                            .group_failures
                            .iter()
                            .map(LinearGroupFailure::summary)
                            .collect::<Vec<_>>()
                            .join("; "),
                    );
                }
                listed_ticket_ids.extend(
                    tickets
                        .iter()
                        .map(|ticket| ticket.identifier.to_ascii_uppercase()),
                );
                tickets
            }
            // Neither a timeout nor a failure may throw away a live GitHub
            // half: 74 pull requests with no ticket edge still beat an empty
            // index. Degrade and name the cause instead.
            Err(RefreshError::TimedOut) => {
                degraded.record(WorkIndexSource::Linear, "observation timed out");
                previous_tickets.clone()
            }
            Err(RefreshError::Failed(message)) => {
                degraded.record(WorkIndexSource::Linear, message);
                previous_tickets.clone()
            }
        },
        _ => Vec::new(),
    };
    let mut directly_observed_tickets = directly_observed_ticket_ids(panes, day_links);
    if let Some(selected) = selected_ticket.and_then(|ticket| normalize_ticket_id(ticket).ok()) {
        if !directly_observed_tickets
            .iter()
            .any(|ticket| ticket.eq_ignore_ascii_case(&selected))
        {
            directly_observed_tickets.push(selected);
        }
    }
    for identifier in directly_observed_tickets {
        match fetch_linear_ticket(
            &identifier,
            linearis_program,
            target_deadline(batch_deadline, target_timeout),
            provider_cache,
            cache_bypass.linear,
        ) {
            Ok(ticket) => {
                if let Some(index) = tickets.iter().position(|existing| {
                    existing.identifier.eq_ignore_ascii_case(&ticket.identifier)
                }) {
                    tickets[index] = ticket;
                } else {
                    tickets.push(ticket);
                }
            }
            Err(RefreshError::TimedOut) => {
                degraded.record(WorkIndexSource::Linear, "ticket observation timed out");
                if !tickets
                    .iter()
                    .any(|ticket| ticket.identifier.eq_ignore_ascii_case(&identifier))
                {
                    if let Some(ticket) = previous_tickets
                        .iter()
                        .find(|ticket| ticket.identifier.eq_ignore_ascii_case(&identifier))
                    {
                        tickets.push(ticket.clone());
                    }
                }
            }
            Err(RefreshError::Failed(message)) => {
                degraded.record(WorkIndexSource::Linear, message);
                if !tickets
                    .iter()
                    .any(|ticket| ticket.identifier.eq_ignore_ascii_case(&identifier))
                {
                    if let Some(ticket) = previous_tickets
                        .iter()
                        .find(|ticket| ticket.identifier.eq_ignore_ascii_case(&identifier))
                    {
                        tickets.push(ticket.clone());
                    }
                }
            }
        }
    }
    let attachments = fetch_attachments(
        &tickets,
        linearis_program,
        batch_deadline,
        target_timeout,
        provider_cache,
        cache_bypass.linear,
    );
    let (conversations, missive_users) = match fetch_missive_snapshot(
        missive,
        panes,
        selected_missive,
        session_missive_users,
        curl_program,
        batch_deadline,
        target_timeout,
        provider_cache,
        cache_bypass.missive,
    ) {
        Ok(snapshot) => snapshot,
        Err(RefreshError::TimedOut) => {
            degraded.record(WorkIndexSource::Missive, "observation timed out");
            (previous_missive(previous, panes), Vec::new())
        }
        Err(RefreshError::Failed(message)) => {
            degraded.record(WorkIndexSource::Missive, message);
            (previous_missive(previous, panes), Vec::new())
        }
    };

    let mut items = github
        .into_iter()
        .map(|pr| {
            let searchable = format!("{}\n{}", pr.branch, pr.body).to_ascii_lowercase();
            let ticket_ids = tickets
                .iter()
                .filter(|ticket| searchable.contains(&ticket.identifier.to_ascii_lowercase()))
                .map(|ticket| ticket.identifier.clone())
                .collect();
            WorkItem {
                repo: pr.repo,
                pr_number: Some(pr.number),
                pr_url: Some(pr.url),
                pr_title: Some(pr.title),
                pr_state: Some(pr.state),
                draft: pr.draft,
                review_decision: pr.review_decision,
                created_at: pr.created_at,
                updated_at: pr.updated_at,
                additions: pr.additions,
                deletions: pr.deletions,
                author: pr.author,
                assignees: pr.assignees,
                labels: pr.labels,
                check_state: pr.check_state,
                audience: pr.audience,
                cached_pr_detail: None,
                ticket_ids,
                ticket_title: None,
                ticket_state: None,
                ticket_details: Vec::new(),
                branch: Some(pr.branch),
                preview_urls: Vec::new(),
                panes: Vec::new(),
                source: WorkItemSource {
                    github: true,
                    ..WorkItemSource::default()
                },
            }
        })
        .collect::<Vec<_>>();

    let mut ticket_by_id = tickets
        .iter()
        .map(|ticket| (ticket.identifier.clone(), ticket))
        .collect::<HashMap<_, _>>();
    for attachment in attachments {
        let ticket = ticket_by_id.remove(&attachment.ticket_id);
        let item_index = items.iter().position(|item| {
            item.pr_number == Some(attachment.number)
                && repo_slugs_match(&item.repo, &attachment.repo)
        });
        let index = if let Some(index) = item_index {
            index
        } else {
            items.push(WorkItem {
                repo: attachment.repo.clone(),
                pr_number: Some(attachment.number),
                pr_url: Some(attachment.url.clone()),
                pr_title: attachment.title.clone(),
                pr_state: attachment.state.clone(),
                draft: attachment.draft,
                review_decision: None,
                created_at: None,
                updated_at: None,
                additions: 0,
                deletions: 0,
                author: None,
                assignees: Vec::new(),
                labels: Vec::new(),
                check_state: PrCheckState::Unknown,
                audience: PrAudience::Unclassified,
                cached_pr_detail: None,
                ticket_ids: Vec::new(),
                ticket_title: None,
                ticket_state: None,
                ticket_details: Vec::new(),
                branch: attachment.branch.clone(),
                preview_urls: attachment.preview_urls.clone(),
                panes: Vec::new(),
                source: WorkItemSource::default(),
            });
            items.len() - 1
        };
        let item = &mut items[index];
        item.source.linear = true;
        push_unique(&mut item.ticket_ids, attachment.ticket_id);
        item.pr_state = item.pr_state.take().or(attachment.state);
        item.branch = item.branch.take().or(attachment.branch);
        item.preview_urls.extend(attachment.preview_urls);
        item.preview_urls.sort();
        item.preview_urls.dedup();
        if let Some(ticket) = ticket {
            item.ticket_title = ticket.title.clone();
            item.ticket_state = ticket.state.clone();
            item.ticket_details.push(ticket.clone());
        }
    }

    for ticket in ticket_by_id.into_values() {
        let Some(_ticket_url) = linear_ticket_url(&ticket.identifier) else {
            continue;
        };
        let repo = panes
            .iter()
            .find(|pane| {
                pane.work_context
                    .ticket_ids
                    .iter()
                    .filter_map(|id| normalize_ticket_id(id).ok())
                    .any(|id| id == ticket.identifier)
            })
            .and_then(|pane| pane.work_context.repo.as_deref())
            .and_then(|repo| normalize_repo_slug(repo).ok())
            .or_else(|| {
                (config.repos.len() == 1)
                    .then(|| normalize_repo_slug(&config.repos[0]).ok())
                    .flatten()
            })
            .unwrap_or_default();
        items.push(WorkItem {
            repo,
            pr_number: None,
            pr_url: None,
            pr_title: None,
            pr_state: None,
            draft: false,
            review_decision: None,
            created_at: None,
            updated_at: None,
            additions: 0,
            deletions: 0,
            author: None,
            assignees: Vec::new(),
            labels: Vec::new(),
            check_state: PrCheckState::Unknown,
            audience: PrAudience::Unclassified,
            cached_pr_detail: None,
            ticket_ids: vec![ticket.identifier.clone()],
            ticket_title: ticket.title.clone(),
            ticket_state: ticket.state.clone(),
            ticket_details: vec![ticket.clone()],
            branch: ticket.branch.clone(),
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: WorkItemSource {
                linear: true,
                ..WorkItemSource::default()
            },
        });
    }

    join_panes(&mut items, panes);
    carry_cached_pr_details(&mut items, previous);
    items.sort_by(|left, right| {
        left.repo
            .cmp(&right.repo)
            .then_with(|| left.pr_number.cmp(&right.pr_number))
            .then_with(|| left.ticket_ids.cmp(&right.ticket_ids))
    });
    let _ = now;
    preserve_previous_rate_limit_reasons(&mut degraded, previous);
    if let Some(cache) = provider_cache {
        cache.finish_refresh();
        record_provider_backoff(&mut degraded, cache);
    }

    Snapshot {
        items,
        conversations,
        missive_users,
        unavailable: (!degraded.is_empty()).then_some(degraded),
        observed_at: SystemTime::now(),
    }
}

fn preserve_previous_rate_limit_reasons(
    unavailable: &mut WorkIndexUnavailable,
    previous: Option<&Snapshot>,
) {
    let Some(previous) = previous.and_then(|snapshot| snapshot.unavailable.as_ref()) else {
        return;
    };
    for source in [
        WorkIndexSource::Github,
        WorkIndexSource::Linear,
        WorkIndexSource::Missive,
    ] {
        let Some(current) = unavailable.reason(source) else {
            continue;
        };
        let Some(prior) = previous.reason(source) else {
            continue;
        };
        if is_rate_limit_reason(current) && is_rate_limit_reason(prior) {
            unavailable.replace_reason(source, prior.to_string());
        }
    }
}

fn is_rate_limit_reason(reason: &str) -> bool {
    let reason = reason.to_ascii_lowercase();
    reason.contains("rate limit") || reason.contains("http 429")
}

fn record_provider_backoff(unavailable: &mut WorkIndexUnavailable, cache: &ProviderCache) {
    for source in [
        WorkIndexSource::Github,
        WorkIndexSource::Linear,
        WorkIndexSource::Missive,
    ] {
        if let Some(rate_limit) = cache.rate_limit(source.provider()) {
            if rate_limit.until > SystemTime::now() {
                unavailable.record(source, rate_limit.reason);
                unavailable.record_rate_limit_until(source, rate_limit.until);
            }
        }
    }
}

fn finalize_provider_refresh(snapshot: &mut Snapshot, cache: &ProviderCache) {
    cache.finish_refresh();
    let mut unavailable = snapshot.unavailable.take().unwrap_or_default();
    record_provider_backoff(&mut unavailable, cache);
    snapshot.unavailable = (!unavailable.is_empty()).then_some(unavailable);
    cache.log_summary();
}

fn exit_detail(label: &str, output: &ProviderOutput) -> String {
    // Keep the child's own words: a bare "exited unsuccessfully" is
    // undiagnosable in the field, which is exactly where these tools fail.
    let stderr = String::from_utf8_lossy(&output.stderr);
    // These tools report failures as pretty-printed JSON, whose first line is
    // a bare "{". Collapse to one line and keep it bounded.
    let collapsed = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
    let detail = (!collapsed.is_empty()).then(|| {
        if collapsed.chars().count() > 200 {
            collapsed.chars().take(200).collect::<String>()
        } else {
            collapsed.clone()
        }
    });
    let detail = detail.as_deref();
    match detail {
        Some(detail) => format!("{label} exited unsuccessfully: {detail}"),
        None => format!("{label} exited unsuccessfully"),
    }
}

fn target_deadline(batch_deadline: Instant, target_timeout: Duration) -> Instant {
    (Instant::now() + target_timeout).min(batch_deadline)
}

/// Resolve provider-local "me" identities and assignee directories once per
/// app session. Callers retain the returned value across index refreshes.
fn resolve_work_index_session(
    config: &WorkIndexConfig,
    mut session: WorkIndexSession,
    missive_users: &[MissiveUser],
    batch_deadline: Instant,
    target_timeout: Duration,
    gh_program: &Path,
    linearis_program: &Path,
    cache: Option<&ProviderCache>,
    bypass: WorkIndexCacheBypass,
) -> WorkIndexSession {
    if !session.linear.resolved {
        (session.linear, session.linear_query_identity) = fetch_linear_directory(
            linearis_program,
            target_deadline(batch_deadline, target_timeout),
            cache,
            bypass.linear,
        );
        session.linear.resolved = true;
    }
    if !session.github.resolved {
        session.github = fetch_github_directory(
            &config.repos,
            gh_program,
            batch_deadline,
            target_timeout,
            cache,
            bypass.github,
        );
        session.github.resolved = true;
    }
    if !session.missive.resolved && !missive_users.is_empty() {
        session.missive = resolve_missive_assignees(missive_users);
        session.missive_users = missive_users.to_vec();
    }
    session
}

fn fetch_linear_directory(
    program: &Path,
    deadline: Instant,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> (ProviderDirectory, Option<String>) {
    let (viewer, query_identity) = {
        let mut command = crate::noninteractive_process::command(program);
        command.args(["auth", "status", "--compact"]);
        run_provider_command(
            cache,
            WorkIndexSource::Linear,
            command,
            VIEWER_IDENTITY_CACHE_TTL,
            bypass,
            deadline,
        )
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok())
        .and_then(|value| {
            let user = value.get("user")?;
            let name = nested_text(Some(user), "name");
            let identity = nested_text(Some(user), "id").or_else(|| name.clone());
            Some((name, identity))
        })
        .unwrap_or_default()
    };
    let mut assignees = {
        let mut command = crate::noninteractive_process::command(program);
        command.args(["users", "list", "--active", "-l", "250", "--compact"]);
        run_provider_command(
            cache,
            WorkIndexSource::Linear,
            command,
            VIEWER_IDENTITY_CACHE_TTL,
            bypass,
            deadline,
        )
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok())
        .and_then(|value| value.get("nodes").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|user| nested_text(Some(&user), "name"))
        .collect::<Vec<_>>()
    };
    include_viewer(&mut assignees, viewer.as_deref());
    (
        ProviderDirectory {
            viewer,
            assignees,
            resolved: true,
        },
        query_identity,
    )
}

fn fetch_github_directory(
    repos: &[String],
    program: &Path,
    batch_deadline: Instant,
    target_timeout: Duration,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> ProviderDirectory {
    let viewer = {
        let mut command = crate::noninteractive_process::command(program);
        command.args(["api", "user"]);
        run_provider_command(
            cache,
            WorkIndexSource::Github,
            command,
            VIEWER_IDENTITY_CACHE_TTL,
            bypass,
            target_deadline(batch_deadline, target_timeout),
        )
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok())
        .and_then(|value| nested_text(Some(&value), "login"))
    };
    let mut assignees = Vec::new();
    for repo in repos
        .iter()
        .filter_map(|repo| normalize_repo_slug(repo).ok())
    {
        let mut command = crate::noninteractive_process::command(program);
        command.args([
            "api",
            &format!("repos/{repo}/assignees"),
            "--paginate",
            "--slurp",
        ]);
        let Some(value) = run_provider_command(
            cache,
            WorkIndexSource::Github,
            command,
            VIEWER_IDENTITY_CACHE_TTL,
            bypass,
            target_deadline(batch_deadline, target_timeout),
        )
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok()) else {
            continue;
        };
        collect_github_assignees(&value, &mut assignees);
    }
    include_viewer(&mut assignees, viewer.as_deref());
    ProviderDirectory {
        viewer,
        assignees,
        resolved: true,
    }
}

fn collect_github_assignees(value: &Value, assignees: &mut Vec<String>) {
    let Some(values) = value.as_array() else {
        return;
    };
    for value in values {
        if value.is_array() {
            collect_github_assignees(value, assignees);
        } else if let Some(login) = nested_text(Some(value), "login") {
            assignees.push(login);
        }
    }
}

fn include_viewer(assignees: &mut Vec<String>, viewer: Option<&str>) {
    if let Some(viewer) = viewer {
        assignees.push(viewer.to_string());
    }
    assignees.sort_by_key(|value| value.to_ascii_lowercase());
    assignees.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
}

pub(crate) fn resolve_missive_assignees(users: &[MissiveUser]) -> ProviderDirectory {
    let viewer = users
        .iter()
        .find(|user| user.is_me)
        .map(|user| user.name.clone());
    let mut assignees = users
        .iter()
        .map(|user| user.name.clone())
        .collect::<Vec<_>>();
    include_viewer(&mut assignees, viewer.as_deref());
    ProviderDirectory {
        viewer,
        assignees,
        resolved: true,
    }
}

const GITHUB_PULL_REQUEST_SUMMARY_FIELDS: &str = "number,title,author,assignees,state,updatedAt,createdAt,reviewDecision,labels,isDraft,headRefName,url";

fn parse_github_pull_request(value: Value, fallback_repo: &str) -> Option<GithubPullRequest> {
    let url = value.get("url")?.as_str()?.to_string();
    Some(GithubPullRequest {
        repo: repo_slug_from_pr_url(&url).unwrap_or_else(|| fallback_repo.to_string()),
        number: value.get("number")?.as_u64()?,
        url,
        title: value.get("title")?.as_str()?.to_string(),
        body: String::new(),
        branch: value.get("headRefName")?.as_str()?.to_string(),
        draft: value
            .get("isDraft")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        state: value
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("OPEN")
            .to_ascii_lowercase(),
        review_decision: value
            .get("reviewDecision")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        created_at: value
            .get("createdAt")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_system_time),
        updated_at: value
            .get("updatedAt")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_system_time),
        additions: value.get("additions").and_then(Value::as_u64).unwrap_or(0),
        deletions: value.get("deletions").and_then(Value::as_u64).unwrap_or(0),
        author: value
            .get("author")
            .and_then(|author| author.get("login"))
            .and_then(Value::as_str)
            .map(str::to_string),
        assignees: value
            .get("assignees")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|assignee| nested_text(Some(assignee), "login"))
            .collect(),
        labels: value
            .get("labels")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|label| value_text(Some(label)))
            .collect(),
        check_state: pr_check_state(value.get("statusCheckRollup")),
        audience: PrAudience::Unclassified,
    })
}

fn fetch_github_pull_requests(
    repo: &str,
    program: &Path,
    deadline: Instant,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> Result<Vec<GithubPullRequest>, RefreshError> {
    let mut command = crate::noninteractive_process::command(program);
    command.args([
        "pr",
        "list",
        "--repo",
        repo,
        "--state",
        "open",
        "--limit",
        "200",
        "--json",
        GITHUB_PULL_REQUEST_SUMMARY_FIELDS,
    ]);
    let output = run_provider_command(
        cache,
        WorkIndexSource::Github,
        command,
        GITHUB_LIST_CACHE_TTL,
        bypass,
        deadline,
    )
    .map_err(|error| {
        if error.kind() == io::ErrorKind::TimedOut {
            RefreshError::TimedOut
        } else {
            RefreshError::Failed(format!("GitHub observation failed: {error}"))
        }
    })?;
    if !output.status.success() {
        return Err(RefreshError::Failed(exit_detail(
            "GitHub observation",
            &output,
        )));
    }
    let values = serde_json::from_slice::<Vec<Value>>(&output.stdout)
        .map_err(|_| RefreshError::Failed("GitHub observation returned invalid JSON".into()))?;
    Ok(values
        .into_iter()
        .filter_map(|value| parse_github_pull_request(value, repo))
        .collect())
}

fn fetch_github_pull_request(
    repo: &str,
    number: u64,
    program: &Path,
    deadline: Instant,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> Result<GithubPullRequest, RefreshError> {
    let mut command = crate::noninteractive_process::command(program);
    let number_arg = number.to_string();
    command.args([
        "pr",
        "view",
        &number_arg,
        "--repo",
        repo,
        "--json",
        GITHUB_PULL_REQUEST_SUMMARY_FIELDS,
    ]);
    let output = run_provider_command(
        cache,
        WorkIndexSource::Github,
        command,
        GITHUB_PR_CACHE_TTL,
        bypass,
        deadline,
    )
    .map_err(|error| {
        if error.kind() == io::ErrorKind::TimedOut {
            RefreshError::TimedOut
        } else {
            RefreshError::Failed(format!("GitHub pull request observation failed: {error}"))
        }
    })?;
    if !output.status.success() {
        return Err(RefreshError::Failed(exit_detail(
            "GitHub pull request observation",
            &output,
        )));
    }
    let value = serde_json::from_slice::<Value>(&output.stdout).map_err(|_| {
        RefreshError::Failed("GitHub pull request observation returned invalid JSON".into())
    })?;
    parse_github_pull_request(value, repo).ok_or_else(|| {
        RefreshError::Failed("GitHub pull request observation returned an invalid item".into())
    })
}

fn fetch_github_pr_numbers(
    repo: &str,
    program: &Path,
    filter: &[&str],
    deadline: Instant,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> Result<HashSet<u64>, RefreshError> {
    let mut command = crate::noninteractive_process::command(program);
    command.args(["pr", "list", "--repo", repo, "--state", "open"]);
    command.args(filter);
    command.args(["--limit", "200", "--json", "number"]);
    let output = run_provider_command(
        cache,
        WorkIndexSource::Github,
        command,
        GITHUB_LIST_CACHE_TTL,
        bypass,
        deadline,
    )
    .map_err(|error| {
        if error.kind() == io::ErrorKind::TimedOut {
            RefreshError::TimedOut
        } else {
            RefreshError::Failed(format!("GitHub PR audience observation failed: {error}"))
        }
    })?;
    if !output.status.success() {
        return Err(RefreshError::Failed(exit_detail(
            "GitHub PR audience observation",
            &output,
        )));
    }
    let values = serde_json::from_slice::<Vec<Value>>(&output.stdout).map_err(|_| {
        RefreshError::Failed("GitHub PR audience observation returned invalid JSON".into())
    })?;
    Ok(values
        .iter()
        .filter_map(|value| value.get("number").and_then(Value::as_u64))
        .collect())
}

fn pr_check_state(value: Option<&Value>) -> PrCheckState {
    let Some(checks) = value.and_then(Value::as_array) else {
        return PrCheckState::Unknown;
    };
    if checks.iter().any(|check| {
        github_check_state(check).is_some_and(|state| {
            matches!(
                state,
                "ERROR" | "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED"
            )
        })
    }) {
        PrCheckState::Failing
    } else if checks.is_empty()
        || checks.iter().any(|check| {
            !matches!(
                github_check_state(check),
                Some("SUCCESS" | "NEUTRAL" | "SKIPPED")
            )
        })
    {
        PrCheckState::Pending
    } else {
        PrCheckState::Passing
    }
}

const GITHUB_PULL_REQUEST_DETAIL_FIELDS: &str =
    "number,title,body,author,baseRefName,headRefName,headRefOid,createdAt,updatedAt,labels,url,reviewDecision,isDraft,statusCheckRollup,reviews,comments,files,commits,mergeable,mergeStateStatus,autoMergeRequest";

const GITHUB_TIMELINE_JQ: &str =
    ".[] | {event,created_at,submitted_at,authored_at,committed_at,author_date:.author.date,sha,message,state,body,actor:.actor.login,user:.user.login,author:(.author.login//.author.name),requested_reviewer:.requested_reviewer.login,label:.label.name,commit_id} | @json";

fn github_api_values(value: Value) -> Vec<Value> {
    fn flatten(value: Value, values: &mut Vec<Value>) {
        if let Value::Array(nested) = value {
            for value in nested {
                flatten(value, values);
            }
        } else {
            values.push(value);
        }
    }
    let mut values = Vec::new();
    flatten(value, &mut values);
    values
}

fn parse_github_json_lines(bytes: &[u8]) -> Result<Value, ()> {
    let text = std::str::from_utf8(bytes).map_err(|_| ())?;
    let values = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<Vec<Value>, _>>()
        .map_err(|_| ())?;
    Ok(Value::Array(values))
}

fn github_identity(value: &Value, key: &str) -> Option<String> {
    value_text(value.get(key))
        .or_else(|| nested_text(value.get(key), "login"))
        .or_else(|| nested_text(value.get(key), "name"))
}

fn github_login(value: &Value) -> Option<String> {
    ["actor", "user", "author"]
        .into_iter()
        .find_map(|key| github_identity(value, key))
}

fn first_line(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .and_then(|text| text.lines().next())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn short_hash(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(|hash| hash.chars().take(7).collect())
}

pub(crate) fn parse_github_timeline(value: Value) -> Vec<WorkItemTimelineEvent> {
    let mut events = github_api_values(value)
        .into_iter()
        .filter_map(|value| {
            let kind = value.get("event")?.as_str()?.to_string();
            let summary = match kind.as_str() {
                "committed" => first_line(value.get("message"))
                    .or_else(|| short_hash(value.get("sha")))
                    .unwrap_or_else(|| "commit".into()),
                "reviewed" => value
                    .get("state")
                    .and_then(Value::as_str)
                    .map(|state| format!("review {state}"))
                    .unwrap_or_else(|| "review".into()),
                "review_requested" => github_identity(&value, "requested_reviewer")
                    .map(|login| format!("review requested from {login}"))
                    .unwrap_or_else(|| "review requested".into()),
                "labeled" | "unlabeled" => value_text(value.get("label"))
                    .or_else(|| nested_text(value.get("label"), "name"))
                    .map(|label| format!("{kind} {label}"))
                    .unwrap_or_else(|| kind.clone()),
                "merged" => short_hash(value.get("commit_id"))
                    .map(|hash| format!("merged {hash}"))
                    .unwrap_or_else(|| "merged".into()),
                "closed" | "reopened" => kind.clone(),
                "commented" => first_line(value.get("body")).unwrap_or_else(|| "commented".into()),
                _ => return None,
            };
            let created_at = [
                "created_at",
                "submitted_at",
                "authored_at",
                "committed_at",
                "author_date",
            ]
            .into_iter()
            .find_map(|key| value.get(key).and_then(Value::as_str).map(str::to_owned))
            .or_else(|| nested_text(value.get("author"), "date"))
            .as_deref()
            .and_then(parse_rfc3339_system_time);
            Some(WorkItemTimelineEvent {
                kind,
                actor: github_login(&value),
                summary,
                created_at,
            })
        })
        .collect::<Vec<_>>();
    events.sort_by_key(|event| std::cmp::Reverse(event.created_at));
    events
}

fn fetch_github_timeline(
    repo: &str,
    number: u64,
    program: &Path,
    deadline: Instant,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> Result<Vec<WorkItemTimelineEvent>, RefreshError> {
    let mut command = crate::noninteractive_process::command(program);
    let endpoint = format!("repos/{repo}/issues/{number}/timeline");
    command.args(["api", &endpoint, "--paginate", "--jq", GITHUB_TIMELINE_JQ]);
    let output = run_provider_command(
        cache,
        WorkIndexSource::Github,
        command,
        WORK_ITEM_DETAIL_PROVIDER_CACHE_TTL,
        bypass,
        deadline,
    )
    .map_err(|error| {
        if error.kind() == io::ErrorKind::TimedOut {
            RefreshError::TimedOut
        } else {
            RefreshError::Failed(format!("GitHub timeline observation failed: {error}"))
        }
    })?;
    if !output.status.success() {
        return Err(RefreshError::Failed(exit_detail(
            "GitHub timeline observation",
            &output,
        )));
    }
    let value = parse_github_json_lines(&output.stdout).map_err(|_| {
        RefreshError::Failed("GitHub timeline observation returned invalid JSON".into())
    })?;
    Ok(parse_github_timeline(value))
}

pub(crate) fn fetch_github_collaborators(
    repo: &str,
    program: &Path,
    deadline: Instant,
) -> Result<Vec<String>, String> {
    let mut command = crate::noninteractive_process::command(program);
    let endpoint = format!("repos/{repo}/collaborators");
    command.args([
        "api",
        &endpoint,
        "--paginate",
        "--jq",
        ".[] | {login} | @json",
    ]);
    let output = run_provider_command(
        None,
        WorkIndexSource::Github,
        command,
        WORK_ITEM_DETAIL_PROVIDER_CACHE_TTL,
        true,
        deadline,
    )
    .map_err(|error| {
        if error.kind() == io::ErrorKind::TimedOut {
            "GitHub collaborators observation timed out".to_string()
        } else {
            format!("GitHub collaborators observation failed: {error}")
        }
    })?;
    if !output.status.success() {
        return Err(exit_detail("GitHub collaborators observation", &output));
    }
    let value = parse_github_json_lines(&output.stdout)
        .map_err(|_| "GitHub collaborators observation returned invalid JSON".to_string())?;
    let mut collaborators = github_api_values(value)
        .iter()
        .filter_map(|value| value_text(value.get("login")))
        .collect::<Vec<_>>();
    collaborators.sort_by_key(|login| login.to_ascii_lowercase());
    collaborators.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    Ok(collaborators)
}

fn fetch_github_pull_request_detail(
    repo: &str,
    number: u64,
    program: &Path,
    deadline: Instant,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> Result<WorkItemDetail, RefreshError> {
    let mut command = crate::noninteractive_process::command(program);
    let number_arg = number.to_string();
    command.args([
        "pr",
        "view",
        &number_arg,
        "--repo",
        repo,
        "--json",
        GITHUB_PULL_REQUEST_DETAIL_FIELDS,
    ]);
    let output = run_provider_command(
        cache,
        WorkIndexSource::Github,
        command,
        WORK_ITEM_DETAIL_PROVIDER_CACHE_TTL,
        bypass,
        deadline,
    )
    .map_err(|error| {
        if error.kind() == io::ErrorKind::TimedOut {
            RefreshError::TimedOut
        } else {
            RefreshError::Failed(format!("GitHub PR detail observation failed: {error}"))
        }
    })?;
    if !output.status.success() {
        return Err(RefreshError::Failed(exit_detail(
            "GitHub PR detail observation",
            &output,
        )));
    }
    let value = serde_json::from_slice::<Value>(&output.stdout).map_err(|_| {
        RefreshError::Failed("GitHub PR detail observation returned invalid JSON".into())
    })?;
    let (timeline, timeline_unavailable) =
        match fetch_github_timeline(repo, number, program, deadline, cache, bypass) {
            Ok(timeline) => (timeline, None),
            Err(RefreshError::TimedOut) => (
                Vec::new(),
                Some("GitHub timeline observation timed out".into()),
            ),
            Err(RefreshError::Failed(message)) => (Vec::new(), Some(message)),
        };
    Ok(WorkItemDetail {
        number: value.get("number").and_then(Value::as_u64),
        title: value_text(value.get("title")),
        body: value_text(value.get("body")),
        author: value
            .get("author")
            .and_then(|author| author.get("login"))
            .and_then(Value::as_str)
            .map(str::to_string),
        base_ref_name: value_text(value.get("baseRefName")),
        head_ref_name: value_text(value.get("headRefName")),
        created_at: value
            .get("createdAt")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_system_time),
        updated_at: value
            .get("updatedAt")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_system_time),
        labels: value
            .get("labels")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|label| value_text(Some(label)))
            .collect(),
        url: value_text(value.get("url")),
        review_decision: value_text(value.get("reviewDecision")),
        is_draft: value.get("isDraft").and_then(Value::as_bool),
        reviewers: value
            .get("reviews")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|review| {
                review
                    .get("author")
                    .and_then(|author| author.get("login"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .fold(Vec::new(), |mut reviewers, reviewer| {
                if !reviewers.contains(&reviewer) {
                    reviewers.push(reviewer);
                }
                reviewers
            }),
        mergeable: value_text(value.get("mergeable")),
        merge_state_status: value_text(value.get("mergeStateStatus")),
        auto_merge_enabled: value
            .get("autoMergeRequest")
            .is_some_and(|request| !request.is_null()),
        head_sha: value_text(value.get("headRefOid")),
        checks: status_check_summary(value.get("statusCheckRollup")),
        comments: github_comments(value.get("comments")),
        actions: github_actions(value.get("statusCheckRollup")),
        files: github_files(value.get("files")),
        commits: github_commits(value.get("commits")),
        timeline,
        timeline_unavailable,
        collaborators: Vec::new(),
        collaborators_unavailable: None,
        unresolved_review_threads: fetch_unresolved_review_thread_count(
            repo, number, program, deadline, cache, bypass,
        ),
        unavailable: None,
        observed_at: SystemTime::now(),
    })
}

/// GraphQL query for a PR's review threads. `gh pr view --json` does not expose
/// these, so the detail path makes a second best-effort call and the fix action
/// can reuse it on demand.
const UNRESOLVED_REVIEW_THREADS_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { reviewThreads(first: 100) { nodes { isResolved comments(first: 20) { nodes { path line body author { login } } } } } } } }";

/// Count a pull request's unresolved review threads via `gh api graphql`.
/// Degrades to `None` on any failure (bad repo slug, non-zero exit, timeout,
/// malformed JSON) so the caller's detail fetch is never blocked by it.
fn fetch_unresolved_review_thread_count(
    repo: &str,
    number: u64,
    program: &Path,
    deadline: Instant,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> Option<usize> {
    fetch_unresolved_review_threads_with_cache(repo, number, program, deadline, cache, bypass)
        .map(|threads| threads.count)
}

/// On-demand entry for the `Fix findings` action: no host cache, always fresh,
/// so the prompt reflects the current thread state.
pub(crate) fn fetch_unresolved_review_threads(
    repo: &str,
    number: u64,
    program: &Path,
    deadline: Instant,
) -> Option<UnresolvedReviewThreads> {
    fetch_unresolved_review_threads_with_cache(repo, number, program, deadline, None, true)
}

fn fetch_unresolved_review_threads_with_cache(
    repo: &str,
    number: u64,
    program: &Path,
    deadline: Instant,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> Option<UnresolvedReviewThreads> {
    let (owner, name) = repo.split_once('/')?;
    let mut command = crate::noninteractive_process::command(program);
    command.args([
        "api",
        "graphql",
        "-f",
        &format!("query={UNRESOLVED_REVIEW_THREADS_QUERY}"),
        "-F",
        &format!("owner={owner}"),
        "-F",
        &format!("name={name}"),
        "-F",
        &format!("number={number}"),
    ]);
    let output = run_provider_command(
        cache,
        WorkIndexSource::Github,
        command,
        WORK_ITEM_DETAIL_PROVIDER_CACHE_TTL,
        bypass,
        deadline,
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = serde_json::from_slice::<Value>(&output.stdout).ok()?;
    parse_unresolved_review_threads(&value)
}

fn parse_unresolved_review_threads(value: &Value) -> Option<UnresolvedReviewThreads> {
    let nodes = value
        .get("data")?
        .get("repository")?
        .get("pullRequest")?
        .get("reviewThreads")?
        .get("nodes")?
        .as_array()?;
    let unresolved = nodes
        .iter()
        .filter(|node| node.get("isResolved").and_then(Value::as_bool) == Some(false))
        .collect::<Vec<_>>();
    let comments = unresolved
        .iter()
        .flat_map(|thread| {
            thread
                .get("comments")
                .and_then(|comments| comments.get("nodes"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|comment| {
            Some(ReviewThreadComment {
                path: comment.get("path")?.as_str()?.to_string(),
                line: comment.get("line").and_then(Value::as_u64),
                body: comment.get("body")?.as_str()?.to_string(),
                author: comment
                    .get("author")
                    .and_then(|author| author.get("login"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
            })
        })
        .collect();
    Some(UnresolvedReviewThreads {
        count: unresolved.len(),
        comments,
    })
}

/// Read one Linear issue with its comment threads.
///
/// Reuses `WorkItemDetail` so the LRU cache, the loading set and the generation
/// guard that already serve pull requests apply to tickets unchanged.
fn fetch_linear_ticket_detail(
    identifier: &str,
    program: &Path,
    deadline: Instant,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> Result<WorkItemDetail, RefreshError> {
    let run = |args: &[&str]| {
        let mut command = crate::noninteractive_process::command(program);
        command.args(args);
        run_provider_command(
            cache,
            WorkIndexSource::Linear,
            command,
            WORK_ITEM_DETAIL_PROVIDER_CACHE_TTL,
            bypass,
            deadline,
        )
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::TimedOut {
                RefreshError::TimedOut
            } else {
                RefreshError::Failed(format!("linearis could not be run ({})", program.display()))
            }
        })
    };
    // Newer linearis builds expose `issues get`; keep the installed `read`
    // spelling as a compatibility fallback until every host has upgraded.
    let mut output = run(&["issues", "get", identifier])?;
    if !output.status.success() {
        output = run(&[
            "issues",
            "read",
            identifier,
            "--with-comment-threads",
            "--compact",
        ])?;
    }
    if !output.status.success() {
        return Err(RefreshError::Failed(format!(
            "linearis issues read failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| RefreshError::Failed("linearis returned invalid JSON".to_string()))?;

    let mut detail = WorkItemDetail::empty();
    detail.title = value_text(value.get("title"));
    detail.body = value_text(value.get("description"));
    detail.url = value_text(value.get("url")).or_else(|| linear_ticket_url(identifier));
    detail.created_at = value_time(value.get("createdAt"));
    detail.updated_at = value_time(value.get("updatedAt"));
    detail.comments = linear_comments(value.get("comments"));
    Ok(detail)
}

/// Flatten Linear's comment threads newest thread first: a root comment
/// followed by its replies, in the order they were written, so a thread still
/// reads top to bottom while the list leads with the most recent discussion.
fn linear_comments(value: Option<&Value>) -> Vec<WorkItemComment> {
    fn author(comment: &Value) -> Option<String> {
        let user = comment.get("user")?;
        user.get("displayName")
            .or_else(|| user.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }
    fn push(comment: &Value, out: &mut Vec<WorkItemComment>) {
        let Some(body) = value_text(comment.get("body")) else {
            return;
        };
        out.push(WorkItemComment {
            author: author(comment),
            body,
            created_at: value_time(comment.get("createdAt")),
        });
        let replies = comment
            .get("replies")
            .and_then(|replies| replies.get("nodes").or(Some(replies)))
            .and_then(Value::as_array);
        for reply in replies.into_iter().flatten() {
            push(reply, out);
        }
    }

    let nodes = value
        .and_then(|value| value.get("nodes").or(Some(value)))
        .and_then(Value::as_array);
    let mut threads = Vec::new();
    for comment in nodes.into_iter().flatten() {
        let mut thread = Vec::new();
        push(comment, &mut thread);
        if !thread.is_empty() {
            threads.push(thread);
        }
    }
    // Order whole threads, not individual comments: a reply belongs under the
    // root it answers even when a newer root exists. A thread is placed by its
    // latest activity, so answering an old thread lifts it back to the top
    // instead of leaving the newest words at the bottom of the list. A thread
    // with no timestamps at all sorts last rather than ahead of every dated one.
    threads.sort_by_key(|thread| {
        newest_first_key(thread.iter().filter_map(|comment| comment.created_at).max())
    });
    threads.into_iter().flatten().collect()
}

/// Sort key that puts the most recent item first and undated items last.
fn newest_first_key(
    created_at: Option<SystemTime>,
) -> (bool, std::cmp::Reverse<Option<SystemTime>>) {
    (created_at.is_none(), std::cmp::Reverse(created_at))
}

/// Newest first, so the detail views lead with the most recent review comment.
/// GitHub returns them oldest first.
fn github_comments(value: Option<&Value>) -> Vec<WorkItemComment> {
    let mut comments = value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|comment| {
            let body = value_text(comment.get("body"))?;
            Some(WorkItemComment {
                author: comment
                    .get("author")
                    .and_then(|author| author.get("login"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                body,
                created_at: value_time(comment.get("createdAt")),
            })
        })
        .collect::<Vec<_>>();
    comments.sort_by_key(|comment| newest_first_key(comment.created_at));
    comments
}

fn github_actions(value: Option<&Value>) -> Vec<WorkItemAction> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|action| {
            let name =
                value_text(action.get("name")).or_else(|| value_text(action.get("context")))?;
            let state = github_check_state(action)
                .map(str::to_string)
                .unwrap_or_else(|| "unknown".to_string());
            Some(WorkItemAction { name, state })
        })
        .collect()
}

fn github_check_state(check: &Value) -> Option<&str> {
    ["conclusion", "state", "status"]
        .into_iter()
        .find_map(|field| {
            check
                .get(field)
                .and_then(Value::as_str)
                .filter(|state| !state.is_empty())
        })
}

fn github_files(value: Option<&Value>) -> Vec<WorkItemFile> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|file| {
            Some(WorkItemFile {
                path: value_text(file.get("path"))?,
                additions: file.get("additions").and_then(Value::as_u64).unwrap_or(0),
                deletions: file.get("deletions").and_then(Value::as_u64).unwrap_or(0),
            })
        })
        .collect()
}

fn github_commits(value: Option<&Value>) -> Vec<WorkItemCommit> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|commit| {
            let oid = value_text(commit.get("oid"))?;
            Some(WorkItemCommit {
                short_id: oid.chars().take(7).collect(),
                subject: value_text(commit.get("messageHeadline")).unwrap_or_else(|| "—".into()),
            })
        })
        .collect()
}

fn status_check_summary(value: Option<&Value>) -> Option<WorkItemCheckSummary> {
    let rollup = value?.as_array()?;
    if rollup.is_empty() {
        return None;
    }
    let failing = rollup
        .iter()
        .filter(|check| {
            github_check_state(check).is_some_and(|state| {
                matches!(
                    state,
                    "ERROR" | "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED"
                )
            })
        })
        .count();
    Some(WorkItemCheckSummary {
        failing,
        total: rollup.len(),
    })
}

pub(crate) fn parse_rfc3339_system_time(value: &str) -> Option<SystemTime> {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let zone_index = bytes[19..]
        .iter()
        .position(|byte| matches!(byte, b'Z' | b'+' | b'-'))?
        + 19;
    if zone_index > 19
        && (bytes[19] != b'.'
            || zone_index == 20
            || !bytes[20..zone_index].iter().all(u8::is_ascii_digit))
    {
        return None;
    }
    let zone = bytes[zone_index];
    let offset_seconds = match zone {
        b'Z' if zone_index + 1 == bytes.len() => 0,
        b'+' | b'-'
            if zone_index + 6 == bytes.len() && bytes.get(zone_index + 3) == Some(&b':') =>
        {
            let hours = parse_decimal(bytes.get(zone_index + 1..zone_index + 3)?)?;
            let minutes = parse_decimal(bytes.get(zone_index + 4..zone_index + 6)?)?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            u64::from(hours) * 60 * 60 + u64::from(minutes) * 60
        }
        _ => return None,
    };
    let year = parse_decimal(bytes.get(0..4)?)?;
    let month = parse_decimal(bytes.get(5..7)?)?;
    let day = parse_decimal(bytes.get(8..10)?)?;
    let hour = parse_decimal(bytes.get(11..13)?)?;
    let minute = parse_decimal(bytes.get(14..16)?)?;
    let second = parse_decimal(bytes.get(17..19)?)?;
    let month_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => return None,
    };
    if year < 1970 || day == 0 || day > month_days || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let seconds = days_since_unix_epoch(year, month, day)
        .checked_mul(24 * 60 * 60)?
        .checked_add(u64::from(hour) * 60 * 60)?
        .checked_add(u64::from(minute) * 60)?
        .checked_add(u64::from(second))?;
    let utc_seconds = match zone {
        b'+' if offset_seconds > seconds => {
            return SystemTime::UNIX_EPOCH
                .checked_sub(Duration::from_secs(offset_seconds - seconds));
        }
        b'+' => seconds - offset_seconds,
        b'-' => seconds.checked_add(offset_seconds)?,
        _ => seconds,
    };
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(utc_seconds))
}

fn parse_decimal(bytes: &[u8]) -> Option<u32> {
    bytes.iter().try_fold(0_u32, |value, byte| {
        if byte.is_ascii_digit() {
            Some(value * 10 + u32::from(*byte - b'0'))
        } else {
            None
        }
    })
}

fn is_leap_year(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn days_since_unix_epoch(year: u32, month: u32, day: u32) -> u64 {
    let adjusted_year = i64::from(year) - i64::from(month <= 2);
    let era = if adjusted_year >= 0 {
        adjusted_year / 400
    } else {
        (adjusted_year - 399) / 400
    };
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    (era * 146_097 + day_of_era - 719_468) as u64
}

#[derive(Debug)]
struct LinearTicketObservation {
    tickets: Vec<LinearTicket>,
    group_failures: Vec<LinearGroupFailure>,
}

#[derive(Debug)]
struct LinearGroupFailure {
    group: TicketGroup,
    error: RefreshError,
}

impl LinearGroupFailure {
    fn summary(&self) -> String {
        let reason = match &self.error {
            RefreshError::TimedOut => "observation timed out",
            RefreshError::Failed(reason) => reason,
        };
        format!("{}: {reason}", self.group.label())
    }
}

#[derive(Debug)]
struct ActiveLinearCycle {
    id: String,
    name: String,
}

const LINEAR_LIST_FIELDS: &str = "nodes.identifier,nodes.title,nodes.state.name,nodes.priority,nodes.assignee.name,nodes.creator.id,nodes.creator.name,nodes.labels.nodes.name,nodes.cycle.name,nodes.updatedAt,nodes.url";
const LINEAR_ITEM_FIELDS: &str =
    "identifier,title,description,state.name,priority,assignee.name,creator.id,creator.name,labels.nodes.name,cycle.name,updatedAt,url";

fn fetch_linear_tickets(
    team: &str,
    assignee: Option<&str>,
    program: &Path,
    deadline: Instant,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> Result<LinearTicketObservation, RefreshError> {
    let mut tickets = Vec::new();
    let mut group_failures = Vec::new();
    let assignee_filter = assignee
        .map(|assignee| vec!["--assignee", assignee])
        .unwrap_or_default();
    tickets.extend(fetch_linear_ticket_group(
        team,
        program,
        deadline,
        TicketGroup::Assigned,
        &assignee_filter,
        true,
        cache,
        bypass,
    )?);
    match fetch_linear_ticket_group(
        team,
        program,
        deadline,
        TicketGroup::Triage,
        &["--status", "Triage"],
        true,
        cache,
        bypass,
    ) {
        Ok(group) => tickets.extend(group),
        Err(error) => group_failures.push(LinearGroupFailure {
            group: TicketGroup::Triage,
            error,
        }),
    }
    match fetch_active_linear_cycle(team, program, deadline, cache, bypass) {
        Ok(Some(cycle)) => match fetch_linear_ticket_group(
            team,
            program,
            deadline,
            TicketGroup::DoneThisCycle,
            &["--cycle", &cycle.id, "--status", "Done"],
            false,
            cache,
            bypass,
        ) {
            Ok(mut group) => {
                for ticket in &mut group {
                    ticket.cycle.get_or_insert_with(|| cycle.name.clone());
                }
                tickets.extend(group);
            }
            Err(error) => group_failures.push(LinearGroupFailure {
                group: TicketGroup::DoneThisCycle,
                error,
            }),
        },
        Ok(None) => {}
        Err(error) => group_failures.push(LinearGroupFailure {
            group: TicketGroup::DoneThisCycle,
            error,
        }),
    }

    let mut deduplicated: Vec<LinearTicket> = Vec::new();
    for ticket in tickets {
        if let Some(existing) = deduplicated
            .iter_mut()
            .find(|existing| existing.identifier == ticket.identifier)
        {
            if ticket_group_rank(ticket.group) > ticket_group_rank(existing.group) {
                *existing = ticket;
            }
        } else {
            deduplicated.push(ticket);
        }
    }
    Ok(LinearTicketObservation {
        tickets: deduplicated,
        group_failures,
    })
}

fn parse_linear_ticket(value: &Value, group: TicketGroup) -> Option<LinearTicket> {
    let identifier = normalize_ticket_id(value.get("identifier")?.as_str()?).ok()?;
    Some(WorkTicket {
        url: value_text(value.get("url")).or_else(|| linear_ticket_url(&identifier)),
        identifier,
        title: value_text(value.get("title")),
        description: value_text(value.get("description")),
        state: nested_text(value.get("state"), "name"),
        assignee: nested_text(value.get("assignee"), "name"),
        creator: value.get("creator").and_then(|creator| {
            Some(WorkPerson {
                id: nested_text(Some(creator), "id")?,
                name: nested_text(Some(creator), "name")?,
            })
        }),
        priority: value
            .get("priority")
            .and_then(Value::as_u64)
            .and_then(|priority| u8::try_from(priority).ok()),
        cycle: nested_text(value.get("cycle"), "name"),
        group,
        created_at: value
            .get("createdAt")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_system_time),
        updated_at: value
            .get("updatedAt")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_system_time),
        branch: value_text(value.get("branchName")),
        labels: value
            .get("labels")
            .and_then(|labels| labels.get("nodes"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|label| nested_text(Some(label), "name"))
            .collect(),
        parent: value.get("parent").and_then(format_linear_reference),
        relations: format_linear_relations(value),
    })
}

fn fetch_linear_ticket(
    identifier: &str,
    program: &Path,
    deadline: Instant,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> Result<LinearTicket, RefreshError> {
    let mut command = crate::noninteractive_process::command(program);
    command.args([
        "--compact",
        "--fields",
        LINEAR_ITEM_FIELDS,
        "issues",
        "read",
        identifier,
    ]);
    let output = run_provider_command(
        cache,
        WorkIndexSource::Linear,
        command,
        WORK_ITEM_DETAIL_PROVIDER_CACHE_TTL,
        bypass,
        deadline,
    )
    .map_err(|error| {
        if error.kind() == io::ErrorKind::TimedOut {
            RefreshError::TimedOut
        } else {
            RefreshError::Failed(format!("Linear ticket observation failed: {error}"))
        }
    })?;
    if !output.status.success() {
        return Err(RefreshError::Failed(exit_detail(
            "Linear ticket observation",
            &output,
        )));
    }
    let value = serde_json::from_slice::<Value>(&output.stdout).map_err(|_| {
        RefreshError::Failed("Linear ticket observation returned invalid JSON".into())
    })?;
    parse_linear_ticket(&value, TicketGroup::Assigned).ok_or_else(|| {
        RefreshError::Failed("Linear ticket observation returned an invalid item".into())
    })
}

fn ticket_group_rank(group: TicketGroup) -> u8 {
    match group {
        TicketGroup::Assigned => 0,
        TicketGroup::Triage => 1,
        TicketGroup::DoneThisCycle => 2,
    }
}

fn fetch_active_linear_cycle(
    team: &str,
    program: &Path,
    deadline: Instant,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> Result<Option<ActiveLinearCycle>, RefreshError> {
    let mut command = crate::noninteractive_process::command(program);
    command.args([
        "--compact",
        "--fields",
        "nodes.id,nodes.name",
        "cycles",
        "list",
        "--team",
        team,
        "--active",
    ]);
    let output = run_provider_command(
        cache,
        WorkIndexSource::Linear,
        command,
        LINEAR_CYCLE_CACHE_TTL,
        bypass,
        deadline,
    )
    .map_err(|error| {
        if error.kind() == io::ErrorKind::TimedOut {
            RefreshError::TimedOut
        } else {
            RefreshError::Failed(format!("Linear cycle observation failed: {error}"))
        }
    })?;
    if !output.status.success() {
        return Err(RefreshError::Failed(exit_detail(
            "Linear cycle observation",
            &output,
        )));
    }
    let value = serde_json::from_slice::<Value>(&output.stdout).map_err(|_| {
        RefreshError::Failed("Linear cycle observation returned invalid JSON".into())
    })?;
    let Some(cycle) = value
        .get("nodes")
        .and_then(Value::as_array)
        .and_then(|nodes| nodes.first())
    else {
        return Ok(None);
    };
    let id = value_text(cycle.get("id")).ok_or_else(|| {
        RefreshError::Failed("Linear cycle observation returned no cycle id".into())
    })?;
    let name = value_text(cycle.get("name")).ok_or_else(|| {
        RefreshError::Failed("Linear cycle observation returned no cycle name".into())
    })?;
    Ok(Some(ActiveLinearCycle { id, name }))
}

fn fetch_linear_ticket_group(
    team: &str,
    program: &Path,
    deadline: Instant,
    group: TicketGroup,
    filters: &[&str],
    incremental: bool,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> Result<Vec<LinearTicket>, RefreshError> {
    let mut argv = vec![
        "--compact".to_string(),
        "--fields".to_string(),
        LINEAR_LIST_FIELDS.to_string(),
        "issues".to_string(),
        "list".to_string(),
        "--team".to_string(),
        team.to_string(),
    ];
    argv.extend(filters.iter().map(|filter| (*filter).to_string()));
    argv.extend(["-l".to_string(), "100".to_string()]);

    let mut baseline = Vec::new();
    let mut request_argv = argv.clone();
    if incremental && !bypass {
        if let Some(cached) = cache.and_then(|cache| cache.cached(Provider::Linear, &argv)) {
            let observed_now = cache.map_or_else(SystemTime::now, ProviderCache::current_time);
            let age = observed_now.duration_since(cached.fetched_at).ok();
            if age.is_some_and(|age| age < LINEAR_FULL_LIST_INTERVAL) {
                baseline = parse_linear_ticket_group_output(&cached.stdout, group)?;
                request_argv.push("--created-after".to_string());
                request_argv.push(utc_date(cached.fetched_at));
            }
        }
    }

    let mut command = crate::noninteractive_process::command(program);
    command.args(&request_argv);
    let output = run_provider_command(
        cache,
        WorkIndexSource::Linear,
        command,
        LINEAR_LIST_CACHE_TTL,
        bypass,
        deadline,
    )
    .map_err(|error| {
        if error.kind() == io::ErrorKind::TimedOut {
            RefreshError::TimedOut
        } else {
            RefreshError::Failed(format!("Linear observation failed: {error}"))
        }
    })?;
    if !output.status.success() {
        return Err(RefreshError::Failed(exit_detail(
            "Linear observation",
            &output,
        )));
    }
    let incremental = parse_linear_ticket_group_output(&output.stdout, group)?;
    for ticket in incremental {
        if let Some(existing) = baseline
            .iter_mut()
            .find(|existing| existing.identifier == ticket.identifier)
        {
            *existing = ticket;
        } else {
            baseline.push(ticket);
        }
    }
    Ok(baseline)
}

fn parse_linear_ticket_group_output(
    stdout: &[u8],
    group: TicketGroup,
) -> Result<Vec<LinearTicket>, RefreshError> {
    let value = serde_json::from_slice::<Value>(stdout)
        .map_err(|_| RefreshError::Failed("Linear observation returned invalid JSON".into()))?;
    let Some(nodes) = value.get("nodes").and_then(Value::as_array) else {
        return Err(RefreshError::Failed(
            "Linear observation returned no nodes".into(),
        ));
    };
    Ok(nodes
        .iter()
        .filter_map(|node| parse_linear_ticket(node, group))
        .collect())
}

fn utc_date(time: SystemTime) -> String {
    let days = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86_400;
    let z = i64::try_from(days)
        .unwrap_or(i64::MAX)
        .saturating_add(719_468);
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn nested_text(value: Option<&Value>, field: &str) -> Option<String> {
    value_text(value.and_then(|value| value.get(field))).or_else(|| value_text(value))
}

fn format_linear_reference(value: &Value) -> Option<String> {
    let identifier = nested_text(Some(value), "identifier");
    let title = nested_text(Some(value), "title");
    match (identifier, title) {
        (Some(identifier), Some(title)) => Some(format!("{identifier}  {title}")),
        (Some(identifier), None) => Some(identifier),
        (None, Some(title)) => Some(title),
        (None, None) => value_text(Some(value)),
    }
}

fn format_linear_relations(value: &Value) -> Vec<String> {
    let forward = value
        .get("relations")
        .and_then(|relations| relations.get("nodes"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|relation| format_linear_relation(relation, false));
    let inverse = value
        .get("inverseRelations")
        .and_then(|relations| relations.get("nodes"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|relation| format_linear_relation(relation, true));
    forward.chain(inverse).collect()
}

fn format_linear_relation(value: &Value, inverse: bool) -> Option<String> {
    let kind = nested_text(Some(value), "type").map(|kind| match (inverse, kind.as_str()) {
        (true, "blocks") => "blocked by".to_string(),
        (true, "blocked by") => "blocks".to_string(),
        (true, "duplicate" | "duplicate of") => "duplicated by".to_string(),
        (false, "duplicate") => "duplicate of".to_string(),
        _ => kind,
    });
    let related = value
        .get("relatedIssue")
        .or_else(|| value.get("issue"))
        .and_then(format_linear_reference)
        .or_else(|| format_linear_reference(value));
    match (kind, related) {
        (Some(kind), Some(related)) => Some(format!("{kind}  {related}")),
        (None, Some(related)) => Some(related),
        _ => None,
    }
}

fn fetch_attachments(
    tickets: &[LinearTicket],
    program: &Path,
    batch_deadline: Instant,
    target_timeout: Duration,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> Vec<Attachment> {
    let mut results = Vec::new();
    for chunk in tickets.chunks(8) {
        std::thread::scope(|scope| {
            let handles = chunk
                .iter()
                .map(|ticket| {
                    let program = program.to_path_buf();
                    scope.spawn(move || {
                        fetch_ticket_attachments(
                            ticket,
                            &program,
                            target_deadline(batch_deadline, target_timeout),
                            cache,
                            bypass,
                        )
                    })
                })
                .collect::<Vec<_>>();
            for handle in handles {
                if let Ok(Ok(mut attachments)) = handle.join() {
                    results.append(&mut attachments);
                }
            }
        });
        if Instant::now() >= batch_deadline {
            break;
        }
    }
    results
}

fn fetch_ticket_attachments(
    ticket: &LinearTicket,
    program: &Path,
    deadline: Instant,
    cache: Option<&ProviderCache>,
    bypass: bool,
) -> Result<Vec<Attachment>, RefreshError> {
    let mut command = crate::noninteractive_process::command(program);
    command.args([
        "attachments",
        "list",
        &ticket.identifier,
        "--source-type",
        "github",
        "--compact",
    ]);
    let output = run_provider_command(
        cache,
        WorkIndexSource::Linear,
        command,
        LINEAR_LIST_CACHE_TTL,
        bypass,
        deadline,
    )
    .map_err(|error| {
        if error.kind() == io::ErrorKind::TimedOut {
            RefreshError::TimedOut
        } else {
            RefreshError::Failed(error.to_string())
        }
    })?;
    if !output.status.success() {
        return Err(RefreshError::Failed(exit_detail(
            "attachment observation",
            &output,
        )));
    }
    let values = serde_json::from_slice::<Vec<Value>>(&output.stdout)
        .map_err(|_| RefreshError::Failed("attachment observation returned invalid JSON".into()))?;
    Ok(values
        .into_iter()
        .filter_map(|value| {
            let metadata = value.get("metadata")?;
            let number = metadata.get("number")?.as_u64()?;
            let url = value.get("url")?.as_str()?.to_string();
            let repo = match (
                metadata.get("repoLogin").and_then(Value::as_str),
                metadata.get("repoName").and_then(Value::as_str),
            ) {
                (Some(owner), Some(name)) => {
                    normalize_repo_slug(&format!("{owner}/{name}")).ok()?
                }
                _ => repo_slug_from_pr_url(&url)?,
            };
            Some(Attachment {
                ticket_id: ticket.identifier.clone(),
                repo,
                number,
                url,
                title: value_text(metadata.get("title")),
                state: value_text(metadata.get("status")),
                draft: metadata
                    .get("draft")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                branch: value_text(metadata.get("branch")),
                preview_urls: metadata
                    .get("previewLinks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|link| link.get("url").and_then(Value::as_str).map(str::to_string))
                    .collect(),
            })
        })
        .collect())
}

fn value_time(value: Option<&Value>) -> Option<SystemTime> {
    value
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_system_time)
}

fn value_text(value: Option<&Value>) -> Option<String> {
    value.and_then(|value| {
        value.as_str().map(str::to_string).or_else(|| {
            value
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
    })
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn join_panes(items: &mut Vec<WorkItem>, panes: &[WorkIndexPane]) {
    for pane in panes {
        let pane_ticket_ids = pane
            .work_context
            .ticket_ids
            .iter()
            .filter_map(|id| normalize_ticket_id(id).ok())
            .collect::<HashSet<_>>();
        let matched = items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let pr_match = item.pr_url.as_deref().is_some_and(|url| {
                    pane.work_context
                        .pr_urls
                        .iter()
                        .any(|pane_url| pane_url == url)
                });
                let ticket_match = item
                    .ticket_ids
                    .iter()
                    .any(|ticket| pane_ticket_ids.contains(ticket));
                let repo_match =
                    pane.work_context.repo.as_deref().is_none_or(|repo| {
                        item.repo.is_empty() || repo_slugs_match(repo, &item.repo)
                    });
                (pr_match || ticket_match)
                    .then_some(index)
                    .filter(|_| repo_match)
            })
            .collect::<Vec<_>>();
        let mut matched = matched;
        if matched.is_empty()
            && (!pane.work_context.pr_urls.is_empty()
                || !pane_ticket_ids.is_empty()
                || pane.work_context.repo.is_some())
        {
            let repo = pane
                .work_context
                .repo
                .as_deref()
                .and_then(|repo| normalize_repo_slug(repo).ok())
                .or_else(|| {
                    pane.work_context
                        .pr_urls
                        .iter()
                        .find_map(|url| repo_slug_from_pr_url(url))
                })
                .unwrap_or_default();
            items.push(WorkItem {
                repo,
                pr_number: None,
                pr_url: pane.work_context.pr_urls.first().cloned(),
                pr_title: None,
                pr_state: None,
                draft: false,
                review_decision: None,
                created_at: None,
                updated_at: None,
                additions: 0,
                deletions: 0,
                author: None,
                assignees: Vec::new(),
                labels: Vec::new(),
                check_state: PrCheckState::Unknown,
                audience: PrAudience::Unclassified,
                cached_pr_detail: None,
                ticket_ids: {
                    // Sorted so the snapshot is byte-stable across refreshes:
                    // it is consumed as JSON by ghx and diffed by hand.
                    let mut ids = pane_ticket_ids.into_iter().collect::<Vec<_>>();
                    ids.sort();
                    ids
                },
                ticket_title: None,
                ticket_state: None,
                ticket_details: Vec::new(),
                branch: pane.work_context.branch.clone(),
                preview_urls: pane.work_context.preview_urls.clone(),
                panes: Vec::new(),
                source: WorkItemSource {
                    pane: true,
                    ..WorkItemSource::default()
                },
            });
            matched.push(items.len() - 1);
        }
        for index in matched {
            let item = &mut items[index];
            item.source.pane = true;
            item.panes.push(WorkItemPane {
                pane_id: pane.pane_id.clone(),
                agent_label: pane
                    .display_agent
                    .clone()
                    .or_else(|| pane.agent.clone())
                    .or_else(|| pane.name.clone()),
                workspace_id: pane.workspace_id.clone(),
                tab_id: pane.tab_id.clone(),
                role: pane.work_context.role,
                active_owner: pane.work_context.active_owner,
                agent_status: pane.agent_status,
            });
        }
    }
}

fn work_index_snapshot_path_for(
    state_dir: &Path,
    session_name: Option<&str>,
) -> std::path::PathBuf {
    state_dir.join("work-index").join(format!(
        "{}.json",
        session_name.unwrap_or(crate::session::DEFAULT_SESSION_NAME)
    ))
}

pub(crate) fn work_index_snapshot_path() -> std::path::PathBuf {
    let session_name = crate::session::active_name();
    work_index_snapshot_path_for(&crate::config::state_dir(), session_name.as_deref())
}

pub(crate) fn remove_session_snapshot(session_name: &str) -> io::Result<()> {
    let path = work_index_snapshot_path_for(&crate::config::state_dir(), Some(session_name));
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn write_snapshot(path: &Path, snapshot: &Snapshot) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(snapshot).map_err(io::Error::other)?;
    std::fs::write(path, bytes)
}

/// Read back a snapshot persisted by [`write_snapshot`].
///
/// Cold start should show the last known work index rather than an empty
/// view until the first refresh completes, but the persisted file is best
/// effort: a missing or corrupt file (partial write, format change) must
/// fall back to `None` rather than panic or block startup.
#[cfg(any(not(test), unix))]
pub(crate) fn load_snapshot(path: &Path) -> Option<Snapshot> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Locate a CLI herdr shells out to.
///
/// The server does not always inherit an interactive shell's `PATH` — started
/// from launchd, a desktop launcher or a login-less session it gets a bare one
/// — so relying on the bare name means the tool silently "could not be run" on
/// exactly the machines where it is installed. Search `PATH` first, then the
/// usual install locations, and fall back to the bare name so the error still
/// names the program rather than a guessed path.
fn resolve_program(name: &str) -> std::path::PathBuf {
    let executable = |path: &Path| -> bool {
        std::fs::metadata(path)
            .map(|metadata| metadata.is_file())
            .unwrap_or(false)
    };

    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join(name);
            if executable(&candidate) {
                return candidate;
            }
        }
    }

    // Ordered by how these tools are actually installed here: user bin first,
    // then Homebrew, then a global npm prefix.
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(home) = home {
        for relative in ["bin", ".local/bin", ".npm-global/bin", ".bun/bin"] {
            candidates.push(home.join(relative).join(name));
        }
    }
    for absolute in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        candidates.push(Path::new(absolute).join(name));
    }
    for candidate in candidates {
        if executable(&candidate) {
            return candidate;
        }
    }
    Path::new(name).to_path_buf()
}

impl crate::app::App {
    pub(crate) fn request_sidebar_view_scan(&mut self, now: Instant) {
        self.request_git_work_context_refresh(now);
        self.next_work_index_refresh = now;
    }

    fn note_work_index_contexts(&mut self, panes: &[WorkIndexPane], now: Instant) -> bool {
        let fingerprint = panes
            .iter()
            .map(|pane| {
                (
                    pane.workspace_id.clone(),
                    pane.tab_id.clone(),
                    pane.pane_id.clone(),
                    pane.work_context.clone(),
                )
            })
            .collect::<WorkIndexContextFingerprint>();
        if fingerprint == self.work_index_context_fingerprint {
            return false;
        }
        self.work_index_context_fingerprint = fingerprint;
        self.next_work_index_refresh = now;
        true
    }

    pub(crate) fn work_index_gh_program(&self) -> std::path::PathBuf {
        #[cfg(test)]
        if let Some(program) = self.work_index_gh_program_override.as_ref() {
            return program.clone();
        }
        resolve_program("gh")
    }

    /// Provider cache rooted at the state dir. Tests point it at a fixture
    /// directory so cached fixture output never leaks between tests.
    fn work_index_provider_cache(&self) -> ProviderCache {
        #[cfg(test)]
        if let Some(root) = self.work_index_provider_cache_root_override.as_ref() {
            return ProviderCache::new(root);
        }
        ProviderCache::new(&crate::config::state_dir())
    }

    pub(crate) fn work_index_linearis_program(&self) -> std::path::PathBuf {
        #[cfg(test)]
        if let Some(program) = self.work_index_linearis_program_override.as_ref() {
            return program.clone();
        }
        resolve_program("linearis")
    }

    pub(crate) fn work_index_curl_program(&self) -> std::path::PathBuf {
        #[cfg(test)]
        if let Some(program) = self.work_index_curl_program_override.as_ref() {
            return program.clone();
        }
        resolve_program("curl")
    }

    pub(crate) fn work_index_refresh_deadline(&self) -> Option<Instant> {
        // A parked timer has a due time the scheduler will not act on; waking
        // for it would spin the headless loop. An attach or a requested
        // refresh unparks it on the next scheduler pass.
        (self.work_index_config.enabled && !self.work_index_timer_parked).then(|| {
            self.work_index_refresh_in_flight
                .as_ref()
                .map_or(self.next_work_index_refresh, |refresh| refresh.deadline)
        })
    }

    pub(crate) fn work_item_detail_refresh_deadline(&self) -> Option<Instant> {
        self.work_item_detail_refresh_in_flight
            .as_ref()
            .map(|refresh| refresh.deadline)
    }

    pub(crate) fn start_work_index_refresh_if_due(&mut self, now: Instant) {
        if !self.work_index_config.enabled {
            return;
        }
        let panes = self.collect_work_index_panes();
        self.note_work_index_contexts(&panes, now);
        if self
            .work_index_refresh_in_flight
            .as_ref()
            .is_some_and(|refresh| now >= refresh.deadline)
        {
            self.work_index_refresh_in_flight = None;
        }
        if self.work_index_refresh_in_flight.is_some() || now < self.next_work_index_refresh {
            return;
        }
        self.work_index_refresh_requested = false;
        self.next_work_index_refresh =
            now + Duration::from_secs(self.work_index_config.refresh_interval_seconds.max(1));
        self.last_work_index_refresh_generation =
            self.last_work_index_refresh_generation.wrapping_add(1);
        let generation = self.last_work_index_refresh_generation;
        let deadline = now + WORK_INDEX_BATCH_TIMEOUT;
        self.work_index_refresh_in_flight = Some(WorkIndexRefreshInFlight {
            generation,
            deadline,
        });
        if let Some(view) = self.state.work_view.as_mut() {
            view.refreshing = true;
        }
        let config = self.work_index_config.clone();
        let mut session_config = config.clone();
        session_config.repos = work_index_repos(&config, &panes);
        let selected_missive = self
            .state
            .work_view
            .as_ref()
            .and_then(|view| view.selected_missive.clone());
        let event_tx = self.event_tx.clone();
        let gh_program = self.work_index_gh_program();
        let linearis_program = self.work_index_linearis_program();
        let mut session = self.work_index_session.clone();
        let curl_program = self.work_index_curl_program();
        let missive = self.missive_config.clone();
        let session_missive_users =
            (!session.missive_users.is_empty()).then(|| session.missive_users.clone());
        let previous_snapshot = self.work_index_snapshot.clone();
        let day_store_root = self.day_store_root.clone();
        let selected_ticket = self
            .state
            .work_view
            .as_ref()
            .and_then(|view| view.selected.as_ref())
            .and_then(|key| key.ticket_id.clone())
            .or_else(|| {
                crate::ui::dock::linear::focused_ticket_key(&self.state)
                    .and_then(|key| key.ticket_id)
            });
        let cache_bypass = std::mem::take(&mut self.work_index_cache_bypass);
        let provider_cache = self.work_index_provider_cache();
        let _ = std::thread::Builder::new()
            .name("herdr-work-index".into())
            .spawn(move || {
                let day_links = day_store_root
                    .as_deref()
                    .map(crate::day::load)
                    .map(|board| board.links())
                    .unwrap_or_default();
                if !session.linear.resolved {
                    (session.linear, session.linear_query_identity) = fetch_linear_directory(
                        &linearis_program,
                        target_deadline(deadline, WORK_INDEX_TARGET_TIMEOUT),
                        Some(&provider_cache),
                        cache_bypass.linear,
                    );
                    session.linear.resolved = true;
                }
                let mut snapshot = refresh_work_index_with_missive(
                    &config,
                    &missive,
                    &panes,
                    WorkIndexRefreshContext {
                        selected_missive: selected_missive.as_deref(),
                        session_missive_users: session_missive_users.as_deref(),
                        previous: previous_snapshot.as_ref(),
                        linear_assignee: session.linear_query_identity.as_deref(),
                        selected_ticket: selected_ticket.as_deref(),
                        day_links: Some(&day_links),
                        provider_cache: Some(&provider_cache),
                        cache_bypass,
                    },
                    Instant::now(),
                    deadline,
                    WORK_INDEX_TARGET_TIMEOUT,
                    &gh_program,
                    &linearis_program,
                    &curl_program,
                );
                let session = resolve_work_index_session(
                    &session_config,
                    session,
                    &snapshot.missive_users,
                    deadline,
                    WORK_INDEX_TARGET_TIMEOUT,
                    &gh_program,
                    &linearis_program,
                    Some(&provider_cache),
                    cache_bypass,
                );
                finalize_provider_refresh(&mut snapshot, &provider_cache);
                let _ = event_tx.blocking_send(crate::events::AppEvent::WorkIndexRefreshed {
                    generation,
                    snapshot: Box::new(snapshot),
                    session,
                });
            });
    }

    pub(crate) fn handle_work_index_refreshed(
        &mut self,
        generation: u64,
        snapshot: Snapshot,
        session: WorkIndexSession,
    ) -> bool {
        if generation <= self.last_applied_work_index_refresh_generation
            || generation != self.last_work_index_refresh_generation
        {
            return false;
        }
        self.work_index_refresh_in_flight = None;
        self.last_applied_work_index_refresh_generation = generation;
        if let Err(error) = write_snapshot(&work_index_snapshot_path(), &snapshot) {
            tracing::warn!(error = %error, "failed to persist work index snapshot");
        }
        if let Some(work_view) = self.state.work_view.as_mut() {
            work_view.replace_snapshot(snapshot.clone());
            work_view.refreshing = false;
        }
        self.work_index_session = session.clone();
        self.state.work_index_session = session;
        self.state.work_index_snapshot = Some(snapshot.clone());
        self.work_index_snapshot = Some(snapshot);
        self.refresh_pane_settlement_at(Instant::now());
        self.invalidate_work_item_details();
        self.state.work_item_detail_cache =
            WorkItemDetailCache::from_snapshot(self.work_index_snapshot.as_ref());
        self.finish_sidebar_refresh_if_idle();
        true
    }

    fn invalidate_work_item_details(&mut self) {
        if let Some(refresh) = self.work_item_detail_refresh_in_flight.take() {
            for key in refresh.keys {
                self.state.work_item_detail_loading.remove(&key);
            }
        }
        self.last_work_item_detail_refresh_generation = self
            .last_work_item_detail_refresh_generation
            .wrapping_add(1);
        self.state.work_item_detail_cache.clear();
    }

    pub(crate) fn start_work_item_detail_refresh_if_due(
        &mut self,
        now: Instant,
        section: crate::app::state::DockHomeSection,
        selection: Option<crate::app::state::WorkItemKey>,
        detail_visible: bool,
    ) {
        if self
            .work_item_detail_refresh_in_flight
            .as_ref()
            .is_some_and(|refresh| now >= refresh.deadline)
        {
            if let Some(refresh) = self.work_item_detail_refresh_in_flight.take() {
                for key in refresh.keys {
                    self.state.work_item_detail_loading.remove(&key);
                }
            }
        }
        if !detail_visible {
            return;
        }
        if self.work_item_detail_refresh_in_flight.is_some() {
            return;
        }
        let mut keys = self.state.dock_home_keys_for_section(section);
        if let Some(selection) = selection.as_ref() {
            if let Some(index) = keys.iter().position(|key| key == selection) {
                let selection = keys.remove(index);
                keys.insert(0, selection);
            } else {
                keys.insert(0, selection.clone());
            }
        }
        if section == crate::app::state::DockHomeSection::Prs {
            keys.truncate(1);
        } else {
            keys.truncate(WORK_ITEM_DETAIL_CACHE_CAPACITY);
        }
        let configured_interval =
            Duration::from_secs(self.work_index_config.refresh_interval_seconds.max(1));
        keys.retain(|key| {
            // Detail follows the selected object. The list refresh stays lean,
            // and opening a large PR section never starts one process per row.
            let fetchable = if key.pr_number.is_some() {
                !key.repo.is_empty()
            } else {
                key.ticket_id.is_some() && selection.as_ref() == Some(key)
            };
            let interval = if key.pr_number.is_some() {
                PR_DETAIL_FOCUS_REFRESH_INTERVAL
            } else {
                configured_interval
            };
            fetchable
                && !self
                    .state
                    .work_item_detail_cache
                    .is_fresh(key, now, interval)
        });
        if keys.is_empty() {
            return;
        }

        self.last_work_item_detail_refresh_generation = self
            .last_work_item_detail_refresh_generation
            .wrapping_add(1);
        let generation = self.last_work_item_detail_refresh_generation;
        let deadline = now + WORK_INDEX_BATCH_TIMEOUT;
        self.work_item_detail_refresh_in_flight = Some(WorkItemDetailRefreshInFlight {
            keys: keys.clone(),
            generation,
            deadline,
        });
        self.state
            .work_item_detail_loading
            .extend(keys.iter().cloned());
        let event_tx = self.event_tx.clone();
        let gh_program = self.work_index_gh_program();
        let linearis_program = self.work_index_linearis_program();
        let provider_cache = self.work_index_provider_cache();
        // The selected PR is an explicit surface refresh (F18-2): its focus
        // fetch, already debounced to PR_DETAIL_FOCUS_REFRESH_INTERVAL,
        // bypasses the provider cache so CI and review state stay live.
        let bypass_key = selection.filter(|key| key.pr_number.is_some());
        let _ = std::thread::Builder::new()
            .name("herdr-work-item-details".into())
            .spawn(move || {
                let mut details = Vec::with_capacity(keys.len());
                for chunk in keys.chunks(8) {
                    std::thread::scope(|scope| {
                        let handles = chunk
                            .iter()
                            .map(|key| {
                                let key = key.clone();
                                let gh_program = gh_program.clone();
                                let linearis_program = linearis_program.clone();
                                let provider_cache = provider_cache.clone();
                                let bypass = bypass_key.as_ref() == Some(&key);
                                scope.spawn(move || {
                                    let target =
                                        target_deadline(deadline, WORK_INDEX_TARGET_TIMEOUT);
                                    let (result, what) = match (key.pr_number, &key.ticket_id) {
                                        (Some(number), _) => (
                                            fetch_github_pull_request_detail(
                                                &key.repo,
                                                number,
                                                &gh_program,
                                                target,
                                                Some(&provider_cache),
                                                bypass,
                                            ),
                                            "GitHub PR detail",
                                        ),
                                        (None, Some(ticket)) => (
                                            fetch_linear_ticket_detail(
                                                ticket,
                                                &linearis_program,
                                                target,
                                                Some(&provider_cache),
                                                false,
                                            ),
                                            "Linear ticket detail",
                                        ),
                                        (None, None) => (
                                            Err(RefreshError::Failed(
                                                "work item has neither a pull request nor a ticket"
                                                    .to_string(),
                                            )),
                                            "work item detail",
                                        ),
                                    };
                                    let detail = match result {
                                        Ok(detail) => detail,
                                        Err(RefreshError::TimedOut) => WorkItemDetail::unavailable(
                                            format!("{what} observation timed out"),
                                        ),
                                        Err(RefreshError::Failed(message)) => {
                                            WorkItemDetail::unavailable(message)
                                        }
                                    };
                                    (key, detail)
                                })
                            })
                            .collect::<Vec<_>>();
                        for handle in handles {
                            if let Ok(detail) = handle.join() {
                                details.push(detail);
                            }
                        }
                    });
                    if Instant::now() >= deadline {
                        break;
                    }
                }
                provider_cache.finish_refresh();
                let _ = event_tx.blocking_send(crate::events::AppEvent::WorkItemDetailRefreshed {
                    generation,
                    details,
                });
            });
    }

    pub(crate) fn handle_work_item_detail_refreshed(
        &mut self,
        generation: u64,
        details: Vec<(crate::app::state::WorkItemKey, WorkItemDetail)>,
    ) -> bool {
        let matches_in_flight = self
            .work_item_detail_refresh_in_flight
            .as_ref()
            .is_some_and(|refresh| {
                refresh.generation == generation
                    && refresh.keys.len() == details.len()
                    && refresh
                        .keys
                        .iter()
                        .zip(&details)
                        .all(|(expected, (actual, _))| expected == actual)
            });
        if !matches_in_flight
            || generation <= self.last_applied_work_item_detail_refresh_generation
            || generation != self.last_work_item_detail_refresh_generation
        {
            return false;
        }
        let refresh = self.work_item_detail_refresh_in_flight.take();
        self.last_applied_work_item_detail_refresh_generation = generation;
        if let Some(refresh) = refresh {
            for key in refresh.keys {
                self.state.work_item_detail_loading.remove(&key);
            }
        }
        let mut snapshot_changed = false;
        for (key, detail) in details {
            if detail.unavailable.is_none() {
                if let Some(snapshot) = self.work_index_snapshot.as_mut() {
                    snapshot_changed |= snapshot.cache_pr_detail(&key, &detail);
                }
                if let Some(snapshot) = self.state.work_index_snapshot.as_mut() {
                    snapshot.cache_pr_detail(&key, &detail);
                }
                if let Some(snapshot) = self
                    .state
                    .work_view
                    .as_mut()
                    .and_then(|view| view.snapshot.as_mut())
                {
                    snapshot.cache_pr_detail(&key, &detail);
                }
            }
            self.state.work_item_detail_cache.insert(key, detail);
        }
        if snapshot_changed {
            if let Some(snapshot) = self.work_index_snapshot.as_ref() {
                if let Err(error) = write_snapshot(&work_index_snapshot_path(), snapshot) {
                    tracing::warn!(error = %error, "failed to persist work item detail");
                }
            }
        }
        true
    }
}

#[cfg(all(test, unix))]
mod tests {

    #[test]
    fn one_pull_request_spelled_two_ways_is_read_once() {
        let links = crate::day::DayLinks {
            tickets: Vec::new(),
            // The case GitHub rewrote, the case a person typed, and a trailing
            // slash a browser left behind. Every survivor costs a provider read.
            prs: vec![
                "https://github.com/acme/app/pull/9".into(),
                "https://github.com/ACME/App/pull/9".into(),
                "https://github.com/acme/app/pull/9/".into(),
                "https://github.com/acme/app/pull/90".into(),
            ],
        };

        let urls = super::direct_pr_urls(&[], Some(&links));

        assert_eq!(urls.len(), 2, "{urls:?}");
        assert!(urls.iter().any(|url| url.ends_with("/pull/90")), "{urls:?}");
    }

    #[test]
    fn resolve_program_finds_a_tool_on_path_and_names_it_when_missing() {
        let dir = std::env::temp_dir().join(format!("herdr-resolve-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let tool = dir.join("herdr-fake-tool");
        std::fs::write(&tool, "#!/bin/sh\nexit 0\n").expect("write fake tool");

        let previous = std::env::var_os("PATH");
        // SAFETY: single-threaded test, restored before returning.
        unsafe { std::env::set_var("PATH", &dir) };
        let found = resolve_program("herdr-fake-tool");
        let missing = resolve_program("herdr-tool-that-does-not-exist");
        match previous {
            Some(path) => unsafe { std::env::set_var("PATH", path) },
            None => unsafe { std::env::remove_var("PATH") },
        }

        assert_eq!(found, tool, "a tool on PATH resolves to its full path");
        assert_eq!(
            missing,
            Path::new("herdr-tool-that-does-not-exist"),
            "an unfound tool keeps its bare name so the error can name it"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unresolved_review_thread_count_counts_only_unresolved_nodes() {
        let value: Value = serde_json::from_str(
            r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
                 {"isResolved":false},
                 {"isResolved":true},
                 {"isResolved":false}
               ]}}}}}"#,
        )
        .expect("valid fixture");

        assert_eq!(
            super::parse_unresolved_review_threads(&value).map(|threads| threads.count),
            Some(2)
        );
    }

    #[test]
    fn unresolved_review_thread_count_is_zero_when_every_thread_is_resolved() {
        let value: Value = serde_json::from_str(
            r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
                 {"isResolved":true}
               ]}}}}}"#,
        )
        .expect("valid fixture");

        assert_eq!(
            super::parse_unresolved_review_threads(&value).map(|threads| threads.count),
            Some(0)
        );
    }

    #[test]
    fn unresolved_review_thread_count_is_none_on_a_malformed_response() {
        let value: Value = serde_json::from_str(r#"{"data":null,"errors":[{"message":"boom"}]}"#)
            .expect("valid fixture");

        assert_eq!(super::parse_unresolved_review_threads(&value), None);
    }

    #[test]
    fn linear_comment_threads_flatten_to_root_then_replies() {
        let value: Value = serde_json::from_str(
            r#"{"nodes":[
                 {"body":"root one","createdAt":"2026-09-01T10:00:00.000Z",
                  "user":{"displayName":"Ada"},
                  "replies":{"nodes":[{"body":"a reply","user":{"name":"Grace"}}]}},
                 {"body":"root two","user":{"displayName":"Alan"}}
               ]}"#,
        )
        .expect("fixture");

        let comments = linear_comments(Some(&value));
        let rendered: Vec<(Option<&str>, &str)> = comments
            .iter()
            .map(|comment| (comment.author.as_deref(), comment.body.as_str()))
            .collect();

        assert_eq!(
            rendered,
            vec![
                (Some("Ada"), "root one"),
                (Some("Grace"), "a reply"),
                (Some("Alan"), "root two"),
            ],
            "a thread reads root first, then its replies"
        );
        assert!(
            comments[0].created_at.is_some(),
            "a comment keeps the time it was written"
        );
        assert!(comments[1].created_at.is_none());
    }

    #[test]
    fn linear_comment_threads_lead_with_their_latest_activity() {
        let value: Value = serde_json::from_str(
            r#"{"nodes":[
                 {"body":"older root","createdAt":"2026-09-01T10:00:00.000Z",
                  "user":{"displayName":"Ada"},
                  "replies":{"nodes":[
                    {"body":"older reply","createdAt":"2026-09-04T10:00:00.000Z",
                     "user":{"name":"Grace"}}]}},
                 {"body":"undated root","user":{"displayName":"Alan"}},
                 {"body":"newer root","createdAt":"2026-09-03T10:00:00.000Z",
                  "user":{"displayName":"Edsger"}}
               ]}"#,
        )
        .expect("fixture");

        let comments = linear_comments(Some(&value));
        let bodies: Vec<&str> = comments
            .iter()
            .map(|comment| comment.body.as_str())
            .collect();

        assert_eq!(
            bodies,
            vec!["older root", "older reply", "newer root", "undated root"],
            "a reply lifts its whole thread above a newer but quiet one, the \
             reply stays under the root it answers, and an undated thread \
             sorts last"
        );
    }

    #[test]
    fn github_comments_lead_with_the_newest() {
        let value: Value = serde_json::from_str(
            r#"[
                 {"body":"first","createdAt":"2026-09-01T10:00:00Z","author":{"login":"ada"}},
                 {"body":"undated","author":{"login":"alan"}},
                 {"body":"second","createdAt":"2026-09-03T10:00:00Z","author":{"login":"grace"}}
               ]"#,
        )
        .expect("fixture");

        let comments = github_comments(Some(&value));
        let bodies: Vec<&str> = comments
            .iter()
            .map(|comment| comment.body.as_str())
            .collect();

        assert_eq!(
            bodies,
            vec!["second", "first", "undated"],
            "GitHub returns comments oldest first; the detail views promise newest first"
        );
    }

    #[test]
    fn linear_comments_tolerate_an_absent_or_empty_field() {
        assert!(linear_comments(None).is_empty());
        let empty: Value = serde_json::from_str(r#"{"nodes":[]}"#).expect("fixture");
        assert!(linear_comments(Some(&empty)).is_empty());
    }

    #[test]
    fn missive_client_request_catalog_is_get_only() {
        let requests = [
            MissiveRequest::Conversations {
                team: "team".into(),
            },
            MissiveRequest::Conversation {
                id: "conversation".into(),
            },
            MissiveRequest::ConversationMessages {
                id: "conversation".into(),
            },
            MissiveRequest::Message {
                id: "message".into(),
            },
            MissiveRequest::ConversationDrafts {
                id: "conversation".into(),
            },
            MissiveRequest::ConversationPosts {
                id: "conversation".into(),
            },
            MissiveRequest::ConversationNotes {
                id: "conversation".into(),
            },
            MissiveRequest::Users {
                organization: Some("organization".into()),
            },
        ];
        assert!(requests.iter().all(|request| request.method() == "GET"));
        assert!(requests
            .iter()
            .all(|request| request.url().starts_with(MISSIVE_API_BASE)));
        assert_eq!(
            requests[0].url(),
            "https://public.missiveapp.com/v1/conversations?team_all=team"
        );
    }

    #[test]
    fn missive_fixture_parsers_cover_conversations_messages_drafts_and_posts() {
        let conversations: Value =
            serde_json::from_str(include_str!("../tests/fixtures/missive/conversations.json"))
                .expect("conversation fixture");
        let messages: Value =
            serde_json::from_str(include_str!("../tests/fixtures/missive/messages.json"))
                .expect("message fixture");
        let drafts: Value =
            serde_json::from_str(include_str!("../tests/fixtures/missive/drafts.json"))
                .expect("draft fixture");
        let posts: Value =
            serde_json::from_str(include_str!("../tests/fixtures/missive/posts.json"))
                .expect("post fixture");

        let parsed = parse_missive_conversations_for_user(&conversations, Some("user-1"));
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].subject, "Billing question from fixture");
        assert_eq!(
            parsed[0].team,
            Some(MissiveTeam {
                id: "team-support".into(),
                name: "Support".into(),
                organization: Some("organization-example".into()),
            })
        );
        assert_eq!(parsed[0].labels, ["billing", "customer"]);
        assert_eq!(parsed[0].assignees[0].name, "Ada Example");
        assert!(!parsed[0].assignees[0].is_me);
        assert_eq!(
            parsed[0].app_url,
            "missive://mail.missiveapp.com/#inbox/conversations/11111111-1111-4111-8111-111111111111"
        );
        assert_eq!(
            parsed[0].web_url,
            "https://mail.missiveapp.com/#inbox/conversations/11111111-1111-4111-8111-111111111111"
        );
        assert!(!parsed[0].closed);
        assert!(parsed[1].closed);
        assert_eq!(parsed[1].subject, "Archived fixture conversation");
        assert_eq!(parse_missive_entries(&messages, "messages").len(), 2);
        assert_eq!(
            parse_missive_entries(&messages, "messages")[1].preview,
            "I am checking that now."
        );
        assert_eq!(parse_missive_entries(&drafts, "drafts").len(), 1);
        assert_eq!(
            parse_missive_entries(&posts, "posts")[0].preview,
            "Invoice lookup completed."
        );
        assert_eq!(parse_missive_entries(&posts, "comments").len(), 1);

        let conversation_response = serde_json::json!({
            "conversations": [conversations["conversations"][0].clone()]
        });
        assert_eq!(
            missive_resource(&conversation_response, "conversations")
                .and_then(|value| parse_missive_conversation_for_user(value, None))
                .map(|conversation| conversation.id),
            Some("11111111-1111-4111-8111-111111111111".into())
        );
        let message_response = serde_json::json!({
            "messages": messages["messages"][0].clone()
        });
        assert_eq!(
            missive_resource(&message_response, "messages")
                .and_then(missive_entry)
                .map(|message| message.preview),
            Some("Could you clarify the latest invoice?".into())
        );
    }

    #[test]
    fn missive_selected_conversation_hydrates_every_read_only_detail() {
        let dir = fixture_dir("missive-selected-detail");
        let curl = dir.join("curl");
        write_executable(
            &curl,
            r#"#!/bin/sh
case "$*" in
  *"--request GET"*) ;;
  *) exit 64 ;;
esac
for argument do url="$argument"; done
case "$url" in
  *"/users?organization=org") printf '%s' '{"users":[{"id":"me","name":"Ada","me":true}]}' ;;
  *"/conversations?team_all=team") printf '%s' '{"conversations":[{"id":"first","subject":"First","app_url":"missive://first","web_url":"https://mail.missiveapp.com/#inbox/conversations/first"},{"id":"selected","subject":"Selected","app_url":"missive://selected","web_url":"https://mail.missiveapp.com/#inbox/conversations/selected"}]}' ;;
  *"/conversations/selected/messages") printf '%s' '{"messages":[{"id":"message-1","preview":"list preview"}]}' ;;
  *"/messages/message-1") printf '%s' '{"messages":{"id":"message-1","preview":"hydrated message"}}' ;;
  *"/conversations/selected/drafts") printf '%s' '{"drafts":[{"id":"draft-1","preview":"draft"}]}' ;;
  *"/conversations/selected/posts") printf '%s' '{"posts":[{"id":"post-1","preview":"post"}]}' ;;
  *"/conversations/selected/comments") printf '%s' '{"comments":[{"id":"note-1","preview":"note"}]}' ;;
  *"/conversations/selected") printf '%s' '{"conversations":[{"id":"selected","subject":"Selected detail","app_url":"missive://selected","web_url":"https://mail.missiveapp.com/#inbox/conversations/selected"}]}' ;;
  *) exit 65 ;;
esac
"#,
        );
        let token_env = format!(
            "HERDR_TEST_MISSIVE_TOKEN_{}",
            crate::config::test_unique_suffix().replace('-', "_")
        );
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        env.set(&token_env, "test-token");
        let config = MissiveConfig {
            token_env,
            team: Some("team".into()),
            organization: Some("org".into()),
        };

        let (conversations, users) = fetch_missive_snapshot(
            &config,
            &[],
            Some("selected"),
            None,
            &curl,
            Instant::now() + Duration::from_secs(5),
            Duration::from_secs(2),
            None,
            false,
        )
        .expect("Missive snapshot");

        let selected = conversations
            .iter()
            .find(|conversation| conversation.id == "selected")
            .expect("selected conversation");
        assert_eq!(selected.subject, "Selected detail");
        assert_eq!(selected.messages[0].preview, "hydrated message");
        assert_eq!(selected.drafts[0].preview, "draft");
        assert_eq!(selected.posts[0].preview, "post");
        assert_eq!(selected.notes[0].preview, "note");
        assert!(users[0].is_me);
        assert!(conversations
            .iter()
            .find(|conversation| conversation.id == "first")
            .is_some_and(|conversation| conversation.messages.is_empty()));
    }

    #[test]
    fn pane_conversation_missing_from_list_is_fetched_and_marked_bound() {
        let dir = fixture_dir("missive-pane-fallback");
        let curl = dir.join("curl");
        let log = dir.join("curl-argv.log");
        write_executable(
            &curl,
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
for argument do url="$argument"; done
case "$url" in
  *"/users") printf '%s' '{{"users":[{{"id":"me","name":"Ada","me":true}}]}}' ;;
  *"/conversations?team_all=team") printf '%s' '{{"conversations":[]}}' ;;
  *"/conversations/pane-only/messages") printf '%s' '{{"messages":[]}}' ;;
  *"/conversations/pane-only/drafts") printf '%s' '{{"drafts":[]}}' ;;
  *"/conversations/pane-only/posts") printf '%s' '{{"posts":[]}}' ;;
  *"/conversations/pane-only/comments") printf '%s' '{{"comments":[]}}' ;;
  *"/conversations/pane-only") printf '%s' '{{"conversations":[{{"id":"pane-only","subject":"Pane conversation","web_url":"https://mail.missiveapp.com/#inbox/conversations/pane-only"}}]}}' ;;
  *) exit 65 ;;
esac
"#,
                log.display()
            ),
        );
        let token_env = format!(
            "HERDR_TEST_MISSIVE_TOKEN_{}",
            crate::config::test_unique_suffix().replace('-', "_")
        );
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        env.set(&token_env, "test-token");
        let config = MissiveConfig {
            token_env,
            team: Some("team".into()),
            organization: None,
        };
        let panes = panes_with_context(crate::work_context::PaneWorkContext {
            missive_urls: vec!["https://mail.missiveapp.com/#inbox/conversations/pane-only".into()],
            ..Default::default()
        });

        let (conversations, _) = fetch_missive_snapshot(
            &config,
            &panes,
            None,
            None,
            &curl,
            Instant::now() + Duration::from_secs(5),
            Duration::from_secs(2),
            None,
            false,
        )
        .expect("Missive pane fallback");

        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0].subject, "Pane conversation");
        assert!(conversations[0].pane_bound);
        let argv = std::fs::read_to_string(log).expect("curl argv");
        assert!(argv.contains("/conversations/pane-only"));
    }

    #[test]
    fn retained_missive_conversations_recompute_pane_binding() {
        let previous = Snapshot {
            items: Vec::new(),
            conversations: vec![MissiveConversation {
                id: "previous".into(),
                subject: "Previous".into(),
                app_url: "missive://previous".into(),
                web_url: "https://mail.missiveapp.com/#inbox/conversations/previous".into(),
                team: None,
                assignees: Vec::new(),
                last_activity_at: None,
                closed: false,
                labels: Vec::new(),
                pane_bound: true,
                messages: Vec::new(),
                notes: Vec::new(),
                drafts: Vec::new(),
                posts: Vec::new(),
            }],
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: SystemTime::UNIX_EPOCH,
        };

        assert!(!previous_missive(Some(&previous), &[])[0].pane_bound);
        let panes = panes_with_context(crate::work_context::PaneWorkContext {
            missive_urls: vec!["https://mail.missiveapp.com/#inbox/conversations/previous".into()],
            ..Default::default()
        });
        assert!(previous_missive(Some(&previous), &panes)[0].pane_bound);
    }

    #[test]
    fn missive_session_users_are_not_persisted() {
        let snapshot = Snapshot {
            items: Vec::new(),
            conversations: Vec::new(),
            missive_users: vec![MissiveUser {
                id: "user-1".into(),
                name: "Ada".into(),
                email: None,
                is_me: true,
            }],
            unavailable: None,
            observed_at: SystemTime::UNIX_EPOCH,
        };
        let persisted = serde_json::to_value(&snapshot).expect("serialize snapshot");
        assert!(persisted.get("missive_users").is_none());
    }

    #[test]
    fn missive_users_are_reused_after_the_session_identity_is_resolved() {
        let dir = fixture_dir("missive-session-users");
        let curl = dir.join("curl");
        write_executable(
            &curl,
            r#"#!/bin/sh
for argument do url="$argument"; done
case "$url" in
  *"/users"*) exit 70 ;;
  *"/conversations?team_all=team") printf '%s' '{"conversations":[]}' ;;
  *) exit 71 ;;
esac
"#,
        );
        let token_env = format!(
            "HERDR_TEST_MISSIVE_TOKEN_{}",
            crate::config::test_unique_suffix().replace('-', "_")
        );
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        env.set(&token_env, "test-token");
        let config = MissiveConfig {
            token_env,
            team: Some("team".into()),
            organization: Some("org".into()),
        };
        let users = vec![MissiveUser {
            id: "me".into(),
            name: "Ada".into(),
            email: None,
            is_me: true,
        }];

        let (_, reused) = fetch_missive_snapshot(
            &config,
            &[],
            None,
            Some(&users),
            &curl,
            Instant::now() + Duration::from_secs(2),
            Duration::from_secs(1),
            None,
            false,
        )
        .expect("cached session identity should avoid /users");

        assert_eq!(reused, users);
    }

    #[test]
    fn missive_curl_program_can_be_overridden_without_changing_global_path() {
        let mut app = test_app_with_work_index();
        app.work_index_curl_program_override = Some(Path::new("/tmp/fake-missive-curl").into());
        assert_eq!(
            app.work_index_curl_program(),
            Path::new("/tmp/fake-missive-curl")
        );
    }
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn work_item_key(number: u64) -> crate::app::state::WorkItemKey {
        crate::app::state::WorkItemKey {
            repo: "owner/repo".into(),
            pr_number: Some(number),
            pr_url: Some(format!("https://github.com/owner/repo/pull/{number}")),
            ticket_id: None,
        }
    }

    fn unavailable_detail(observed_at: SystemTime) -> WorkItemDetail {
        let mut detail = WorkItemDetail::unavailable("not available");
        detail.observed_at = observed_at;
        detail
    }

    fn test_app_with_work_index() -> crate::app::App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut config = crate::config::Config::default();
        config.work_index.enabled = true;
        config.work_index.repos = vec!["owner/repo".into()];
        let mut app =
            crate::app::App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        app.state.work_index_enabled = true;
        app
    }

    fn write_executable(path: &Path, contents: &str) {
        std::fs::write(path, contents).expect("write fake executable");
        let mut permissions = std::fs::metadata(path)
            .expect("read fake executable metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("make fake executable");
    }

    fn fixture_dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "herdr-work-index-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("create fixture directory");
        path
    }

    fn config() -> WorkIndexConfig {
        WorkIndexConfig {
            enabled: true,
            refresh_interval_seconds: 300,
            linear_team: Some("SCA".into()),
            repos: vec!["owner/repo".into()],
        }
    }

    fn panes_with_context(context: crate::work_context::PaneWorkContext) -> Vec<WorkIndexPane> {
        vec![WorkIndexPane {
            work_context: context,
            name: Some("fixture".into()),
            agent: Some("codex".into()),
            display_agent: Some("cx".into()),
            agent_status: AgentStatus::Idle,
            workspace_id: "workspace".into(),
            tab_id: "tab".into(),
            pane_id: "pane".into(),
        }]
    }

    #[test]
    fn pane_declaration_change_makes_work_index_refresh_immediately_due() {
        let mut app = test_app_with_work_index();
        let later = Instant::now() + Duration::from_secs(300);
        app.next_work_index_refresh = later;
        let mut context = PaneWorkContext::default();
        context.ticket_ids.push("SCA-18".into());
        let panes = panes_with_context(context);

        let now = Instant::now();
        assert!(app.note_work_index_contexts(&panes, now));
        assert_eq!(app.next_work_index_refresh, now);

        app.next_work_index_refresh = later;
        assert!(!app.note_work_index_contexts(&panes, now));
        assert_eq!(app.next_work_index_refresh, later);
    }

    #[test]
    fn opening_sidebar_view_requests_git_and_provider_scans_once() {
        let mut app = test_app_with_work_index();
        let now = Instant::now();
        let later = now + Duration::from_secs(300);
        app.next_git_work_context_refresh = later;
        app.next_work_index_refresh = later;

        app.state
            .set_sidebar_group_mode(crate::app::state::SidebarGroupMode::Spaces);
        assert!(app.state.take_sidebar_view_scan_request());
        app.request_sidebar_view_scan(now);
        assert_eq!(app.next_git_work_context_refresh, now);
        assert_eq!(app.next_work_index_refresh, now);

        app.state
            .set_sidebar_group_mode(crate::app::state::SidebarGroupMode::Spaces);
        assert!(!app.state.take_sidebar_view_scan_request());
    }

    fn fake_programs(
        dir: &Path,
        gh: &str,
        linearis: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let gh_path = dir.join("gh");
        let linearis_path = dir.join("linearis");
        write_executable(&gh_path, gh);
        write_executable(&linearis_path, linearis);
        (gh_path, linearis_path)
    }

    #[test]
    fn shell_pane_declared_pull_request_enters_the_snapshot() {
        let dir = fixture_dir("shell-pane-pr");
        let (gh, linearis) = fake_programs(
            &dir,
            &format!(
                r#"#!/bin/sh
case "$*" in
  "pr view 159 --repo owner/repo --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}") printf '%s' '{{"number":159,"title":"Shell pane PR","state":"OPEN","headRefName":"t3/f16","url":"https://github.com/owner/repo/pull/159"}}' ;;
  *"pr list"*) printf '%s' '[]' ;;
  *) exit 42 ;;
esac
"#
            ),
            "#!/bin/sh\nexit 42\n",
        );
        let mut app = test_app_with_work_index();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("shell")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .expect("shell terminal")
            .clone();
        let terminal = app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("shell terminal state");
        assert!(!terminal.is_agent_terminal());
        terminal
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                pr_urls: Some(vec!["https://github.com/owner/repo/pull/159".into()]),
                ..Default::default()
            })
            .expect("shell work context");
        assert!(app.collect_agent_infos().is_empty());

        let panes = app.collect_work_index_panes();
        assert_eq!(panes.len(), 1);
        let mut config = config();
        config.repos.clear();
        config.linear_team = None;
        let snapshot = refresh_work_index(
            &config,
            &panes,
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );

        let pull_request = snapshot
            .items
            .iter()
            .find(|item| item.pr_number == Some(159))
            .expect("shell pane pull request");
        assert_eq!(pull_request.pr_title.as_deref(), Some("Shell pane PR"));
        assert!(pull_request.source.github && pull_request.source.pane);
        assert_eq!(pull_request.panes[0].pane_id, panes[0].pane_id);
    }

    #[test]
    fn pane_declared_objects_are_fetched_with_empty_repo_config() {
        let dir = fixture_dir("pane-fallback");
        let gh_log = dir.join("gh-argv.log");
        let linear_log = dir.join("linear-argv.log");
        let (gh, linearis) = fake_programs(
            &dir,
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$*" in
  "pr list --repo pane/repo --state open --limit 200 --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}") printf '%s' '[]' ;;
  "pr list --repo pane/repo --state open"*) printf '%s' '[]' ;;
  "pr view 77 --repo pane/repo --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}") printf '%s' '{{"number":77,"title":"Merged pane PR","state":"MERGED","headRefName":"issue/out-9","url":"https://github.com/pane/repo/pull/77"}}' ;;
  *) exit 42 ;;
esac
"#,
                gh_log.display()
            ),
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$*" in
  "issues list"*) printf '%s' '{{"nodes":[]}}' ;;
  "cycles list"*) printf '%s' '{{"nodes":[]}}' ;;
  *"issues read SCA-9999") printf '%s' '{{"identifier":"SCA-9999","title":"Outside filter","description":"full summary","state":{{"name":"Canceled"}},"assignee":{{"name":"other"}},"priority":1,"branchName":"issue/sca-9999"}}' ;;
  "attachments list SCA-9999"*) printf '%s' '[]' ;;
  *) exit 43 ;;
esac
"#,
                linear_log.display()
            ),
        );
        let panes = panes_with_context(crate::work_context::PaneWorkContext {
            repo: Some("pane/repo".into()),
            pr_urls: vec!["https://github.com/pane/repo/pull/77".into()],
            ticket_ids: vec!["SCA-9999".into()],
            ..Default::default()
        });
        let mut config = config();
        config.repos.clear();

        let snapshot = refresh_work_index(
            &config,
            &panes,
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );

        let pull_request = snapshot
            .items
            .iter()
            .find(|item| item.pr_url.as_deref() == Some("https://github.com/pane/repo/pull/77"))
            .expect("pane pull request");
        assert_eq!(pull_request.pr_title.as_deref(), Some("Merged pane PR"));
        assert_eq!(pull_request.pr_state.as_deref(), Some("merged"));
        assert!(pull_request.source.github && pull_request.source.pane);
        let ticket = snapshot
            .items
            .iter()
            .flat_map(|item| item.ticket_details.iter())
            .find(|ticket| ticket.identifier == "SCA-9999")
            .expect("pane ticket");
        assert_eq!(ticket.title.as_deref(), Some("Outside filter"));
        assert_eq!(ticket.description.as_deref(), Some("full summary"));
        assert!(snapshot.items.iter().any(|item| {
            item.source.linear && item.source.pane && item.ticket_ids == ["SCA-9999"]
        }));
        let gh_argv = std::fs::read_to_string(gh_log).expect("GitHub argv");
        assert!(gh_argv.contains("pr list --repo pane/repo --state open"));
        assert!(gh_argv.contains(&format!(
            "pr view 77 --repo pane/repo --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}"
        )));
        let linear_argv = std::fs::read_to_string(linear_log).expect("Linear argv");
        assert!(linear_argv
            .lines()
            .any(|line| line.ends_with("issues read SCA-9999")));
    }

    #[test]
    fn day_links_fetch_merged_pull_requests_and_done_tickets() {
        let dir = fixture_dir("day-links");
        let (gh, linearis) = fake_programs(
            &dir,
            &format!(
                r#"#!/bin/sh
case "$*" in
  "pr view 9 --repo acme/app --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}") printf '%s' '{{"number":9,"title":"Merged day PR","state":"MERGED","headRefName":"day/9","url":"https://github.com/acme/app/pull/9"}}' ;;
  *"pr list"*) printf '%s' '[]' ;;
  *) exit 42 ;;
esac
"#
            ),
            r#"#!/bin/sh
case "$*" in
  *"issues read SCA-42") printf '%s' '{"identifier":"SCA-42","title":"Done day ticket","state":{"name":"Done"}}' ;;
  *"attachments list SCA-42"*) printf '%s' '[]' ;;
  *) exit 43 ;;
esac
"#,
        );
        let links = crate::day::DayLinks {
            tickets: vec!["SCA-42".into()],
            prs: vec!["https://github.com/acme/app/pull/9".into()],
        };
        let mut config = config();
        config.repos.clear();
        config.linear_team = None;

        let snapshot = refresh_work_index_with_missive(
            &config,
            &MissiveConfig::default(),
            &[],
            WorkIndexRefreshContext {
                day_links: Some(&links),
                ..WorkIndexRefreshContext::default()
            },
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
            Path::new("/usr/bin/false"),
        );

        assert!(snapshot.items.iter().any(|item| {
            item.pr_url.as_deref() == Some("https://github.com/acme/app/pull/9")
                && item.pr_state.as_deref() == Some("merged")
        }));
        assert!(snapshot.items.iter().any(|item| {
            item.ticket_details.iter().any(|ticket| {
                ticket.identifier == "SCA-42" && ticket.state.as_deref() == Some("Done")
            })
        }));
    }

    #[test]
    fn observation_budgets_cover_real_provider_latency() {
        assert!(WORK_INDEX_TARGET_TIMEOUT >= Duration::from_secs(20));
        assert!(WORK_INDEX_BATCH_TIMEOUT >= Duration::from_secs(60));
    }

    #[test]
    fn pane_pull_request_finishes_before_slow_list_and_previous_list_items_survive() {
        let dir = fixture_dir("pane-before-slow-list");
        let argv_log = dir.join("gh-argv.log");
        let list_budget = crate::platform::test_spawn_budget(Duration::from_millis(100));
        let (gh, linearis) = fake_programs(
            &dir,
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$*" in
  "pr view 159 --repo owner/repo --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}") printf '%s' '{{"number":159,"title":"Pane first","state":"MERGED","headRefName":"t3/f15","url":"https://github.com/owner/repo/pull/159"}}' ;;
  "pr list --repo owner/repo --state open --limit 200 --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}") exec sleep {} ;;
  *) exit 42 ;;
esac
"#,
                argv_log.display(),
                // The list must still be running when the budget expires, so it
                // outlives the budget by the same factor on every platform.
                list_budget.as_secs_f32() * 10.0
            ),
            "#!/bin/sh\ncase \"$*\" in *\"issues list\"*) printf '%s' '{\"nodes\":[]}' ;; *\"cycles list\"*) printf '%s' '{\"nodes\":[]}' ;; *) printf '%s' '[]' ;; esac\n",
        );
        let panes = panes_with_context(crate::work_context::PaneWorkContext {
            repo: Some("owner/repo".into()),
            pr_urls: vec!["https://github.com/owner/repo/pull/159".into()],
            ..Default::default()
        });
        let previous = Snapshot {
            items: vec![WorkItem {
                repo: "owner/repo".into(),
                pr_number: Some(7),
                pr_url: Some("https://github.com/owner/repo/pull/7".into()),
                pr_title: Some("Previous list item".into()),
                pr_state: Some("open".into()),
                draft: false,
                review_decision: None,
                created_at: None,
                updated_at: None,
                additions: 0,
                deletions: 0,
                author: None,
                assignees: Vec::new(),
                labels: Vec::new(),
                check_state: PrCheckState::Unknown,
                audience: PrAudience::Unclassified,
                cached_pr_detail: None,
                ticket_ids: Vec::new(),
                ticket_title: None,
                ticket_state: None,
                ticket_details: Vec::new(),
                branch: Some("previous".into()),
                preview_urls: Vec::new(),
                panes: Vec::new(),
                source: WorkItemSource {
                    github: true,
                    ..WorkItemSource::default()
                },
            }],
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: SystemTime::UNIX_EPOCH,
        };

        let snapshot = refresh_work_index_with_missive(
            &config(),
            &MissiveConfig::default(),
            &panes,
            WorkIndexRefreshContext {
                previous: Some(&previous),
                ..WorkIndexRefreshContext::default()
            },
            Instant::now(),
            Instant::now() + Duration::from_secs(5),
            list_budget,
            &gh,
            &linearis,
            Path::new("/usr/bin/false"),
        );

        assert!(snapshot.items.iter().any(|item| {
            item.pr_number == Some(159)
                && item.pr_title.as_deref() == Some("Pane first")
                && item.source.pane
        }));
        assert!(snapshot.items.iter().any(|item| item.pr_number == Some(7)));
        assert_eq!(
            snapshot.unavailable_reason(WorkIndexSource::Github),
            Some("list observation timed out")
        );
        let argv = std::fs::read_to_string(argv_log).expect("GitHub argv");
        let mut calls = argv.lines();
        assert!(calls
            .next()
            .is_some_and(|call| call.starts_with("pr view 159 --repo owner/repo")));
        assert!(calls
            .next()
            .is_some_and(|call| call.starts_with("pr list --repo owner/repo")));
    }

    #[test]
    fn repo_union_deduplicates_config_pane_and_pull_request_repositories() {
        let panes = panes_with_context(crate::work_context::PaneWorkContext {
            repo: Some("owner/repo".into()),
            pr_urls: vec![
                "https://github.com/OWNER/repo/pull/1".into(),
                "https://github.com/pane/other/pull/2".into(),
            ],
            ..Default::default()
        });
        let mut config = config();
        config.repos = vec!["owner/repo".into(), "configured/only".into()];

        assert_eq!(
            work_index_repos(&config, &panes),
            ["owner/repo", "configured/only", "pane/other"]
        );
    }

    #[test]
    fn failed_repository_does_not_discard_a_successful_pane_pull_request() {
        let dir = fixture_dir("partial-repo-failure");
        let (gh, linearis) = fake_programs(
            &dir,
            &format!(
                r#"#!/bin/sh
case "$*" in
  "pr view 77 --repo good/repo --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}") printf '%s' '{{"number":77,"title":"Fresh pane PR","state":"MERGED","headRefName":"issue/77","url":"https://github.com/good/repo/pull/77"}}' ;;
  "pr list --repo good/repo --state open --limit 200 --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}") printf '%s' '[]' ;;
  "pr list --repo bad/repo --state open --limit 200 --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}") printf '%s' 'rate limited' >&2; exit 42 ;;
  *"--state open"*) printf '%s' '[]' ;;
  *) exit 43 ;;
esac
"#
            ),
            "#!/bin/sh\nprintf '%s' '{\"nodes\":[]}'\n",
        );
        let panes = panes_with_context(crate::work_context::PaneWorkContext {
            pr_urls: vec!["https://github.com/good/repo/pull/77".into()],
            ..Default::default()
        });
        let mut config = config();
        config.repos = vec!["good/repo".into(), "bad/repo".into()];

        let snapshot = refresh_work_index(
            &config,
            &panes,
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );

        assert!(snapshot.items.iter().any(|item| {
            item.pr_number == Some(77)
                && item.pr_title.as_deref() == Some("Fresh pane PR")
                && item.source.github
                && item.source.pane
        }));
        assert!(snapshot
            .unavailable_reason(WorkIndexSource::Github)
            .is_some_and(|reason| reason.contains("rate limited")));
    }

    #[test]
    fn github_audience_failure_is_named_without_discarding_pull_requests() {
        let dir = fixture_dir("github-audience-failure");
        let (gh, linearis) = fake_programs(
            &dir,
            &format!(
                r#"#!/bin/sh
case "$*" in
  "pr list --repo owner/repo --state open --limit 200 --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}") printf '%s' '[{{"number":7,"title":"Visible PR","state":"OPEN","headRefName":"issue/7","url":"https://github.com/owner/repo/pull/7"}}]' ;;
  *"--state open"*) printf '%s' 'audience unavailable' >&2; exit 42 ;;
  *) exit 43 ;;
esac
"#
            ),
            "#!/bin/sh\nprintf '%s' '{\"nodes\":[]}'\n",
        );

        let snapshot = refresh_work_index(
            &config(),
            &[],
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );

        assert!(snapshot.items.iter().any(|item| item.pr_number == Some(7)));
        assert!(snapshot
            .unavailable_reason(WorkIndexSource::Github)
            .is_some_and(|reason| reason.contains("audience unavailable")));
    }

    #[test]
    fn parses_normal_rfc3339_timestamp() {
        assert_eq!(
            parse_rfc3339_system_time("2026-08-30T11:22:33Z"),
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_788_088_953))
        );
    }

    #[test]
    fn parses_leap_year_rfc3339_timestamp() {
        assert_eq!(
            parse_rfc3339_system_time("2024-02-29T00:00:00Z"),
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_709_164_800))
        );
    }

    #[test]
    fn parses_fractional_and_offset_rfc3339_timestamps() {
        let expected = parse_rfc3339_system_time("2026-08-30T11:22:33Z");
        assert_eq!(
            parse_rfc3339_system_time("2026-08-30T13:22:33.123+02:00"),
            expected
        );
        assert_eq!(
            parse_rfc3339_system_time("2026-08-30T09:22:33-02:00"),
            expected
        );
    }

    #[test]
    fn malformed_rfc3339_timestamp_is_unknown() {
        assert_eq!(parse_rfc3339_system_time("2023-02-29T00:00:00Z"), None);
    }

    #[test]
    fn older_work_item_json_defaults_created_at_to_unknown() {
        let item: WorkItem = serde_json::from_str(
            r#"{"repo":"owner/repo","pr_number":7,"pr_url":null,"pr_title":null,"pr_state":null,"draft":false,"review_decision":null,"ticket_ids":[],"ticket_title":null,"ticket_state":null,"branch":null,"preview_urls":[],"panes":[],"source":{"github":true,"linear":false,"pane":false}}"#,
        )
        .expect("older work item JSON");

        assert_eq!(item.created_at, None);
    }

    #[test]
    fn work_index_session_resolves_me_once_with_injected_programs() {
        let dir = fixture_dir("session-identities");
        let (gh, linearis) = fake_programs(
            &dir,
            r#"#!/bin/sh
case "$*" in
  "api user") printf '%s' '{"login":"matthias"}' ;;
  "api repos/owner/repo/assignees --paginate --slurp") printf '%s' '[[{"login":"grace"},{"login":"matthias"}]]' ;;
  *) exit 42 ;;
esac
"#,
            r#"#!/bin/sh
case "$*" in
  "auth status --compact") printf '%s' '{"authenticated":true,"user":{"id":"linear-user-1","name":"Matthias"}}' ;;
  "users list --active -l 250 --compact") printf '%s' '{"nodes":[{"name":"Ada"},{"name":"Matthias"}]}' ;;
  *) exit 42 ;;
esac
"#,
        );
        let deadline = Instant::now() + WORK_INDEX_BATCH_TIMEOUT;
        let missive_users = [MissiveUser {
            id: "missive-1".into(),
            name: "Mina".into(),
            email: None,
            is_me: true,
        }];
        let session = resolve_work_index_session(
            &config(),
            WorkIndexSession::default(),
            &missive_users,
            deadline,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
            None,
            WorkIndexCacheBypass::default(),
        );

        assert_eq!(session.linear.viewer.as_deref(), Some("Matthias"));
        assert_eq!(
            session.linear_query_identity.as_deref(),
            Some("linear-user-1")
        );
        assert_eq!(session.linear.assignees, ["Ada", "Matthias"]);
        assert_eq!(session.github.viewer.as_deref(), Some("matthias"));
        assert_eq!(session.github.assignees, ["grace", "matthias"]);
        assert_eq!(session.missive.viewer.as_deref(), Some("Mina"));
        assert_eq!(session.missive.assignees, ["Mina"]);

        let unchanged = resolve_work_index_session(
            &config(),
            session.clone(),
            &[],
            deadline,
            WORK_INDEX_TARGET_TIMEOUT,
            Path::new("/usr/bin/false"),
            Path::new("/usr/bin/false"),
            None,
            WorkIndexCacheBypass::default(),
        );
        assert_eq!(
            unchanged, session,
            "resolved providers are not queried twice"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn linear_viewer_name_fallback_and_unresolved_query_omission() {
        let fallback_dir = fixture_dir("linear-viewer-name-fallback");
        let (_, fallback_linearis) = fake_programs(
            &fallback_dir,
            "#!/bin/sh\nexit 42\n",
            r#"#!/bin/sh
case "$*" in
  "auth status --compact") printf '%s' '{"authenticated":true,"user":{"name":"Matthias"}}' ;;
  "users list --active -l 250 --compact") printf '%s' '{"nodes":[]}' ;;
  *) exit 42 ;;
esac
"#,
        );
        let (fallback, fallback_identity) = fetch_linear_directory(
            &fallback_linearis,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
            None,
            false,
        );
        assert_eq!(fallback.viewer.as_deref(), Some("Matthias"));
        assert_eq!(fallback_identity.as_deref(), Some("Matthias"));

        let unresolved_dir = fixture_dir("linear-viewer-unresolved");
        let argv_log = unresolved_dir.join("linearis-argv.log");
        let (_, unresolved_linearis) = fake_programs(
            &unresolved_dir,
            "#!/bin/sh\nexit 42\n",
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$*" in
  "auth status --compact") printf '%s' '{{"authenticated":true}}' ;;
  "users list --active -l 250 --compact") printf '%s' '{{"nodes":[]}}' ;;
  *"issues list --team SCA -l 100") printf '%s' '{{"nodes":[{{"identifier":"SCA-1","title":"unfiltered","state":{{"name":"In Progress"}},"assignee":{{"name":"Ada"}}}}]}}' ;;
  *"issues list --team SCA --status Triage -l 100") printf '%s' '{{"nodes":[]}}' ;;
  *"cycles list --team SCA --active") printf '%s' '{{"nodes":[]}}' ;;
  *) exit 42 ;;
esac
"#,
                argv_log.display()
            ),
        );
        let (unresolved, unresolved_identity) = fetch_linear_directory(
            &unresolved_linearis,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
            None,
            false,
        );
        assert!(unresolved.viewer.is_none());
        assert!(unresolved_identity.is_none());
        let observation = fetch_linear_tickets(
            "SCA",
            unresolved_identity.as_deref(),
            &unresolved_linearis,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
            None,
            false,
        )
        .expect("unfiltered Linear tickets");
        let tickets = observation.tickets;
        assert_eq!(tickets.len(), 1);
        let session = WorkIndexSession::default();
        assert!(
            crate::app::state::SidebarWorkFilter::default().matches_linear(&tickets[0], &session)
        );
        let argv = std::fs::read_to_string(argv_log).expect("Linear argv");
        assert!(argv
            .lines()
            .any(|call| call.ends_with("issues list --team SCA -l 100")));
        assert!(!argv.split_whitespace().any(|argument| argument == "me"));
    }

    #[test]
    fn github_pull_request_list_uses_lean_open_query() {
        let dir = fixture_dir("github-created-at");
        let (gh, _linearis) = fake_programs(
            &dir,
            r#"#!/bin/sh
test "$*" = "pr list --repo owner/repo --state open --limit 200 --json number,title,author,assignees,state,updatedAt,createdAt,reviewDecision,labels,isDraft,headRefName,url" || exit 42
printf '%s' '[{"number":7,"title":"PR","author":{"login":"ada"},"assignees":[{"login":"grace"}],"state":"OPEN","headRefName":"branch","isDraft":false,"reviewDecision":"","url":"https://github.com/owner/repo/pull/7","createdAt":"2026-08-30T11:22:33Z","updatedAt":"2026-08-31T11:22:33Z","labels":[{"name":"bug"}]}]'
"#,
            "#!/bin/sh\nprintf '%s' '[]'\n",
        );

        let pull_requests = fetch_github_pull_requests(
            "owner/repo",
            &gh,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
            None,
            false,
        )
        .unwrap_or_else(|_| panic!("GitHub pull request fetch failed"));

        assert_eq!(pull_requests.len(), 1);
        assert_eq!(
            pull_requests[0].created_at,
            parse_rfc3339_system_time("2026-08-30T11:22:33Z")
        );
        assert_eq!(pull_requests[0].author.as_deref(), Some("ada"));
        assert_eq!(pull_requests[0].assignees, vec!["grace"]);
        assert_eq!(pull_requests[0].state, "open");
        assert_eq!(
            (pull_requests[0].additions, pull_requests[0].deletions),
            (0, 0)
        );
        assert_eq!(pull_requests[0].labels, vec!["bug"]);
        assert_eq!(pull_requests[0].check_state, PrCheckState::Unknown);
    }

    #[test]
    fn github_pull_request_detail_fetch_requests_every_render_field() {
        let dir = fixture_dir("github-detail-fields");
        let (gh, _linearis) = fake_programs(
            &dir,
            &format!(
                r#"#!/bin/sh
test "$*" = "pr view 7 --repo owner/repo --json {GITHUB_PULL_REQUEST_DETAIL_FIELDS}" || exit 42
printf '%s' '{{"number":7,"title":"Detail","body":"Body","author":{{"login":"ms"}},"baseRefName":"main","headRefName":"feat/detail","headRefOid":"abcdef012345","createdAt":"2026-08-30T11:22:33Z","updatedAt":"2026-08-30T12:22:33Z","labels":[{{"name":"high-risk"}}],"url":"https://github.com/owner/repo/pull/7","reviewDecision":"REVIEW_REQUIRED","isDraft":false,"statusCheckRollup":[{{"name":"test","conclusion":"FAILURE"}},{{"name":"lint","conclusion":"SUCCESS"}}],"reviews":[{{"author":{{"login":"grace"}}}}],"comments":[{{"author":{{"login":"reviewer"}},"body":"Looks good"}}],"files":[{{"path":"src/lib.rs","additions":4,"deletions":2}}],"commits":[{{"oid":"abcdef012345","messageHeadline":"fix detail"}}],"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","autoMergeRequest":{{"enabledAt":"2026-08-30T12:30:00Z"}}}}'
"#
            ),
            "#!/bin/sh\nprintf '%s' '[]'\n",
        );

        let detail = fetch_github_pull_request_detail(
            "owner/repo",
            7,
            &gh,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
            None,
            false,
        )
        .unwrap_or_else(|_| panic!("GitHub pull request detail fetch failed"));

        assert_eq!(detail.title.as_deref(), Some("Detail"));
        assert_eq!(detail.author.as_deref(), Some("ms"));
        assert_eq!(detail.reviewers, vec!["grace"]);
        assert_eq!(detail.head_sha.as_deref(), Some("abcdef012345"));
        assert_eq!(detail.merge_state_status.as_deref(), Some("CLEAN"));
        assert!(detail.auto_merge_enabled);
        assert_eq!(detail.labels, vec!["high-risk"]);
        assert_eq!(detail.review_decision.as_deref(), Some("REVIEW_REQUIRED"));
        assert_eq!(
            detail.checks,
            Some(WorkItemCheckSummary {
                failing: 1,
                total: 2
            })
        );
        assert_eq!(detail.comments[0].author.as_deref(), Some("reviewer"));
        assert_eq!(detail.comments[0].body, "Looks good");
        assert_eq!(detail.actions[0].name, "test");
        assert_eq!(detail.actions[0].state, "FAILURE");
        assert_eq!(detail.files[0].path, "src/lib.rs");
        assert_eq!(
            (detail.files[0].additions, detail.files[0].deletions),
            (4, 2)
        );
        assert_eq!(detail.commits[0].short_id, "abcdef0");
        assert_eq!(detail.commits[0].subject, "fix detail");
    }

    #[test]
    fn github_timeline_maps_real_shape_fixture_newest_first() {
        let fixture = include_str!("../tests/fixtures/work-index/github-pr-timeline.json");
        let events = parse_github_timeline(
            serde_json::from_str(fixture).expect("real-shape GitHub timeline fixture"),
        );

        assert_eq!(events.len(), 7);
        assert_eq!(events[0].kind, "commented");
        assert_eq!(events[0].actor.as_deref(), Some("reviewer"));
        assert_eq!(events[0].summary, "First line of the comment");
        assert_eq!(events[1].summary, "merged 1234567");
        assert_eq!(events[2].summary, "labeled approved");
        assert_eq!(events[3].summary, "review requested from reviewer");
        assert_eq!(events[4].summary, "review approved");
        assert_eq!(events[5].summary, "fix: map timeline events");
        assert_eq!(events[6].summary, "closed");
    }

    #[test]
    fn github_timeline_uses_paginated_lean_api_argv() {
        let dir = fixture_dir("github-timeline-argv");
        let log = dir.join("argv.log");
        let gh = dir.join("gh");
        write_executable(
            &gh,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nprintf '%s' '[]'\n",
                log.display()
            ),
        );

        assert!(fetch_github_timeline(
            "owner/repo",
            42,
            &gh,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
            None,
            true,
        )
        .expect("GitHub timeline")
        .is_empty());
        let argv = std::fs::read_to_string(log).expect("timeline argv");
        assert!(
            argv.starts_with("api repos/owner/repo/issues/42/timeline --paginate --jq "),
            "{argv}"
        );
        assert!(argv.contains("{event,created_at"), "{argv}");
        assert!(argv.contains("| @json"), "{argv}");
    }

    #[test]
    fn github_collaborators_use_paginated_lean_api_argv() {
        let dir = fixture_dir("github-collaborators");
        let log = dir.join("argv.log");
        let gh = dir.join("gh");
        write_executable(
            &gh,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nprintf '%s' '[[{{\"login\":\"Zed\"}},{{\"login\":\"ada\"}},{{\"login\":\"Ada\"}}]]'\n",
                log.display()
            ),
        );

        let collaborators = fetch_github_collaborators(
            "owner/repo",
            &gh,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
        )
        .expect("GitHub collaborators");

        assert_eq!(collaborators, ["ada", "Zed"]);
        assert_eq!(
            std::fs::read_to_string(log).expect("collaborator argv"),
            "api repos/owner/repo/collaborators --paginate --jq .[] | {login} | @json\n"
        );
    }

    #[test]
    fn on_demand_github_detail_is_cached_in_snapshot_item_and_round_trips() {
        let dir = fixture_dir("github-detail-snapshot-cache");
        let (gh, _linearis) = fake_programs(
            &dir,
            &format!(
                r#"#!/bin/sh
test "$*" = "pr view 7 --repo owner/repo --json {GITHUB_PULL_REQUEST_DETAIL_FIELDS}" || exit 42
printf '%s' '{{"number":7,"title":"Detail","body":"Fetched on demand","author":{{"login":"ms"}},"baseRefName":"main","headRefName":"feat/detail","headRefOid":"abcdef012345","url":"https://github.com/owner/repo/pull/7","statusCheckRollup":[{{"name":"test","conclusion":"SUCCESS"}}],"files":[{{"path":"src/lib.rs","additions":4,"deletions":2}}]}}'
"#
            ),
            "#!/bin/sh\nexit 42\n",
        );
        let detail = fetch_github_pull_request_detail(
            "owner/repo",
            7,
            &gh,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
            None,
            false,
        )
        .expect("on-demand GitHub detail");
        let key = work_item_key(7);
        let mut snapshot = Snapshot {
            items: vec![WorkItem {
                repo: "owner/repo".into(),
                pr_number: Some(7),
                pr_url: Some("https://github.com/owner/repo/pull/7".into()),
                pr_title: Some("Lean summary".into()),
                pr_state: Some("open".into()),
                draft: false,
                review_decision: None,
                created_at: None,
                updated_at: None,
                additions: 0,
                deletions: 0,
                author: None,
                assignees: Vec::new(),
                labels: Vec::new(),
                check_state: PrCheckState::Unknown,
                audience: PrAudience::Unclassified,
                cached_pr_detail: None,
                ticket_ids: Vec::new(),
                ticket_title: None,
                ticket_state: None,
                ticket_details: Vec::new(),
                branch: Some("feat/detail".into()),
                preview_urls: Vec::new(),
                panes: Vec::new(),
                source: WorkItemSource {
                    github: true,
                    ..WorkItemSource::default()
                },
            }],
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: SystemTime::UNIX_EPOCH,
        };
        assert!(snapshot.cache_pr_detail(&key, &detail));
        let path = dir.join("snapshot.json");
        write_snapshot(&path, &snapshot).expect("persist snapshot detail");

        let loaded = load_snapshot(&path).expect("load snapshot detail");
        let cached = loaded.items[0]
            .cached_pr_detail
            .as_ref()
            .expect("cached PR detail");
        assert_eq!(cached.body.as_deref(), Some("Fetched on demand"));
        assert_eq!(cached.checks.as_ref().map(|checks| checks.total), Some(1));
        assert_eq!(cached.files[0].additions, 4);
        let cache = WorkItemDetailCache::from_snapshot(Some(&loaded));
        assert_eq!(
            cache.get(&key).and_then(|detail| detail.body.as_deref()),
            Some("Fetched on demand")
        );
        assert!(!cache.is_fresh(&key, Instant::now(), Duration::from_secs(300)));
        let mut refreshed_items = loaded.items.clone();
        refreshed_items[0].cached_pr_detail = None;
        carry_cached_pr_details(&mut refreshed_items, Some(&loaded));
        assert_eq!(
            refreshed_items[0]
                .cached_pr_detail
                .as_ref()
                .and_then(|detail| detail.body.as_deref()),
            Some("Fetched on demand")
        );
    }

    #[test]
    fn github_pull_request_detail_maps_the_real_cli_shape() {
        let dir = fixture_dir("github-detail-real-shape");
        let fixture = include_str!("../tests/fixtures/work-index/github-pr-view.json");
        let script = format!(
            "#!/bin/sh\ntest \"$*\" = \"pr view 125 --repo example/project --json {GITHUB_PULL_REQUEST_DETAIL_FIELDS}\" || exit 42\nprintf '%s' '{fixture}'\n"
        );
        let (gh, _linearis) = fake_programs(&dir, &script, "#!/bin/sh\nprintf '%s' '[]'\n");

        let detail = fetch_github_pull_request_detail(
            "example/project",
            125,
            &gh,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
            None,
            false,
        )
        .expect("GitHub pull request detail from captured CLI fixture");

        assert_eq!(detail.number, Some(125));
        assert_eq!(
            detail.title.as_deref(),
            Some("Render pull request details from the CLI response")
        );
        assert_eq!(detail.author.as_deref(), Some("example-author"));
        assert_eq!(detail.base_ref_name.as_deref(), Some("main"));
        assert_eq!(detail.head_ref_name.as_deref(), Some("fix/pr-detail"));
        assert_eq!(detail.reviewers, vec!["example-reviewer"]);
        assert_eq!(detail.review_decision.as_deref(), Some("APPROVED"));
        assert_eq!(detail.actions.len(), 2);
        assert!(detail
            .actions
            .iter()
            .all(|action| action.state == "SUCCESS"));
        assert_eq!(
            detail.checks,
            Some(WorkItemCheckSummary {
                failing: 0,
                total: 2,
            })
        );
        assert_eq!(detail.comments.len(), 1);
        assert_eq!(detail.files.len(), 1);
        assert_eq!(detail.commits.len(), 1);

        let value: Value = serde_json::from_str(fixture).expect("captured GitHub fixture JSON");
        assert_eq!(
            pr_check_state(value.get("statusCheckRollup")),
            PrCheckState::Passing
        );
    }

    #[test]
    fn linear_ticket_fetch_includes_inverse_relations() {
        let dir = fixture_dir("linear-inverse-relations");
        let (_gh, linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nprintf '%s' '[]'\n",
            r#"#!/bin/sh
printf '%s' '{"nodes":[{"identifier":"SCA-7","relations":{"nodes":[{"type":"blocks","relatedIssue":{"identifier":"SCA-8","title":"outbound"}}]},"inverseRelations":{"nodes":[{"type":"blocks","issue":{"identifier":"SCA-6","title":"inbound"}},{"type":"duplicate","issue":{"identifier":"SCA-5","title":"original"}}]}}]}'
"#,
        );

        let observation = fetch_linear_tickets(
            "SCA",
            None,
            &linearis,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
            None,
            false,
        )
        .expect("Linear ticket fetch");
        let tickets = observation.tickets;

        assert_eq!(
            tickets[0].relations,
            vec![
                "blocks  SCA-8  outbound",
                "blocked by  SCA-6  inbound",
                "duplicated by  SCA-5  original"
            ]
        );
    }

    #[test]
    fn linear_cycle_query_uses_id_and_keeps_the_cycle_name() {
        let dir = fixture_dir("linear-ticket-sets");
        let log = dir.join("argv.log");
        let (_gh, linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nprintf '%s' '[]'\n",
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$*" in
  *"issues list --team SCA --assignee linear-user-1 -l 100") printf '%s' '{{"nodes":[{{"identifier":"SCA-1","title":"assigned","state":{{"name":"In Progress"}},"priority":2}}]}}' ;;
  *"issues list --team SCA --status Triage -l 100") printf '%s' '{{"nodes":[{{"identifier":"SCA-2","title":"triage","state":{{"name":"Triage"}},"priority":3}}]}}' ;;
  *"cycles list --team SCA --active") printf '%s' '{{"nodes":[{{"id":"cycle-id-34","name":"cycle 34"}}]}}' ;;
  *"issues list --team SCA --cycle cycle-id-34 --status Done -l 100") printf '%s' '{{"nodes":[{{"identifier":"SCA-3","title":"done","state":{{"name":"Done"}},"priority":4}}]}}' ;;
  *) exit 42 ;;
esac
"#,
                log.display()
            ),
        );

        let observation = fetch_linear_tickets(
            "SCA",
            Some("linear-user-1"),
            &linearis,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
            None,
            false,
        )
        .expect("Linear ticket sets");
        assert!(observation.group_failures.is_empty());
        let tickets = observation.tickets;

        assert_eq!(tickets.len(), 3);
        assert_eq!(tickets[0].group, TicketGroup::Assigned);
        assert_eq!(tickets[1].group, TicketGroup::Triage);
        assert_eq!(tickets[2].group, TicketGroup::DoneThisCycle);
        assert_eq!(tickets[0].priority, Some(2));
        assert_eq!(tickets[2].cycle.as_deref(), Some("cycle 34"));
        let argv = std::fs::read_to_string(log).expect("read argv log");
        assert!(argv.contains("--assignee linear-user-1"));
        assert!(!argv.split_whitespace().any(|argument| argument == "me"));
        assert!(argv.contains("--status Triage"));
        assert!(argv.contains("--cycle cycle-id-34 --status Done"));
        assert!(!argv.contains("--cycle cycle 34 --status Done"));
    }

    #[test]
    fn triage_failure_keeps_other_groups_and_the_previous_triage_group() {
        let dir = fixture_dir("linear-triage-degraded");
        let (gh, linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nprintf '%s' '[]'\n",
            r#"#!/bin/sh
case "$*" in
  *"issues list --team SCA -l 100") printf '%s' '{"nodes":[]}' ;;
  *"issues list --team SCA --status Triage -l 100") printf '%s' '{"nodes":[{"identifier":"SCA-2","title":"Previous triage","state":{"name":"Triage"}}]}' ;;
  *"cycles list --team SCA --active") printf '%s' '{"nodes":[]}' ;;
  "attachments list"*) printf '%s' '[]' ;;
  *) exit 42 ;;
esac
"#,
        );
        let mut linear_only = config();
        linear_only.repos.clear();
        let previous = refresh_work_index(
            &linear_only,
            &[],
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );
        write_executable(
            &linearis,
            r#"#!/bin/sh
case "$*" in
  *"issues list --team SCA -l 100") printf '%s' '{"nodes":[{"identifier":"SCA-1","title":"Assigned","state":{"name":"In Progress"}}]}' ;;
  *"issues list --team SCA --status Triage -l 100") printf '%s' 'triage unavailable' >&2; exit 42 ;;
  *"cycles list --team SCA --active") printf '%s' '{"nodes":[{"id":"cycle-id-34","name":"cycle 34"}]}' ;;
  *"issues list --team SCA --cycle cycle-id-34 --status Done -l 100") printf '%s' '{"nodes":[{"identifier":"SCA-3","title":"Done","state":{"name":"Done"}}]}' ;;
  "attachments list"*) printf '%s' '[]' ;;
  *) exit 43 ;;
esac
"#,
        );

        let snapshot = refresh_work_index_with_missive(
            &linear_only,
            &MissiveConfig::default(),
            &[],
            WorkIndexRefreshContext {
                previous: Some(&previous),
                ..WorkIndexRefreshContext::default()
            },
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
            Path::new("/usr/bin/false"),
        );

        let tickets = snapshot
            .items
            .iter()
            .flat_map(|item| item.ticket_details.iter())
            .collect::<Vec<_>>();
        assert!(tickets
            .iter()
            .any(|ticket| ticket.identifier == "SCA-1" && ticket.group == TicketGroup::Assigned));
        assert!(tickets
            .iter()
            .any(|ticket| { ticket.identifier == "SCA-2" && ticket.group == TicketGroup::Triage }));
        assert!(tickets.iter().any(|ticket| {
            ticket.identifier == "SCA-3" && ticket.group == TicketGroup::DoneThisCycle
        }));
        let reason = snapshot
            .unavailable_reason(WorkIndexSource::Linear)
            .expect("Triage degradation");
        assert!(reason.contains("Triage"), "{reason}");
        assert!(reason.contains("triage unavailable"), "{reason}");
    }

    #[test]
    fn done_group_failure_keeps_assigned_and_triage_groups() {
        let dir = fixture_dir("linear-done-degraded");
        let (gh, linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nprintf '%s' '[]'\n",
            r#"#!/bin/sh
case "$*" in
  *"issues list --team SCA -l 100") printf '%s' '{"nodes":[{"identifier":"SCA-1","title":"Assigned","state":{"name":"In Progress"}}]}' ;;
  *"issues list --team SCA --status Triage -l 100") printf '%s' '{"nodes":[{"identifier":"SCA-2","title":"Triage","state":{"name":"Triage"}}]}' ;;
  *"cycles list --team SCA --active") printf '%s' '{"nodes":[{"id":"cycle-id-34","name":"cycle 34"}]}' ;;
  *"issues list --team SCA --cycle cycle-id-34 --status Done -l 100") printf '%s' 'done unavailable' >&2; exit 42 ;;
  "attachments list"*) printf '%s' '[]' ;;
  *) exit 43 ;;
esac
"#,
        );
        let mut linear_only = config();
        linear_only.repos.clear();

        let snapshot = refresh_work_index(
            &linear_only,
            &[],
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );

        let tickets = snapshot
            .items
            .iter()
            .flat_map(|item| item.ticket_details.iter())
            .collect::<Vec<_>>();
        assert!(tickets
            .iter()
            .any(|ticket| ticket.identifier == "SCA-1" && ticket.group == TicketGroup::Assigned));
        assert!(tickets
            .iter()
            .any(|ticket| { ticket.identifier == "SCA-2" && ticket.group == TicketGroup::Triage }));
        let reason = snapshot
            .unavailable_reason(WorkIndexSource::Linear)
            .expect("DoneThisCycle degradation");
        assert!(reason.contains("DoneThisCycle"), "{reason}");
        assert!(reason.contains("done unavailable"), "{reason}");
    }

    #[test]
    fn linear_detail_prefers_get_and_projects_comments() {
        let dir = fixture_dir("linear-detail-get");
        let (_gh, linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nprintf '%s' '[]'\n",
            r#"#!/bin/sh
test "$*" = "issues get SCA-7" || exit 42
printf '%s' '{"title":"ticket","description":"- [ ] ship","url":"https://linear.app/acme/issue/SCA-7","comments":{"nodes":[{"body":"newest","createdAt":"2026-09-01T10:00:00Z","user":{"name":"Ada"}}]}}'
"#,
        );

        let detail = fetch_linear_ticket_detail(
            "SCA-7",
            &linearis,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
            None,
            false,
        )
        .expect("Linear ticket detail");
        assert_eq!(detail.title.as_deref(), Some("ticket"));
        assert_eq!(detail.body.as_deref(), Some("- [ ] ship"));
        assert_eq!(detail.comments[0].author.as_deref(), Some("Ada"));
    }

    #[test]
    fn linear_detail_maps_the_real_cli_shape() {
        let dir = fixture_dir("linear-detail-real-shape");
        let fixture = include_str!("../tests/fixtures/work-index/linear-issue-read.json");
        let script = format!(
            "#!/bin/sh\ncase \"$*\" in\n  \"issues get SCA-3165\") exit 42 ;;\n  \"issues read SCA-3165 --with-comment-threads --compact\") printf '%s' '{fixture}' ;;\n  *) exit 43 ;;\nesac\n"
        );
        let (_gh, linearis) = fake_programs(&dir, "#!/bin/sh\nprintf '%s' '[]'\n", &script);

        let detail = fetch_linear_ticket_detail(
            "SCA-3165",
            &linearis,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
            None,
            false,
        )
        .expect("Linear ticket detail from captured CLI fixture");

        assert_eq!(
            detail.title.as_deref(),
            Some("Render ticket details from the CLI response")
        );
        assert!(detail
            .body
            .as_deref()
            .is_some_and(|body| body.contains("Shows the full ticket body")));
        assert_eq!(
            detail.url.as_deref(),
            Some("https://linear.app/scalable/issue/SCA-3165")
        );
        assert!(detail.created_at.is_some());
        assert!(detail.updated_at.is_some());
        assert_eq!(detail.comments.len(), 1);
    }

    #[test]
    fn linear_ticket_fixture_round_trips_creator() {
        let fixture = include_str!("../tests/fixtures/work-index/linear-issue-read.json");
        let value = serde_json::from_str(fixture).expect("Linear ticket fixture");
        let ticket = parse_linear_ticket(&value, TicketGroup::Assigned)
            .expect("ticket parsed from captured CLI fixture");

        assert_eq!(
            ticket.creator.as_ref(),
            Some(&WorkPerson {
                id: "creator_redacted".into(),
                name: "Example Creator".into(),
            })
        );
        let encoded = serde_json::to_string(&ticket).expect("serialize ticket");
        let decoded = serde_json::from_str::<WorkTicket>(&encoded).expect("deserialize ticket");
        assert_eq!(decoded, ticket);
    }

    #[test]
    fn linear_ticket_without_creator_parses_with_none() {
        let value = serde_json::json!({
            "identifier": "SCA-1",
            "title": "No creator in compact output"
        });
        let ticket =
            parse_linear_ticket(&value, TicketGroup::Assigned).expect("ticket without creator");

        assert_eq!(ticket.creator, None);
    }

    #[test]
    fn lean_pull_request_summaries_join_tickets_from_branch_only() {
        let dir = fixture_dir("ticket-pr-join");
        let (gh, linearis) = fake_programs(
            &dir,
            r#"#!/bin/sh
case "$*" in
  *"--author @me"*|*"review-requested:@me"*) printf '%s' '[]' ;;
  *) printf '%s' '[{"number":7,"title":"branch match","headRefName":"issue/sca-7-fix","url":"https://github.com/owner/repo/pull/7"},{"number":8,"title":"body unavailable in list","headRefName":"plain","url":"https://github.com/owner/repo/pull/8"}]' ;;
esac
"#,
            r#"#!/bin/sh
case "$*" in
  *"issues list"*) printf '%s' '{"nodes":[{"identifier":"SCA-7","title":"ticket","state":{"name":"In Progress"}}]}' ;;
  *"cycles list"*) printf '%s' '{"nodes":[]}' ;;
  *) printf '%s' '[]' ;;
esac
"#,
        );
        let snapshot = refresh_work_index(
            &config(),
            &[],
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );
        let linked = snapshot
            .items
            .iter()
            .filter(|item| item.pr_number.is_some())
            .collect::<Vec<_>>();
        assert_eq!(linked.len(), 2);
        assert_eq!(linked[0].ticket_ids, ["SCA-7"]);
        assert!(linked[1].ticket_ids.is_empty());
    }

    #[test]
    fn status_rollup_distinguishes_absent_empty_and_all_passing() {
        assert_eq!(status_check_summary(None), None);
        assert_eq!(status_check_summary(Some(&serde_json::json!([]))), None);
        assert_eq!(
            status_check_summary(Some(&serde_json::json!([
                {"conclusion": "SUCCESS"},
                {"conclusion": "NEUTRAL"},
                {"conclusion": null}
            ]))),
            Some(WorkItemCheckSummary {
                failing: 0,
                total: 3
            })
        );
        assert_eq!(
            status_check_summary(Some(&serde_json::json!([
                {"conclusion": "FAILURE"},
                {"conclusion": "TIMED_OUT"},
                {"conclusion": "CANCELLED"},
                {"conclusion": "ACTION_REQUIRED"},
                {"conclusion": "SUCCESS"}
            ]))),
            Some(WorkItemCheckSummary {
                failing: 4,
                total: 5
            })
        );
    }

    #[test]
    fn work_item_detail_cache_is_keyed_and_bounded() {
        let mut cache = WorkItemDetailCache::default();
        for number in 1..=17 {
            cache.insert(
                work_item_key(number),
                unavailable_detail(SystemTime::UNIX_EPOCH),
            );
        }

        assert_eq!(cache.len(), WORK_ITEM_DETAIL_CACHE_CAPACITY);
        assert!(cache.get(&work_item_key(1)).is_none());
        assert!(cache.get(&work_item_key(2)).is_some());
        assert!(cache.get(&work_item_key(17)).is_some());
    }

    #[test]
    fn work_item_detail_cache_freshness_uses_monotonic_time() {
        let mut cache = WorkItemDetailCache::default();
        let key = work_item_key(7);
        let refreshed_at = Instant::now();
        let wall_clock_in_future = SystemTime::now() + Duration::from_secs(3_600);
        cache.insert_at(
            key.clone(),
            unavailable_detail(wall_clock_in_future),
            refreshed_at,
        );

        assert!(cache.is_fresh(
            &key,
            refreshed_at + Duration::from_secs(59),
            Duration::from_secs(60),
        ));
        assert!(!cache.is_fresh(
            &key,
            refreshed_at + Duration::from_secs(60),
            Duration::from_secs(60),
        ));
    }

    #[test]
    fn stale_work_item_detail_generation_is_rejected() {
        let mut app = test_app_with_work_index();
        let current_key = work_item_key(8);
        app.last_work_item_detail_refresh_generation = 2;
        app.last_applied_work_item_detail_refresh_generation = 0;
        app.work_item_detail_refresh_in_flight = Some(WorkItemDetailRefreshInFlight {
            keys: vec![current_key.clone()],
            generation: 2,
            deadline: Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
        });
        app.state
            .work_item_detail_loading
            .insert(current_key.clone());

        assert!(!app.handle_work_item_detail_refreshed(
            1,
            vec![(work_item_key(7), unavailable_detail(SystemTime::UNIX_EPOCH),)],
        ));
        assert!(app
            .state
            .work_item_detail_cache
            .get(&work_item_key(7))
            .is_none());
        assert!(app.state.work_item_detail_loading.contains(&current_key));
    }

    #[test]
    fn work_index_refresh_invalidates_detail_cache_and_in_flight_generation() {
        let mut app = test_app_with_work_index();
        let key = work_item_key(8);
        app.state
            .work_item_detail_cache
            .insert(key.clone(), unavailable_detail(SystemTime::UNIX_EPOCH));
        app.state.work_item_detail_loading.insert(key.clone());
        app.work_item_detail_refresh_in_flight = Some(WorkItemDetailRefreshInFlight {
            keys: vec![key.clone()],
            generation: 4,
            deadline: Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
        });
        app.last_work_item_detail_refresh_generation = 4;
        app.invalidate_work_item_details();
        assert!(app.work_item_detail_refresh_in_flight.is_none());
        assert!(!app.state.work_item_detail_loading.contains(&key));
        assert!(app.state.work_item_detail_cache.get(&key).is_none());
        assert_eq!(app.last_work_item_detail_refresh_generation, 5);
        assert!(!app.handle_work_item_detail_refreshed(
            4,
            vec![(key, unavailable_detail(SystemTime::UNIX_EPOCH))],
        ));
    }

    #[test]
    fn one_bounded_batch_fetches_only_the_selected_pr_detail() {
        let mut app = test_app_with_work_index();
        app.work_index_gh_program_override = Some(Path::new("/usr/bin/false").to_path_buf());
        app.state.workspaces = vec![
            crate::workspace::Workspace::test_new("first"),
            crate::workspace::Workspace::test_new("second"),
        ];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        for (workspace_index, number) in [7_u64, 8].into_iter().enumerate() {
            let pane_id = app.state.workspaces[workspace_index].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[workspace_index]
                .terminal_id(pane_id)
                .expect("terminal")
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal state")
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    pr_urls: Some(vec![format!("https://github.com/owner/repo/pull/{number}")]),
                    ..Default::default()
                })
                .expect("work context");
        }
        let first_key = app.state.dock_home_selected_row().expect("first row").key;
        app.state.dock_home_selection = Some(first_key);
        app.state.move_dock_home_selection(1);
        let key = app
            .state
            .dock_home_selected_row()
            .expect("newly selected row")
            .key;
        assert_eq!(key.pr_number, Some(8));
        let now = Instant::now();

        app.start_work_item_detail_refresh_if_due(
            now,
            crate::app::state::DockHomeSection::Prs,
            Some(key.clone()),
            true,
        );
        app.start_work_item_detail_refresh_if_due(
            now,
            crate::app::state::DockHomeSection::Prs,
            Some(key.clone()),
            true,
        );

        assert_eq!(app.last_work_item_detail_refresh_generation, 1);
        let refresh = app
            .work_item_detail_refresh_in_flight
            .as_ref()
            .expect("detail batch");
        assert_eq!(refresh.generation, 1, "second call must not start a batch");
        assert_eq!(refresh.keys.len(), 1);
        assert_eq!(refresh.keys.first(), Some(&key));
        assert!(refresh
            .keys
            .iter()
            .all(|key| app.state.work_item_detail_loading.contains(key)));
    }

    #[test]
    fn pr_detail_refresh_does_not_prefetch_the_visible_list() {
        let mut app = test_app_with_work_index();
        app.work_index_gh_program_override = Some(Path::new("/usr/bin/false").to_path_buf());
        app.state.workspaces = (1_u64..=20)
            .map(|number| crate::workspace::Workspace::test_new(&format!("pr-{number}")))
            .collect();
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        for (workspace_index, number) in (1_u64..=20).enumerate() {
            let pane_id = app.state.workspaces[workspace_index].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[workspace_index]
                .terminal_id(pane_id)
                .expect("terminal")
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal state")
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    pr_urls: Some(vec![format!("https://github.com/owner/repo/pull/{number}")]),
                    ..Default::default()
                })
                .expect("work context");
        }

        app.start_work_item_detail_refresh_if_due(
            Instant::now(),
            crate::app::state::DockHomeSection::Prs,
            None,
            true,
        );

        assert_eq!(
            app.work_item_detail_refresh_in_flight
                .as_ref()
                .expect("detail batch")
                .keys
                .len(),
            1
        );
    }

    #[test]
    fn focused_pr_detail_refresh_is_debounced_for_thirty_seconds() {
        let dir = fixture_dir("focused-pr-detail-debounce");
        let log = dir.join("detail.log");
        let gh = dir.join("gh");
        write_executable(
            &gh,
            &format!(
                r#"#!/bin/sh
case "$*" in
  "pr view 7 --repo owner/repo --json {GITHUB_PULL_REQUEST_DETAIL_FIELDS}")
    printf '%s\n' detail >> '{}'
    printf '%s' '{{"number":7,"title":"Detail","url":"https://github.com/owner/repo/pull/7"}}'
    ;;
  "api repos/owner/repo/issues/7/timeline"*) printf '%s' '[]' ;;
  "api graphql"*) printf '%s' '{{"data":{{"repository":{{"pullRequest":{{"reviewThreads":{{"nodes":[]}}}}}}}}}}' ;;
  *) exit 42 ;;
esac
"#,
                log.display()
            ),
        );
        let mut app = test_app_with_work_index();
        app.work_index_gh_program_override = Some(gh);
        app.work_index_provider_cache_root_override = Some(dir.clone());
        let key = work_item_key(7);
        let now = Instant::now();

        app.start_work_item_detail_refresh_if_due(
            now,
            crate::app::state::DockHomeSection::Prs,
            Some(key.clone()),
            true,
        );
        let event = app.event_rx.blocking_recv().expect("first detail result");
        let crate::events::AppEvent::WorkItemDetailRefreshed {
            generation,
            details,
        } = event
        else {
            panic!("expected detail refresh event");
        };
        assert!(app.handle_work_item_detail_refreshed(generation, details));

        app.start_work_item_detail_refresh_if_due(
            now + Duration::from_secs(29),
            crate::app::state::DockHomeSection::Prs,
            Some(key.clone()),
            true,
        );
        assert!(app.work_item_detail_refresh_in_flight.is_none());
        assert_eq!(
            std::fs::read_to_string(&log).expect("first count"),
            "detail\n"
        );

        app.start_work_item_detail_refresh_if_due(
            now + Duration::from_secs(31),
            crate::app::state::DockHomeSection::Prs,
            Some(key),
            true,
        );
        let event = app.event_rx.blocking_recv().expect("second detail result");
        let crate::events::AppEvent::WorkItemDetailRefreshed {
            generation,
            details,
        } = event
        else {
            panic!("expected detail refresh event");
        };
        assert!(app.handle_work_item_detail_refreshed(generation, details));
        assert_eq!(
            std::fs::read_to_string(log).expect("second count"),
            "detail\ndetail\n"
        );
    }

    #[test]
    fn selected_ticket_outside_dock_projection_still_refreshes_detail() {
        let mut app = test_app_with_work_index();
        app.work_index_linearis_program_override = Some(Path::new("/usr/bin/false").to_path_buf());
        let key = crate::app::state::WorkItemKey {
            repo: String::new(),
            pr_number: None,
            pr_url: None,
            ticket_id: Some("SCA-3165".into()),
        };

        app.start_work_item_detail_refresh_if_due(
            Instant::now(),
            crate::app::state::DockHomeSection::Tickets,
            Some(key.clone()),
            true,
        );

        let refresh = app
            .work_item_detail_refresh_in_flight
            .as_ref()
            .expect("selected ticket detail batch");
        assert_eq!(refresh.keys, [key]);
    }

    #[test]
    fn detail_prefetch_does_not_rotate_beyond_the_cache_capacity() {
        let mut app = test_app_with_work_index();
        app.work_index_gh_program_override = Some(Path::new("/usr/bin/false").to_path_buf());
        app.state.workspaces = (1_u64..=20)
            .map(|number| crate::workspace::Workspace::test_new(&format!("pr-{number}")))
            .collect();
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        for (workspace_index, number) in (1_u64..=20).enumerate() {
            let pane_id = app.state.workspaces[workspace_index].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[workspace_index]
                .terminal_id(pane_id)
                .expect("terminal")
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal state")
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    pr_urls: Some(vec![format!("https://github.com/owner/repo/pull/{number}")]),
                    ..Default::default()
                })
                .expect("work context");
        }
        let keys = app
            .state
            .dock_home_keys_for_section(crate::app::state::DockHomeSection::Prs);
        for key in keys.iter().take(WORK_ITEM_DETAIL_CACHE_CAPACITY) {
            app.state
                .work_item_detail_cache
                .insert(key.clone(), unavailable_detail(SystemTime::now()));
        }
        let now = Instant::now();

        app.start_work_item_detail_refresh_if_due(
            now,
            crate::app::state::DockHomeSection::Prs,
            None,
            true,
        );

        assert!(app.work_item_detail_refresh_in_flight.is_none());
    }

    #[test]
    fn expired_detail_batch_is_cleared_while_detail_is_hidden() {
        let mut app = test_app_with_work_index();
        let key = work_item_key(8);
        app.state.work_item_detail_loading.insert(key.clone());
        app.work_item_detail_refresh_in_flight = Some(WorkItemDetailRefreshInFlight {
            keys: vec![key.clone()],
            generation: 1,
            deadline: Instant::now() - Duration::from_secs(1),
        });

        app.start_work_item_detail_refresh_if_due(
            Instant::now(),
            crate::app::state::DockHomeSection::Prs,
            Some(key.clone()),
            false,
        );

        assert!(app.work_item_detail_refresh_in_flight.is_none());
        assert!(!app.state.work_item_detail_loading.contains(&key));
    }

    #[test]
    fn attachment_join_keeps_orphan_buckets_and_ignores_branch_names() {
        let dir = fixture_dir("join");
        let (gh, linearis) = fake_programs(
            &dir,
            r#"#!/bin/sh
printf '%s' '[{"number":7,"title":"Attached PR","headRefName":"plain-branch","isDraft":false,"reviewDecision":"","url":"https://github.com/owner/repo/pull/7","createdAt":"2026-08-30T11:22:33Z"},{"number":8,"title":"No ticket PR","headRefName":"other-branch","isDraft":false,"reviewDecision":"","url":"https://github.com/owner/repo/pull/8"}]'
"#,
            r#"#!/bin/sh
case "$*" in
  *"issues list"*) printf '%s' '{"nodes":[{"identifier":"SCA-2","title":"Attached ticket","state":"In Progress","branchName":"plain-branch"},{"identifier":"SCA-3","title":"No PR ticket","state":"In Review","branchName":"SCA-3"}]}' ;;
  *"attachments list SCA-2"*) printf '%s' '[{"url":"https://github.com/owner/repo/pull/7","metadata":{"number":7,"repoName":"repo","repoLogin":"owner","status":"open","draft":false,"branch":"plain-branch","previewLinks":[]}}]' ;;
  *) printf '%s' '[]' ;;
esac
"#,
        );
        let snapshot = refresh_work_index(
            &config(),
            &[],
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );
        let joined = snapshot
            .items
            .iter()
            .find(|item| item.pr_number == Some(7))
            .expect("joined PR");
        assert_eq!(joined.ticket_ids, vec!["SCA-2"]);
        assert_eq!(joined.branch.as_deref(), Some("plain-branch"));
        assert_eq!(
            joined.created_at,
            parse_rfc3339_system_time("2026-08-30T11:22:33Z")
        );
        assert!(snapshot
            .items
            .iter()
            .any(|item| item.ticket_ids == ["SCA-3"]));
        assert!(snapshot
            .items
            .iter()
            .any(|item| item.pr_number == Some(8) && item.ticket_ids.is_empty()));
    }

    #[test]
    fn attachment_only_pull_request_keeps_its_title() {
        // A PR outside the configured repo allowlist is never enumerated by
        // `gh`, so its title can only come from the attachment payload.
        let dir = fixture_dir("attachment-title");
        let (gh, linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nprintf '%s' '[]'\n",
            r#"#!/bin/sh
case "$*" in
  *"issues list"*) printf '%s' '{"nodes":[{"identifier":"SCA-9","title":"Cross repo ticket","state":"In Progress","branchName":"sca-9"}]}' ;;
  *"attachments list SCA-9"*) printf '%s' '[{"url":"https://github.com/owner/other/pull/42","metadata":{"number":42,"title":"Titled from attachment","repoName":"other","repoLogin":"owner","status":"merged","draft":false,"branch":"sca-9","previewLinks":[]}}]' ;;
  *) printf '%s' '[]' ;;
esac
"#,
        );
        let snapshot = refresh_work_index(
            &config(),
            &[],
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );
        let item = snapshot
            .items
            .iter()
            .find(|item| item.pr_number == Some(42))
            .expect("attachment-only PR");
        assert_eq!(item.pr_title.as_deref(), Some("Titled from attachment"));
        assert_eq!(item.pr_state.as_deref(), Some("merged"));
        assert_eq!(item.ticket_ids, vec!["SCA-9"]);
    }

    #[test]
    fn linear_failure_keeps_the_github_half() {
        // A dead Linear half must degrade, not erase: the pull requests are
        // still real work and still worth showing.
        let dir = fixture_dir("linear-down");
        let (gh, linearis) = fake_programs(
            &dir,
            r#"#!/bin/sh
printf '%s' '[{"number":7,"title":"Live PR","headRefName":"b","isDraft":false,"reviewDecision":"","url":"https://github.com/owner/repo/pull/7"}]'
"#,
            "#!/bin/sh\nprintf '%s' '{\n  \"error\": \"No API token found.\"\n}' >&2\nexit 1\n",
        );
        let snapshot = refresh_work_index(
            &config(),
            &[],
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.items[0].pr_number, Some(7));
        let unavailable = snapshot.unavailable.expect("degraded message");
        let unavailable = unavailable.summary();
        // Collapsed to one line, so the cause is legible rather than "{".
        assert!(unavailable.contains("No API token found"), "{unavailable}");
        assert!(!unavailable.contains('\n'));
    }

    #[test]
    fn provider_failures_keep_previous_source_items_and_name_every_reason() {
        let dir = fixture_dir("all-sources-degraded");
        let (good_gh, good_linearis) = fake_programs(
            &dir,
            r#"#!/bin/sh
case "$*" in
  *"--author @me"*|*"review-requested:@me"*) printf '%s' '[]' ;;
  *) printf '%s' '[{"number":7,"title":"Previous PR","body":"","state":"OPEN","headRefName":"old","url":"https://github.com/owner/repo/pull/7"}]' ;;
esac
"#,
            r#"#!/bin/sh
case "$*" in
  *"issues list"*) printf '%s' '{"nodes":[{"identifier":"SCA-2","title":"Previous ticket","state":{"name":"In Progress"}}]}' ;;
  *"cycles list"*) printf '%s' '{"nodes":[]}' ;;
  "attachments list"*) printf '%s' '[]' ;;
  *) exit 42 ;;
esac
"#,
        );
        let mut previous = refresh_work_index(
            &config(),
            &[],
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &good_gh,
            &good_linearis,
        );
        previous.conversations.push(MissiveConversation {
            id: "previous".into(),
            subject: "Previous conversation".into(),
            app_url: "https://mail.missiveapp.com/#inbox/conversations/previous".into(),
            web_url: "https://mail.missiveapp.com/#inbox/conversations/previous".into(),
            team: None,
            assignees: Vec::new(),
            last_activity_at: None,
            closed: false,
            labels: Vec::new(),
            pane_bound: true,
            messages: Vec::new(),
            notes: Vec::new(),
            drafts: Vec::new(),
            posts: Vec::new(),
        });
        let (failed_gh, failed_linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nprintf '%s' 'github rate limited' >&2\nexit 1\n",
            "#!/bin/sh\nprintf '%s' 'linear rate limited' >&2\nexit 1\n",
        );
        let panes = panes_with_context(crate::work_context::PaneWorkContext {
            missive_urls: vec!["https://mail.missiveapp.com/#inbox/conversations/previous".into()],
            ..Default::default()
        });

        let snapshot = refresh_work_index_with_missive(
            &config(),
            &MissiveConfig::default(),
            &panes,
            WorkIndexRefreshContext {
                previous: Some(&previous),
                ..WorkIndexRefreshContext::default()
            },
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &failed_gh,
            &failed_linearis,
            Path::new("/usr/bin/false"),
        );

        assert!(snapshot
            .items
            .iter()
            .any(|item| item.pr_title.as_deref() == Some("Previous PR")));
        assert!(snapshot.items.iter().any(|item| {
            item.ticket_details
                .iter()
                .any(|ticket| ticket.title.as_deref() == Some("Previous ticket"))
        }));
        assert_eq!(snapshot.conversations[0].subject, "Previous conversation");
        assert!(snapshot
            .unavailable_reason(WorkIndexSource::Github)
            .is_some_and(|reason| reason.contains("github rate limited")));
        assert!(snapshot
            .unavailable_reason(WorkIndexSource::Linear)
            .is_some_and(|reason| reason.contains("linear rate limited")));
        assert_eq!(
            snapshot.unavailable_reason(WorkIndexSource::Missive),
            Some("team is not configured")
        );
    }

    #[test]
    fn linear_lists_use_compact_fields_and_increment_after_the_ten_minute_ttl() {
        let dir = fixture_dir("linear-incremental");
        let log = dir.join("argv.log");
        let (_gh, linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nprintf '%s' '[]'\n",
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$*" in
  *"--created-after 2026-09-07") printf '%s' '{{"nodes":[{{"identifier":"SCA-2","title":"new","state":{{"name":"Triage"}}}}]}}' ;;
  *) printf '%s' '{{"nodes":[{{"identifier":"SCA-1","title":"full","state":{{"name":"Triage"}}}}]}}' ;;
esac
"#,
                log.display()
            ),
        );
        let base = parse_rfc3339_system_time("2026-09-07T08:00:00Z").expect("base time");
        let first_cache = ProviderCache::at_time(&dir, base);
        let first = fetch_linear_ticket_group(
            "SCA",
            &linearis,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
            TicketGroup::Triage,
            &["--status", "Triage"],
            true,
            Some(&first_cache),
            false,
        )
        .expect("full query");
        assert_eq!(first.len(), 1);

        let incremental_cache = ProviderCache::at_time(&dir, base + Duration::from_secs(10 * 60));
        let merged = fetch_linear_ticket_group(
            "SCA",
            &linearis,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
            TicketGroup::Triage,
            &["--status", "Triage"],
            true,
            Some(&incremental_cache),
            false,
        )
        .expect("incremental query");
        assert_eq!(
            merged
                .iter()
                .map(|ticket| ticket.identifier.as_str())
                .collect::<Vec<_>>(),
            ["SCA-1", "SCA-2"]
        );
        let calls = std::fs::read_to_string(log).expect("argv log");
        assert_eq!(calls.lines().count(), 2);
        assert!(calls.lines().all(|call| call.contains(LINEAR_LIST_FIELDS)));
        assert!(calls.contains("--created-after 2026-09-07"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn selected_ticket_is_read_even_when_lists_do_not_contain_it() {
        let dir = fixture_dir("selected-ticket-refresh");
        let log = dir.join("argv.log");
        let (gh, linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nprintf '%s' '[]'\n",
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$*" in
  *"issues read SCA-77") printf '%s' '{{"identifier":"SCA-77","title":"selected","state":{{"name":"In Progress"}}}}' ;;
  *"issues list"*|*"cycles list"*) printf '%s' '{{"nodes":[]}}' ;;
  *"attachments list"*) printf '%s' '[]' ;;
  *) exit 42 ;;
esac
"#,
                log.display()
            ),
        );
        let mut config = config();
        config.repos.clear();
        let snapshot = refresh_work_index_with_missive(
            &config,
            &MissiveConfig::default(),
            &[],
            WorkIndexRefreshContext {
                selected_ticket: Some("SCA-77"),
                ..WorkIndexRefreshContext::default()
            },
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
            Path::new("/usr/bin/false"),
        );
        assert!(snapshot.items.iter().any(|item| {
            item.ticket_details
                .iter()
                .any(|ticket| ticket.identifier == "SCA-77")
        }));
        let calls = std::fs::read_to_string(log).expect("argv log");
        assert!(calls.lines().any(|call| {
            call.contains(LINEAR_ITEM_FIELDS) && call.ends_with("issues read SCA-77")
        }));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn declared_ticket_already_listed_is_still_refetched() {
        let dir = fixture_dir("declared-ticket-already-listed");
        let log = dir.join("linear-argv.log");
        let linearis_script = r##"#!/bin/sh
printf '%s\n' "$*" >> "{linear_log}"
if echo "$*" | grep -q 'issues read SCA-77'; then
  printf '%s' '{{"identifier":"SCA-77","title":"detailed","state":{"name":"In Progress"}}}'
elif echo "$*" | grep -q 'issues list'; then
  printf '%s' '{{"nodes":[{"identifier":"SCA-77","title":"listed","state":{"name":"Triage"}}]}}'
elif echo "$*" | grep -q 'cycles list'; then
  printf '%s' '{{"nodes":[]}}'
elif echo "$*" | grep -q 'attachments list'; then
  printf '%s' '[]'
else
  exit 42
fi
"##
        .replace("{linear_log}", &log.display().to_string());
        let (gh, linearis) = fake_programs(&dir, "#!/bin/sh\nprintf '%s' '[]'\n", &linearis_script);
        let mut config = config();
        config.repos.clear();
        let panes = panes_with_context(crate::work_context::PaneWorkContext {
            ticket_ids: vec!["SCA-77".into()],
            ..Default::default()
        });
        let first_cache = ProviderCache::at_time(
            &dir,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        );
        let first = refresh_work_index_with_missive(
            &config,
            &MissiveConfig::default(),
            &panes,
            WorkIndexRefreshContext {
                provider_cache: Some(&first_cache),
                ..WorkIndexRefreshContext::default()
            },
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
            Path::new("/usr/bin/false"),
        );
        let second_cache = ProviderCache::at_time(
            &dir,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + 10 * 60 + 1),
        );
        let _ = refresh_work_index_with_missive(
            &config,
            &MissiveConfig::default(),
            &panes,
            WorkIndexRefreshContext {
                previous: Some(&first),
                provider_cache: Some(&second_cache),
                ..WorkIndexRefreshContext::default()
            },
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
            Path::new("/usr/bin/false"),
        );

        let calls = std::fs::read_to_string(log).expect("Linear argv");
        assert_eq!(
            calls
                .lines()
                .filter(|call| call.contains("issues read SCA-77"))
                .count(),
            2
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn degradation_text_is_short_but_keeps_the_raw_reason() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let raw = r#"{"error":"Rate limit exceeded. Only 2500 requests are allowed per 1 hour"}"#;
        let mut unavailable = WorkIndexUnavailable::only(WorkIndexSource::Linear, raw);
        unavailable
            .record_rate_limit_until(WorkIndexSource::Linear, now + Duration::from_secs(12 * 60));
        assert_eq!(
            unavailable
                .short_reason(WorkIndexSource::Linear, now)
                .as_deref(),
            Some("rate limited · retry in 12m")
        );
        assert_eq!(unavailable.reason(WorkIndexSource::Linear), Some(raw));

        let previous = Snapshot {
            items: Vec::new(),
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: Some(unavailable.clone()),
            observed_at: now,
        };
        let mut next = WorkIndexUnavailable::only(WorkIndexSource::Linear, "rate limited");
        preserve_previous_rate_limit_reasons(&mut next, Some(&previous));
        assert_eq!(next.reason(WorkIndexSource::Linear), Some(raw));

        let timeout = WorkIndexUnavailable::only(
            WorkIndexSource::Github,
            "pull request observation timed out",
        );
        assert_eq!(
            timeout
                .short_reason(WorkIndexSource::Github, now)
                .as_deref(),
            Some("timed out (30s)")
        );
        let missive =
            WorkIndexUnavailable::only(WorkIndexSource::Missive, "Missive team is not configured");
        assert_eq!(
            missive
                .short_reason(WorkIndexSource::Missive, now)
                .as_deref(),
            Some("team not configured")
        );
        let group = WorkIndexUnavailable::only(
            WorkIndexSource::Linear,
            "Triage: Linear observation failed: opaque provider JSON",
        );
        assert_eq!(
            group.short_reason(WorkIndexSource::Linear, now).as_deref(),
            Some("Triage query failed (see log)")
        );
    }

    #[test]
    fn nonzero_github_exit_sets_unavailable() {
        let dir = fixture_dir("gh-failure");
        let (gh, linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nexit 1\n",
            "#!/bin/sh\nprintf '%s' '{\"nodes\":[]}'\n",
        );
        let snapshot = refresh_work_index(
            &config(),
            &[],
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );
        assert!(snapshot.items.is_empty());
        assert!(snapshot.unavailable.is_some());
    }

    #[test]
    fn timeout_is_no_observation() {
        let dir = fixture_dir("timeout");
        let (gh, linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nsleep 2\n",
            "#!/bin/sh\nprintf '%s' '{\"nodes\":[]}'\n",
        );
        let snapshot = refresh_work_index(
            &config(),
            &[],
            Instant::now(),
            Instant::now() + Duration::from_secs(1),
            Duration::from_millis(50),
            &gh,
            &linearis,
        );
        assert!(snapshot.items.is_empty());
        assert_eq!(
            snapshot.unavailable_reason(WorkIndexSource::Github),
            Some("list observation timed out")
        );
    }

    #[test]
    fn linear_timeout_is_no_observation() {
        let dir = fixture_dir("linear-timeout");
        let (gh, linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nprintf '%s' '[]'\n",
            "#!/bin/sh\nsleep 2\n",
        );
        let mut linear_only_config = config();
        linear_only_config.repos.clear();
        let snapshot = refresh_work_index(
            &linear_only_config,
            &[],
            Instant::now(),
            Instant::now() + Duration::from_secs(1),
            Duration::from_millis(50),
            &gh,
            &linearis,
        );

        assert!(snapshot.items.is_empty());
        assert_eq!(
            snapshot.unavailable_reason(WorkIndexSource::Linear),
            Some("observation timed out")
        );
    }

    #[test]
    fn snapshot_write_is_valid_json_at_explicit_path() {
        let dir = fixture_dir("write");
        let path = dir.join("nested/work-index.json");
        let snapshot = Snapshot {
            items: Vec::new(),
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: SystemTime::now(),
        };
        write_snapshot(&path, &snapshot).expect("write snapshot");
        let value: Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("read snapshot"))
                .expect("valid JSON");
        assert!(value.get("items").is_some());
    }

    #[test]
    fn snapshot_path_is_scoped_to_session_name_including_default() {
        let state_dir = Path::new("/tmp/herdr-state");
        assert_eq!(
            work_index_snapshot_path_for(state_dir, None),
            state_dir.join("work-index/default.json")
        );
        assert_eq!(
            work_index_snapshot_path_for(state_dir, Some("customer-support")),
            state_dir.join("work-index/customer-support.json")
        );
    }

    #[test]
    fn cold_load_reads_only_its_session_snapshot_and_ignores_legacy_file() {
        let dir = fixture_dir("session-snapshot-isolation");
        let alpha_path = work_index_snapshot_path_for(&dir, Some("alpha"));
        let beta_path = work_index_snapshot_path_for(&dir, Some("beta"));
        let legacy_path = dir.join("work-index.json");
        let snapshot = |reason| Snapshot {
            items: Vec::new(),
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: Some(WorkIndexUnavailable::only(WorkIndexSource::Github, reason)),
            observed_at: SystemTime::UNIX_EPOCH,
        };
        write_snapshot(&alpha_path, &snapshot("alpha")).expect("alpha snapshot");
        write_snapshot(&beta_path, &snapshot("beta")).expect("beta snapshot");
        write_snapshot(&legacy_path, &snapshot("legacy")).expect("legacy snapshot");

        assert_eq!(
            load_snapshot(&alpha_path)
                .and_then(|snapshot| snapshot.unavailable_summary())
                .as_deref(),
            Some("GitHub: alpha")
        );
        assert_eq!(
            load_snapshot(&beta_path)
                .and_then(|snapshot| snapshot.unavailable_summary())
                .as_deref(),
            Some("GitHub: beta")
        );
        assert!(load_snapshot(&work_index_snapshot_path_for(&dir, None)).is_none());
    }

    #[test]
    fn load_snapshot_round_trips_a_written_snapshot() {
        let dir = fixture_dir("load-round-trip");
        let path = dir.join("work-index.json");
        let snapshot = Snapshot {
            items: Vec::new(),
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: Some(WorkIndexUnavailable::only(
                WorkIndexSource::Linear,
                "observation timed out",
            )),
            observed_at: SystemTime::now(),
        };
        write_snapshot(&path, &snapshot).expect("write snapshot");

        let loaded = load_snapshot(&path).expect("snapshot loads back");

        assert_eq!(loaded, snapshot);
    }

    #[test]
    fn load_snapshot_returns_none_for_missing_file() {
        let dir = fixture_dir("load-missing");
        let path = dir.join("does-not-exist.json");

        assert!(load_snapshot(&path).is_none());
    }

    #[test]
    fn load_snapshot_returns_none_for_corrupt_file() {
        let dir = fixture_dir("load-corrupt");
        let path = dir.join("work-index.json");
        std::fs::write(&path, b"not valid json").expect("write corrupt fixture");

        assert!(load_snapshot(&path).is_none());
    }
}

/// A write a human asked for against a work item.
///
/// Each variant maps to exactly one CLI invocation. They are kept together so
/// the confirm-then-run flow has a single vocabulary, and so it is obvious at a
/// glance which of them change something outside herdr.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkItemWrite {
    CommentOnPullRequest {
        repo: String,
        number: u64,
        body: String,
    },
    CommentOnTicket {
        identifier: String,
        body: String,
    },
    TransitionTicket {
        identifier: String,
        state: String,
    },
    AssignTicket {
        identifier: String,
        assignee: String,
    },
    SetTicketPriority {
        identifier: String,
        priority: u8,
    },
    LinkTicketPullRequest {
        identifier: String,
        title: String,
        url: String,
    },
    ApprovePullRequest {
        repo: String,
        number: u64,
    },
    MergePullRequest {
        repo: String,
        number: u64,
    },
    ClosePullRequest {
        repo: String,
        number: u64,
    },
    MarkPullRequestDraft {
        repo: String,
        number: u64,
    },
    MarkPullRequestReady {
        repo: String,
        number: u64,
    },
    AddPullRequestReviewer {
        repo: String,
        number: u64,
        login: String,
    },
    SetPullRequestAutoMerge {
        repo: String,
        number: u64,
        enabled: bool,
        method: crate::config::MergeMethodConfig,
    },
}

impl WorkItemWrite {
    /// What the user is about to do, for the confirmation line. Phrased as the
    /// action and its target so a mis-aimed write is visible before it runs.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::CommentOnPullRequest { repo, number, .. } => {
                format!("comment on {repo}#{number}")
            }
            Self::CommentOnTicket { identifier, .. } => format!("comment on {identifier}"),
            Self::TransitionTicket { identifier, state } => {
                format!("move {identifier} to {state}")
            }
            Self::AssignTicket {
                identifier,
                assignee,
            } => format!("assign {identifier} to {assignee}"),
            Self::SetTicketPriority {
                identifier,
                priority,
            } => format!("set {identifier} priority to P{priority}"),
            Self::LinkTicketPullRequest { identifier, .. } => {
                format!("link pull request to {identifier}")
            }
            Self::ApprovePullRequest { repo, number } => format!("approve {repo}#{number}"),
            Self::MergePullRequest { repo, number } => {
                format!("squash-merge {repo}#{number}")
            }
            Self::ClosePullRequest { repo, number } => format!("close {repo}#{number}"),
            Self::MarkPullRequestDraft { repo, number } => {
                format!("mark {repo}#{number} draft")
            }
            Self::MarkPullRequestReady { repo, number } => {
                format!("mark {repo}#{number} ready")
            }
            Self::AddPullRequestReviewer {
                repo,
                number,
                login,
            } => format!("request review from {login} on {repo}#{number}"),
            Self::SetPullRequestAutoMerge {
                repo,
                number,
                enabled,
                ..
            } => format!(
                "{} auto-merge for {repo}#{number}",
                if *enabled { "enable" } else { "disable" }
            ),
        }
    }

    /// The work item this write targets, so its cached detail can be dropped
    /// once the write lands and the next refresh shows the result.
    pub(crate) fn target(&self) -> Option<crate::app::state::WorkItemKey> {
        match self {
            Self::CommentOnPullRequest { repo, number, .. }
            | Self::ApprovePullRequest { repo, number }
            | Self::MergePullRequest { repo, number }
            | Self::ClosePullRequest { repo, number }
            | Self::MarkPullRequestDraft { repo, number }
            | Self::MarkPullRequestReady { repo, number }
            | Self::AddPullRequestReviewer { repo, number, .. }
            | Self::SetPullRequestAutoMerge { repo, number, .. } => {
                Some(crate::app::state::WorkItemKey {
                    repo: repo.clone(),
                    pr_number: Some(*number),
                    pr_url: None,
                    ticket_id: None,
                })
            }
            Self::CommentOnTicket { identifier, .. }
            | Self::TransitionTicket { identifier, .. }
            | Self::AssignTicket { identifier, .. }
            | Self::SetTicketPriority { identifier, .. }
            | Self::LinkTicketPullRequest { identifier, .. } => {
                Some(crate::app::state::WorkItemKey {
                    repo: String::new(),
                    pr_number: None,
                    pr_url: None,
                    ticket_id: Some(identifier.clone()),
                })
            }
        }
    }
}

/// Run one authorized write. Returns the message to show the user either way.
pub(crate) fn run_work_item_write(
    write: &WorkItemWrite,
    gh_program: &Path,
    linearis_program: &Path,
    deadline: Instant,
) -> Result<String, String> {
    let (command, stdin) = match write {
        WorkItemWrite::CommentOnPullRequest { repo, number, body } => {
            let mut command = crate::noninteractive_process::command(gh_program);
            // `--body-file -` rather than `--body`: a multi-line markdown comment
            // has no business going through argv quoting or its length limit.
            command.args([
                "pr",
                "comment",
                &number.to_string(),
                "-R",
                repo,
                "--body-file",
                "-",
            ]);
            (command, Some(body.clone().into_bytes()))
        }
        WorkItemWrite::CommentOnTicket { identifier, body } => {
            let mut command = crate::noninteractive_process::command(linearis_program);
            command.args(["issues", "discuss", identifier, "--body", body]);
            (command, None)
        }
        WorkItemWrite::TransitionTicket { identifier, state } => {
            let mut command = crate::noninteractive_process::command(linearis_program);
            command.args(["issues", "update", identifier, "--status", state]);
            (command, None)
        }
        WorkItemWrite::AssignTicket {
            identifier,
            assignee,
        } => {
            let mut command = crate::noninteractive_process::command(linearis_program);
            command.args(["issues", "update", identifier, "--assignee", assignee]);
            (command, None)
        }
        WorkItemWrite::SetTicketPriority {
            identifier,
            priority,
        } => {
            let mut command = crate::noninteractive_process::command(linearis_program);
            command.args([
                "issues",
                "update",
                identifier,
                "--priority",
                &priority.to_string(),
            ]);
            (command, None)
        }
        WorkItemWrite::LinkTicketPullRequest {
            identifier,
            title,
            url,
        } => {
            let mut command = crate::noninteractive_process::command(linearis_program);
            command.args([
                "attachments",
                "create",
                identifier,
                "--title",
                title,
                "--url",
                url,
            ]);
            (command, None)
        }
        WorkItemWrite::ApprovePullRequest { repo, number } => {
            let mut command = crate::noninteractive_process::command(gh_program);
            command.args(["pr", "review", &number.to_string(), "-R", repo, "--approve"]);
            (command, None)
        }
        WorkItemWrite::MergePullRequest { repo, number } => {
            let mut command = crate::noninteractive_process::command(gh_program);
            command.args(["pr", "merge", &number.to_string(), "-R", repo, "--squash"]);
            (command, None)
        }
        WorkItemWrite::ClosePullRequest { repo, number } => {
            let mut command = crate::noninteractive_process::command(gh_program);
            command.args(["pr", "close", &number.to_string(), "-R", repo]);
            (command, None)
        }
        WorkItemWrite::MarkPullRequestDraft { repo, number } => {
            let mut command = crate::noninteractive_process::command(gh_program);
            command.args(["pr", "ready", "--undo", &number.to_string(), "-R", repo]);
            (command, None)
        }
        WorkItemWrite::MarkPullRequestReady { repo, number } => {
            let mut command = crate::noninteractive_process::command(gh_program);
            command.args(["pr", "ready", &number.to_string(), "-R", repo]);
            (command, None)
        }
        WorkItemWrite::AddPullRequestReviewer {
            repo,
            number,
            login,
        } => {
            let mut command = crate::noninteractive_process::command(gh_program);
            command.args([
                "pr",
                "edit",
                &number.to_string(),
                "--add-reviewer",
                login,
                "-R",
                repo,
            ]);
            (command, None)
        }
        WorkItemWrite::SetPullRequestAutoMerge {
            repo,
            number,
            enabled,
            method,
        } => {
            let mut command = crate::noninteractive_process::command(gh_program);
            if *enabled {
                command.args([
                    "pr",
                    "merge",
                    &number.to_string(),
                    "--auto",
                    method.flag(),
                    "-R",
                    repo,
                ]);
            } else {
                command.args([
                    "pr",
                    "merge",
                    &number.to_string(),
                    "--disable-auto",
                    "-R",
                    repo,
                ]);
            }
            (command, None)
        }
    };

    let output = match stdin {
        Some(stdin) => {
            crate::noninteractive_process::output_with_stdin_and_deadline(command, stdin, deadline)
        }
        None => crate::noninteractive_process::output_with_deadline(command, deadline),
    }
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::TimedOut {
            format!("{} timed out", write.describe())
        } else {
            format!("{} could not be run", write.describe())
        }
    })?;

    if output.status.success() {
        return Ok(format!("{} done", write.describe()));
    }
    // The CLI's own message says why far better than a generic failure would.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let reason = stderr
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("no reason given")
        .trim()
        .to_string();
    Err(format!("{} failed: {reason}", write.describe()))
}

#[cfg(all(test, unix))]
mod work_item_write_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn recorder() -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "herdr-linear-write-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).expect("create recorder directory");
        let program = root.join("linearis");
        let log = root.join("argv.log");
        std::fs::write(
            &program,
            format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
        )
        .expect("write recorder");
        let mut permissions = std::fs::metadata(&program)
            .expect("recorder metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&program, permissions).expect("make recorder executable");
        (program, log)
    }

    #[test]
    fn ticket_writes_map_to_linearis_argv() {
        let (linearis, log) = recorder();
        let deadline = Instant::now() + Duration::from_secs(2);
        run_work_item_write(
            &WorkItemWrite::TransitionTicket {
                identifier: "SCA-7".into(),
                state: "In Review".into(),
            },
            Path::new("/usr/bin/false"),
            &linearis,
            deadline,
        )
        .expect("transition command");
        run_work_item_write(
            &WorkItemWrite::LinkTicketPullRequest {
                identifier: "SCA-7".into(),
                title: "owner/repo#42".into(),
                url: "https://github.com/owner/repo/pull/42".into(),
            },
            Path::new("/usr/bin/false"),
            &linearis,
            deadline,
        )
        .expect("link command");
        run_work_item_write(
            &WorkItemWrite::AssignTicket {
                identifier: "SCA-7".into(),
                assignee: "viewer-id".into(),
            },
            Path::new("/usr/bin/false"),
            &linearis,
            deadline,
        )
        .expect("assign command");
        run_work_item_write(
            &WorkItemWrite::SetTicketPriority {
                identifier: "SCA-7".into(),
                priority: 1,
            },
            Path::new("/usr/bin/false"),
            &linearis,
            deadline,
        )
        .expect("priority command");
        run_work_item_write(
            &WorkItemWrite::CommentOnTicket {
                identifier: "SCA-7".into(),
                body: "ship it".into(),
            },
            Path::new("/usr/bin/false"),
            &linearis,
            deadline,
        )
        .expect("comment command");
        run_work_item_write(
            &WorkItemWrite::TransitionTicket {
                identifier: "SCA-7".into(),
                state: "Canceled".into(),
            },
            Path::new("/usr/bin/false"),
            &linearis,
            deadline,
        )
        .expect("cancel command");

        let argv = std::fs::read_to_string(log).expect("read recorder log");
        assert!(argv.contains("issues update SCA-7 --status In Review"));
        assert!(argv.contains(
            "attachments create SCA-7 --title owner/repo#42 --url https://github.com/owner/repo/pull/42"
        ));
        assert!(argv.contains("issues update SCA-7 --assignee viewer-id"));
        assert!(argv.contains("issues update SCA-7 --priority 1"));
        assert!(argv.contains("issues discuss SCA-7 --body ship it"));
        assert!(argv.contains("issues update SCA-7 --status Canceled"));
    }

    #[test]
    fn pr_state_writes_map_to_gh_argv() {
        let (gh, log) = recorder();
        let deadline = Instant::now() + Duration::from_secs(2);
        for write in [
            WorkItemWrite::ClosePullRequest {
                repo: "owner/repo".into(),
                number: 42,
            },
            WorkItemWrite::MarkPullRequestDraft {
                repo: "owner/repo".into(),
                number: 42,
            },
            WorkItemWrite::MarkPullRequestReady {
                repo: "owner/repo".into(),
                number: 42,
            },
            WorkItemWrite::AddPullRequestReviewer {
                repo: "owner/repo".into(),
                number: 42,
                login: "grace".into(),
            },
            WorkItemWrite::SetPullRequestAutoMerge {
                repo: "owner/repo".into(),
                number: 42,
                enabled: true,
                method: crate::config::MergeMethodConfig::Rebase,
            },
            WorkItemWrite::SetPullRequestAutoMerge {
                repo: "owner/repo".into(),
                number: 42,
                enabled: false,
                method: crate::config::MergeMethodConfig::Merge,
            },
        ] {
            run_work_item_write(&write, &gh, Path::new("/usr/bin/false"), deadline)
                .expect("pull request state command");
        }

        assert_eq!(
            std::fs::read_to_string(log).expect("read recorder log"),
            [
                "pr close 42 -R owner/repo",
                "pr ready --undo 42 -R owner/repo",
                "pr ready 42 -R owner/repo",
                "pr edit 42 --add-reviewer grace -R owner/repo",
                "pr merge 42 --auto --rebase -R owner/repo",
                "pr merge 42 --disable-auto -R owner/repo",
                "",
            ]
            .join("\n")
        );
    }
}
