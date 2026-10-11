//! Account-level subscription usage for the top status bar.
//!
//! Four providers, one shape: a 5-hour window and a 7-day window per account,
//! each a used percentage plus the instant it resets. The numbers describe the
//! *account*, not the focused pane, because the quota is what every agent on
//! that profile shares.
//!
//! Every source is already on disk or already installed. Claude Code writes its
//! own rate-limit payload to the statusline cache, Codex records its limits in
//! each rollout, and `kimi-usage` and `agy-usage` reshape their plans into the
//! same fields.
//! Nothing here talks to a provider API, so a dead network costs the bar a dim
//! segment rather than a stalled frame.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashMap},
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Claude's cache is written by whichever Claude Code session last rendered its
/// statusline. Past this age the numbers describe a window that may already
/// have rolled over, so the bar dims them instead of asserting them.
pub(crate) const CLAUDE_CACHE_STALE_AFTER: Duration = Duration::from_secs(30 * 60);
const KIMI_TIMEOUT: Duration = Duration::from_secs(5);
const KIMI_OUTPUT_LIMIT: usize = 64 * 1024;
const AGY_TIMEOUT: Duration = Duration::from_secs(5);
const AGY_OUTPUT_LIMIT: usize = 64 * 1024;
const FIVE_HOUR_MINUTES: u64 = 300;
const SEVEN_DAY_MINUTES: u64 = 10_080;
const MAX_USAGE_FILES: usize = 64;
const MAX_USAGE_DIRECTORIES: usize = 256;
const MAX_USAGE_ENTRIES: usize = 4096;
const MAX_USAGE_FILE_BYTES: u64 = 512 * 1024;
const MAX_CREDIT_BALANCE: f64 = 1_000_000.0;
const MAX_CCUSAGE_DISCOVERY_ENTRIES: usize = 256;
const MAX_CCUSAGE_OUTPUT_BYTES: usize = 512 * 1024;
const CCUSAGE_TIMEOUT: Duration = Duration::from_secs(10);
const CCUSAGE_FAILURE_RETRY_INTERVAL: Duration = Duration::from_secs(30 * 60);
const CCUSAGE_SUCCESS_CACHE_INTERVAL: Duration = Duration::from_secs(10 * 60);
const MAX_CLAUDE_USAGE_AMOUNT: f64 = 1_000_000.0;
const MAX_CLAUDE_REMAINING_MINUTES: u64 = 24 * 60;
const MAX_PROVIDER_PROFILES: usize = 32;
const MAX_PROFILE_METADATA_BYTES: u64 = 64 * 1024;
const MAX_CODEX_QUOTA_CACHE_ENTRIES: usize = MAX_PROVIDER_PROFILES * MAX_USAGE_FILES * 2;

#[derive(Debug, Clone, Default, PartialEq)]
struct CodexRateLimits {
    windows: Vec<CodexUsageWindow>,
    credits: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
struct CodexUsageWindow {
    used_percent: f64,
    window_minutes: u64,
    resets_at: i64,
}

#[derive(Debug, Clone)]
struct CachedCodexRateLimits {
    fingerprint: UsageFileFingerprint,
    rate_limits: Option<CodexRateLimits>,
}

#[derive(Debug, serde::Deserialize)]
struct RawRateLimits {
    primary: Option<RawUsageWindow>,
    secondary: Option<RawUsageWindow>,
    credits: Option<RawCredits>,
}

#[derive(Debug, serde::Deserialize)]
struct RawUsageWindow {
    used_percent: f64,
    window_minutes: u64,
    resets_at: i64,
}

#[derive(Debug, serde::Deserialize)]
struct RawCredits {
    balance: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
struct RawCcusageResponse {
    blocks: Vec<RawCcusageBlock>,
}

#[derive(Debug, serde::Deserialize)]
struct RawCcusageBlock {
    #[serde(rename = "isActive")]
    is_active: bool,
    #[serde(rename = "endTime")]
    end_time: Option<String>,
    #[serde(rename = "costUSD")]
    cost_usd: Option<f64>,
    projection: Option<RawCcusageProjection>,
}

#[derive(Debug, serde::Deserialize)]
struct RawCcusageProjection {
    #[serde(rename = "remainingMinutes")]
    remaining_minutes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ClaudeUsageDetails {
    cost_usd: f64,
    remaining_minutes: Option<u64>,
}

#[derive(Debug, Default)]
struct CcusageCache {
    last_success: Option<(Instant, ClaudeUsageDetails)>,
    retry_at: Option<Instant>,
}

impl CcusageCache {
    fn poll(
        &mut self,
        now: Instant,
        attempt: impl FnOnce() -> Result<ClaudeUsageDetails, ()>,
    ) -> Option<ClaudeUsageDetails> {
        if let Some((refreshed_at, details)) = self.last_success {
            if let Some(age) = now
                .checked_duration_since(refreshed_at)
                .filter(|age| *age < CCUSAGE_SUCCESS_CACHE_INTERVAL)
            {
                // The block keeps counting down while its cost is reused.
                let elapsed_minutes = age.as_secs() / 60;
                return Some(ClaudeUsageDetails {
                    remaining_minutes: details
                        .remaining_minutes
                        .map(|minutes| minutes.saturating_sub(elapsed_minutes)),
                    ..details
                });
            }
        }
        if self.retry_at.is_some_and(|retry_at| now < retry_at) {
            return None;
        }
        match attempt() {
            Ok(details) => {
                self.retry_at = None;
                self.last_success = Some((now, details));
                Some(details)
            }
            Err(()) => {
                self.retry_at = now.checked_add(CCUSAGE_FAILURE_RETRY_INTERVAL);
                None
            }
        }
    }
}

fn parse_codex_record(line: &str) -> Option<CodexRateLimits> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let rate_limits = value
        .get("payload")
        .and_then(|payload| payload.get("rate_limits"))
        .or_else(|| value.get("rate_limits"))?;
    let raw: RawRateLimits = serde_json::from_value(rate_limits.clone()).ok()?;
    let mut windows = [raw.primary, raw.secondary]
        .into_iter()
        .filter_map(|raw| raw.and_then(parse_usage_window))
        .collect::<Vec<_>>();
    windows.sort_unstable_by_key(|window| window.window_minutes);
    windows.dedup_by_key(|window| window.window_minutes);
    let credits = raw
        .credits
        .and_then(|credits| credits.balance)
        .and_then(parse_credit_balance);
    (!windows.is_empty()).then_some(CodexRateLimits { windows, credits })
}

fn parse_credit_balance(value: serde_json::Value) -> Option<f64> {
    let balance = match value {
        serde_json::Value::Number(number) => number.as_f64()?,
        serde_json::Value::String(value) => value.parse().ok()?,
        _ => return None,
    };
    (balance.is_finite() && (0.0..=MAX_CREDIT_BALANCE).contains(&balance)).then_some(balance)
}

fn parse_usage_window(raw: RawUsageWindow) -> Option<CodexUsageWindow> {
    if !raw.used_percent.is_finite()
        || !(0.0..=100.0).contains(&raw.used_percent)
        || ![FIVE_HOUR_MINUTES, SEVEN_DAY_MINUTES].contains(&raw.window_minutes)
        || raw.resets_at <= 0
    {
        return None;
    }
    Some(CodexUsageWindow {
        used_percent: raw.used_percent,
        window_minutes: raw.window_minutes,
        resets_at: raw.resets_at,
    })
}

fn usage_window(usage: &CodexRateLimits, minutes: u64) -> Option<&CodexUsageWindow> {
    usage
        .windows
        .iter()
        .find(|window| window.window_minutes == minutes)
}

fn parse_utc_timestamp(value: &str) -> Option<i64> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|value| value.unix_timestamp())
}

fn parse_ccusage_output(output: &str, now: i64) -> Result<Option<ClaudeUsageDetails>, ()> {
    let response: RawCcusageResponse = serde_json::from_str(output).map_err(|_| ())?;
    let Some(block) = response.blocks.into_iter().find(|block| block.is_active) else {
        return Ok(None);
    };
    let cost_usd = block
        .cost_usd
        .filter(|cost| cost.is_finite() && (0.0..=MAX_CLAUDE_USAGE_AMOUNT).contains(cost))
        .ok_or(())?;
    let resets_at = parse_utc_timestamp(block.end_time.as_deref().ok_or(())?).ok_or(())?;
    if resets_at <= now {
        return Ok(None);
    }
    let remaining_minutes = block.projection.ok_or(())?.remaining_minutes;
    let remaining_minutes =
        (remaining_minutes <= MAX_CLAUDE_REMAINING_MINUTES).then_some(remaining_minutes);
    Ok(Some(ClaudeUsageDetails {
        cost_usd,
        remaining_minutes,
    }))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn discover_ccusage() -> Option<PathBuf> {
    let path_candidate = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join("ccusage"))
        .find(|path| is_executable_file(path));
    if path_candidate.is_some() {
        return path_candidate;
    }

    if let Some(path) = home_path(".local/bin/ccusage") {
        if is_executable_file(&path) {
            return Some(path);
        }
    }

    let root = home_path(".local/state/fnm_multishells")?;
    let Ok(entries) = fs::read_dir(root) else {
        return None;
    };
    for (index, entry) in entries.enumerate() {
        if index >= MAX_CCUSAGE_DISCOVERY_ENTRIES {
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let Some(candidate) = entry
            .file_type()
            .ok()
            .is_some_and(|file_type| file_type.is_dir())
            .then(|| entry.path().join("bin/ccusage"))
        else {
            continue;
        };
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn resolve_ccusage() -> Option<PathBuf> {
    static CCUSAGE_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    CCUSAGE_PATH.get_or_init(discover_ccusage).clone()
}

fn fetch_claude_usage_details(now: i64) -> Result<ClaudeUsageDetails, ()> {
    let binary = resolve_ccusage().ok_or(())?;
    let mut command = crate::noninteractive_process::command(binary);
    command.args(["blocks", "--active", "--json", "--offline"]);
    let output = crate::noninteractive_process::output_with_deadline_limited(
        command,
        Instant::now() + CCUSAGE_TIMEOUT,
        MAX_CCUSAGE_OUTPUT_BYTES,
    )
    .map_err(|_| ())?;
    if !output.status.success() {
        return Err(());
    }
    let output = String::from_utf8(output.stdout).map_err(|_| ())?;
    parse_ccusage_output(&output, now)?.ok_or(())
}

fn load_claude_usage_details(now_unix: i64, now: Instant) -> Option<ClaudeUsageDetails> {
    static CCUSAGE_CACHE: OnceLock<Mutex<CcusageCache>> = OnceLock::new();
    CCUSAGE_CACHE
        .get_or_init(|| Mutex::new(CcusageCache::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .poll(now, || fetch_claude_usage_details(now_unix))
}

fn recent_jsonl_files(root: &Path, max_files: usize) -> Result<Vec<PathBuf>, ()> {
    if max_files == 0 {
        return Ok(Vec::new());
    }
    let root_metadata = fs::metadata(root).map_err(|_| ())?;
    if !root_metadata.is_dir() {
        return Err(());
    }
    let root_mtime = root_metadata.modified().unwrap_or(UNIX_EPOCH);
    let mut directories = vec![(root_mtime, root.to_path_buf(), 0usize)];
    let mut visited_directories = 0usize;
    let mut visited_entries = 0usize;
    let mut files = Vec::new();
    while let Some((_, directory, depth)) = directories.pop() {
        if visited_directories >= MAX_USAGE_DIRECTORIES || visited_entries >= MAX_USAGE_ENTRIES {
            break;
        }
        visited_directories = visited_directories.saturating_add(1);
        let entries = fs::read_dir(directory).map_err(|_| ())?;
        let remaining_entries = MAX_USAGE_ENTRIES.saturating_sub(visited_entries);
        let mut child_directories = Vec::new();
        for entry in entries.take(remaining_entries) {
            let entry = entry.map_err(|_| ())?;
            visited_entries = visited_entries.saturating_add(1);
            let path = entry.path();
            let file_type = entry.file_type().map_err(|_| ())?;
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(UNIX_EPOCH);
            if file_type.is_dir() && depth < 4 {
                child_directories.push((modified, path, depth.saturating_add(1)));
            } else if file_type.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "jsonl")
            {
                files.push((modified, path));
                files.sort_unstable_by_key(|entry| Reverse(entry.0));
                files.truncate(max_files);
            }
        }
        child_directories.sort_unstable_by_key(|entry| Reverse(entry.0));
        directories.extend(child_directories.into_iter().rev());
    }
    Ok(files.into_iter().map(|(_, path)| path).collect())
}

fn read_file_tail(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(MAX_USAGE_FILE_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_USAGE_FILE_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    let text = String::from_utf8_lossy(&bytes);
    if start == 0 {
        return Some(text.into_owned());
    }
    text.find('\n')
        .map(|newline| text[newline.saturating_add(1)..].to_owned())
}

fn home_path(directory: &str) -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|home| home.join(directory))
}

/// Cached dashboard aggregates live beside the other local Herdr state.
pub(crate) const USAGE_CACHE_FILE: &str = "usage-cache.json";
const USAGE_CACHE_VERSION: u32 = 1;

/// Provider attached to one locally observed token-usage record.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UsageProvider {
    ClaudeCode,
    Codex,
    /// Proves that consumers handle a newly collected provider without adding
    /// a provider that production does not collect yet.
    #[cfg(test)]
    TestCollected,
}

impl UsageProvider {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            #[cfg(test)]
            Self::TestCollected => "Test Collected",
        }
    }

    pub(crate) fn series_index(self) -> usize {
        match self {
            Self::ClaudeCode => 0,
            Self::Codex => 1,
            #[cfg(test)]
            Self::TestCollected => 2,
        }
    }
}

/// One billable response or turn, normalised across local provider logs.
///
/// `input_tokens` excludes cached reads. Claude reports all four buckets
/// separately. Codex reports cached reads inside `input_tokens`, so its parser
/// subtracts that bucket before storing the sample.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct UsageSample {
    pub provider: UsageProvider,
    pub session_id: String,
    pub timestamp: i64,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
}

impl UsageSample {
    pub(crate) fn processed_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_write_tokens)
            .saturating_add(self.cache_read_tokens)
    }
}

/// Complete dashboard input produced by one filesystem scan.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct UsageSnapshot {
    pub samples: Vec<UsageSample>,
    /// Files present below the two provider roots during this scan.
    pub files_seen: usize,
    /// Files parsed rather than restored from the fingerprint cache.
    pub files_read: usize,
}

impl UsageSnapshot {
    pub(crate) fn samples_between(
        &self,
        start: i64,
        end: i64,
    ) -> impl Iterator<Item = &UsageSample> {
        self.samples
            .iter()
            .filter(move |sample| sample.timestamp >= start && sample.timestamp < end)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct UsageFileFingerprint {
    modified_seconds: u64,
    modified_nanos: u32,
    size: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CachedUsageFile {
    fingerprint: UsageFileFingerprint,
    samples: Vec<UsageSample>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct UsageCache {
    version: u32,
    files: BTreeMap<PathBuf, CachedUsageFile>,
}

/// One quota window, normalised across providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QuotaWindow {
    pub used_percent: u8,
    /// Unix seconds. `None` when the provider reports a window without a reset,
    /// which Kimi does for a five-hour window that has not started.
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsageUnavailable {
    NoStatuslineData,
    NoSessionData,
    HelperMissing,
    HelperTimedOut,
    HelperFailed,
    HelperUnreadable,
    NoQuotaSource,
    UnknownProfile,
    UnknownAccount,
    AuthExpired,
}

fn unavailable(reason: UsageUnavailable) -> AccountUsage {
    AccountUsage {
        unavailable: Some(reason),
        ..AccountUsage::default()
    }
}

/// A single provider account: its short label and its windows.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct AccountUsage {
    pub unavailable: Option<UsageUnavailable>,
    /// Short account code, e.g. `SHQ`. `None` when the account cannot be named.
    pub account: Option<String>,
    /// Full account email, when the provider's local credentials name it.
    pub email: Option<String>,
    pub five_hour: Option<QuotaWindow>,
    pub seven_day: Option<QuotaWindow>,
    /// Remaining provider credits, when the provider reports a balance.
    pub credits: Option<f64>,
    /// Cost of the current Claude five-hour window, when reported by ccusage.
    pub cost_usd: Option<f64>,
    /// Minutes remaining in the current Claude five-hour window.
    pub remaining_minutes: Option<u64>,
    /// Unix seconds when the local provider source was last updated, when known.
    pub last_refresh_unix: Option<i64>,
    /// The source is older than its freshness budget. Values render dimmed.
    pub stale: bool,
}

impl AccountUsage {
    pub(crate) fn is_empty(&self) -> bool {
        self.five_hour.is_none() && self.seven_day.is_none()
    }

    /// The hottest window, which decides whether the account escalates.
    pub(crate) fn peak_percent(&self) -> Option<u8> {
        [self.five_hour, self.seven_day]
            .into_iter()
            .flatten()
            .map(|window| window.used_percent)
            .max()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum QuotaProvider {
    Claude,
    Codex,
    Kimi,
    Agy,
    OpenCode,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProviderAccountUsage {
    pub provider: QuotaProvider,
    pub profile_id: String,
    pub label: String,
    pub usage: AccountUsage,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ProviderUsageSnapshot {
    pub collected_unix: Option<i64>,
    pub accounts: Vec<ProviderAccountUsage>,
    primary_claude: String,
    primary_codex: String,
    primary_kimi: String,
    primary_agy: String,
    primary_opencode: String,
}

impl ProviderUsageSnapshot {
    #[cfg(test)]
    pub(crate) fn with_primary_accounts(
        claude: AccountUsage,
        codex: AccountUsage,
        kimi: AccountUsage,
    ) -> Self {
        Self {
            accounts: vec![
                ProviderAccountUsage {
                    provider: QuotaProvider::Claude,
                    profile_id: "default".into(),
                    label: "Claude Code".into(),
                    usage: claude,
                },
                ProviderAccountUsage {
                    provider: QuotaProvider::Codex,
                    profile_id: "default".into(),
                    label: "Codex".into(),
                    usage: codex,
                },
                ProviderAccountUsage {
                    provider: QuotaProvider::Kimi,
                    profile_id: "default".into(),
                    label: "Kimi".into(),
                    usage: kimi,
                },
            ],
            collected_unix: None,
            primary_claude: "default".into(),
            primary_codex: "default".into(),
            primary_kimi: "default".into(),
            primary_agy: "default".into(),
            primary_opencode: "default".into(),
        }
    }

    pub(crate) fn primary(&self, provider: QuotaProvider) -> Option<&ProviderAccountUsage> {
        let profile_id = match provider {
            QuotaProvider::Claude => &self.primary_claude,
            QuotaProvider::Codex => &self.primary_codex,
            QuotaProvider::Kimi => &self.primary_kimi,
            QuotaProvider::Agy => &self.primary_agy,
            QuotaProvider::OpenCode => &self.primary_opencode,
        };
        self.accounts
            .iter()
            .find(|account| account.provider == provider && account.profile_id == *profile_id)
    }

    pub(crate) fn primary_usage(&self, provider: QuotaProvider) -> &AccountUsage {
        static EMPTY: OnceLock<AccountUsage> = OnceLock::new();
        self.primary(provider)
            .map(|account| &account.usage)
            .unwrap_or_else(|| EMPTY.get_or_init(AccountUsage::default))
    }

    #[cfg(test)]
    pub(crate) fn primary_usage_mut(&mut self, provider: QuotaProvider) -> &mut AccountUsage {
        let profile_id = match provider {
            QuotaProvider::Claude => &mut self.primary_claude,
            QuotaProvider::Codex => &mut self.primary_codex,
            QuotaProvider::Kimi => &mut self.primary_kimi,
            QuotaProvider::Agy => &mut self.primary_agy,
            QuotaProvider::OpenCode => &mut self.primary_opencode,
        };
        if profile_id.is_empty() {
            *profile_id = "default".into();
        }
        let index = self
            .accounts
            .iter()
            .position(|account| account.provider == provider && account.profile_id == *profile_id)
            .unwrap_or_else(|| {
                self.accounts.push(ProviderAccountUsage {
                    provider,
                    profile_id: profile_id.clone(),
                    label: profile_id.clone(),
                    usage: AccountUsage::default(),
                });
                self.accounts.len() - 1
            });
        &mut self.accounts[index].usage
    }
}

/// Collects every provider. Blocking: callers run it off the render thread.
pub(crate) fn collect(now_unix: Option<i64>, now: Instant) -> ProviderUsageSnapshot {
    let (claude, primary_claude) = load_all_claude_usage(now_unix, now);
    let (codex, primary_codex) = load_all_codex_usage(now_unix);
    let kimi = ProviderAccountUsage {
        provider: QuotaProvider::Kimi,
        profile_id: "default".into(),
        label: "Kimi".into(),
        usage: load_kimi_usage(now_unix),
    };
    let agy_usage = load_agy_usage(now_unix);
    let mut accounts = claude;
    accounts.extend(codex);
    accounts.push(kimi);
    // OpenCode has no account quota cache contract. Keep it visible without
    // borrowing another provider's limits or inventing usage percentages.
    accounts.push(ProviderAccountUsage {
        provider: QuotaProvider::OpenCode,
        profile_id: "default".into(),
        label: "default".into(),
        usage: unavailable(UsageUnavailable::NoQuotaSource),
    });
    {
        accounts.push(ProviderAccountUsage {
            provider: QuotaProvider::Agy,
            profile_id: "default".into(),
            label: "Antigravity".into(),
            usage: agy_usage,
        });
    }
    ProviderUsageSnapshot {
        collected_unix: now_unix,
        accounts,
        primary_claude,
        primary_codex,
        primary_kimi: "default".into(),
        primary_agy: "default".into(),
        primary_opencode: "default".into(),
    }
}

/// Loads the last complete scan without touching provider logs, so opening the
/// dashboard can paint immediately while its background refresh runs.
pub(crate) fn load_cached_usage() -> Option<UsageSnapshot> {
    let cache = read_usage_cache(&crate::config::state_dir().join(USAGE_CACHE_FILE));
    if cache.version != USAGE_CACHE_VERSION {
        return None;
    }
    let mut snapshot = UsageSnapshot {
        files_seen: cache.files.len(),
        files_read: 0,
        samples: cache
            .files
            .into_values()
            .flat_map(|file| file.samples)
            .collect(),
    };
    sort_usage_samples(&mut snapshot.samples);
    Some(snapshot)
}

/// Background-job entry point for a fresh scan of both provider histories.
pub(crate) fn scan_historical_usage() -> Result<UsageSnapshot, String> {
    let home = home_path("").ok_or_else(|| "home directory is not set".to_string())?;
    Ok(collect_dashboard_usage_from(
        &home,
        &crate::config::state_dir().join(USAGE_CACHE_FILE),
    ))
}

/// Explicit-root form used by tests and isolated runtime fixtures.
pub(crate) fn collect_dashboard_usage_from(home: &Path, cache_path: &Path) -> UsageSnapshot {
    let mut discovered = Vec::new();
    for (provider, root) in [
        (UsageProvider::ClaudeCode, home.join(".claude/projects")),
        (UsageProvider::Codex, home.join(".codex/sessions")),
    ] {
        if let Err(error) = discover_jsonl_files(provider, &root, &mut discovered) {
            tracing::warn!(path = %root.display(), %error, "failed to discover provider usage logs");
        }
    }
    discovered.sort_by(|left, right| left.1.cmp(&right.1));

    let previous = read_usage_cache(cache_path);
    let mut next_files = BTreeMap::new();
    let mut snapshot = UsageSnapshot {
        files_seen: discovered.len(),
        ..UsageSnapshot::default()
    };
    for (provider, path) in discovered {
        let Ok(fingerprint) = usage_file_fingerprint(&path) else {
            continue;
        };
        let (fingerprint, samples) = if let Some(cached) = previous
            .files
            .get(&path)
            .filter(|cached| cached.fingerprint == fingerprint)
        {
            (fingerprint, cached.samples.clone())
        } else {
            snapshot.files_read = snapshot.files_read.saturating_add(1);
            let mut samples = parse_usage_file(provider, &path);
            let mut stable = usage_file_fingerprint(&path).unwrap_or(fingerprint.clone());
            if stable != fingerprint {
                samples = parse_usage_file(provider, &path);
                stable = usage_file_fingerprint(&path).unwrap_or(stable);
            }
            (stable, samples)
        };
        snapshot.samples.extend(samples.iter().cloned());
        next_files.insert(
            path,
            CachedUsageFile {
                fingerprint,
                samples,
            },
        );
    }
    sort_usage_samples(&mut snapshot.samples);

    let cache = UsageCache {
        version: USAGE_CACHE_VERSION,
        files: next_files,
    };
    if let Err(error) = write_usage_cache(cache_path, &cache) {
        tracing::warn!(path = %cache_path.display(), %error, "failed to persist provider usage cache");
    }
    snapshot
}

fn sort_usage_samples(samples: &mut [UsageSample]) {
    samples.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.provider.cmp(&right.provider))
            .then_with(|| left.session_id.cmp(&right.session_id))
            .then_with(|| left.model.cmp(&right.model))
    });
}

fn discover_jsonl_files(
    provider: UsageProvider,
    root: &Path,
    files: &mut Vec<(UsageProvider, PathBuf)>,
) -> io::Result<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            if let Err(error) = discover_jsonl_files(provider, &path, files) {
                tracing::warn!(path = %path.display(), %error, "failed to inspect provider usage directory");
            }
        } else if file_type.is_file() && path.extension().is_some_and(|value| value == "jsonl") {
            files.push((provider, path));
        }
    }
    Ok(())
}

fn usage_file_fingerprint(path: &Path) -> io::Result<UsageFileFingerprint> {
    let metadata = fs::metadata(path)?;
    let modified = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Ok(UsageFileFingerprint {
        modified_seconds: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
        size: metadata.len(),
    })
}

fn read_usage_cache(path: &Path) -> UsageCache {
    let Ok(contents) = fs::read(path) else {
        return UsageCache::default();
    };
    let Ok(cache) = serde_json::from_slice::<UsageCache>(&contents) else {
        return UsageCache::default();
    };
    if cache.version == USAGE_CACHE_VERSION {
        cache
    } else {
        UsageCache::default()
    }
}

fn write_usage_cache(path: &Path, cache: &UsageCache) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("usage cache path has no parent"))?;
    fs::create_dir_all(parent)?;
    let contents = serde_json::to_vec_pretty(cache).map_err(io::Error::other)?;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("json.{}.{}.tmp", std::process::id(), unique));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    if let Err(error) = file.write_all(&contents).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    drop(file);
    if let Err(error) = crate::platform::replace_file_durably(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

fn parse_usage_file(provider: UsageProvider, path: &Path) -> Vec<UsageSample> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let session_id = usage_session_id(path);
    match provider {
        UsageProvider::ClaudeCode => parse_claude_usage_samples(&contents, &session_id),
        UsageProvider::Codex => parse_codex_usage_samples(&contents, &session_id),
        #[cfg(test)]
        UsageProvider::TestCollected => Vec::new(),
    }
}

fn usage_session_id(path: &Path) -> String {
    let claude_parent_session = path
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|name| name == "subagents"))
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty());
    if let Some(session_id) = claude_parent_session {
        return session_id.to_owned();
    }
    path.file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

fn json_u64(value: Option<&serde_json::Value>) -> u64 {
    value.and_then(serde_json::Value::as_u64).unwrap_or(0)
}

fn usage_timestamp(value: Option<&serde_json::Value>) -> Option<i64> {
    let value = value?;
    if let Some(timestamp) = value.as_i64() {
        return Some(if timestamp > 10_000_000_000 {
            timestamp / 1_000
        } else {
            timestamp
        });
    }
    value.as_str().and_then(parse_utc_timestamp)
}

fn nonempty_sample(sample: &UsageSample) -> bool {
    sample.processed_tokens() > 0
}

/// Parses Claude Code assistant records. The fields used are
/// `message.model`, `message.usage.{input_tokens,output_tokens,
/// cache_creation_input_tokens,cache_read_input_tokens}`, and top-level
/// `timestamp`. Session identity comes from the JSONL path.
pub(crate) fn parse_claude_usage_samples(contents: &str, session_id: &str) -> Vec<UsageSample> {
    contents
        .lines()
        .filter_map(|line| {
            let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
            let message = value.get("message")?;
            let usage = message.get("usage")?;
            let sample = UsageSample {
                provider: UsageProvider::ClaudeCode,
                session_id: session_id.to_owned(),
                timestamp: usage_timestamp(value.get("timestamp"))?,
                model: message
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned(),
                input_tokens: json_u64(usage.get("input_tokens")),
                output_tokens: json_u64(usage.get("output_tokens")),
                cache_write_tokens: json_u64(usage.get("cache_creation_input_tokens")),
                cache_read_tokens: json_u64(usage.get("cache_read_input_tokens")),
            };
            nonempty_sample(&sample).then_some(sample)
        })
        .collect()
}

fn codex_sample(
    usage: &serde_json::Value,
    timestamp: i64,
    model: &str,
    session_id: &str,
) -> UsageSample {
    let reported_input = json_u64(usage.get("input_tokens"));
    let cache_read_tokens = json_u64(usage.get("cached_input_tokens"));
    let cache_write_tokens = json_u64(usage.get("cache_write_input_tokens"));
    UsageSample {
        provider: UsageProvider::Codex,
        session_id: session_id.to_owned(),
        timestamp,
        model: model.to_owned(),
        input_tokens: reported_input.saturating_sub(cache_read_tokens),
        output_tokens: json_u64(usage.get("output_tokens")),
        cache_write_tokens,
        cache_read_tokens,
    }
}

/// Parses Codex rollout records. Modern logs use incremental
/// `token_usage_record.payload.usage` values joined to
/// `turn_context.payload.model` by turn id. Older logs use
/// `event_msg/token_count.info.last_token_usage`; those are read only when a
/// file has no modern records because current rollouts contain both forms.
pub(crate) fn parse_codex_usage_samples(contents: &str, session_id: &str) -> Vec<UsageSample> {
    let mut models_by_turn = HashMap::new();
    let mut current_model: Option<String> = None;
    let mut modern = Vec::new();
    let mut legacy = Vec::new();

    for line in contents.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let record_type = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let payload = value.get("payload");
        if record_type == "turn_context" {
            let turn_id = payload
                .and_then(|payload| payload.get("turn_id"))
                .and_then(serde_json::Value::as_str);
            let model = payload
                .and_then(|payload| payload.get("model"))
                .and_then(serde_json::Value::as_str);
            if let Some(model) = model {
                current_model = Some(model.to_owned());
                if let Some(turn_id) = turn_id {
                    models_by_turn.insert(turn_id.to_owned(), model.to_owned());
                }
            }
            continue;
        }

        let Some(timestamp) = usage_timestamp(value.get("timestamp")) else {
            continue;
        };
        if record_type == "token_usage_record" {
            let Some(payload) = payload else {
                continue;
            };
            let Some(usage) = payload.get("usage") else {
                continue;
            };
            let model = payload
                .get("turn_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|turn_id| models_by_turn.get(turn_id))
                .or(current_model.as_ref())
                .map(String::as_str)
                .unwrap_or("unknown");
            let sample = codex_sample(usage, timestamp, model, session_id);
            if nonempty_sample(&sample) {
                modern.push(sample);
            }
            continue;
        }

        let legacy_usage = (record_type == "event_msg")
            .then_some(payload)
            .flatten()
            .filter(|payload| {
                payload.get("type").and_then(serde_json::Value::as_str) == Some("token_count")
            })
            .and_then(|payload| payload.get("info"))
            .and_then(|info| info.get("last_token_usage"));
        if let Some(usage) = legacy_usage {
            let model = current_model.as_deref().unwrap_or("unknown");
            let sample = codex_sample(usage, timestamp, model, session_id);
            if nonempty_sample(&sample) {
                legacy.push(sample);
            }
        }
    }

    if modern.is_empty() {
        legacy
    } else {
        modern
    }
}

fn statusline_cache_dir() -> PathBuf {
    std::env::var_os("CLAUDE_STATUSLINE_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/claude-statusline"))
}

fn named_profile_dirs(root: &Path, marker_files: &[&str]) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut profiles = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let id = entry.file_name().to_string_lossy().into_owned();
            if id.starts_with(".bak-")
                || id.is_empty()
                || !marker_files
                    .iter()
                    .any(|marker| entry.path().join(marker).is_file())
            {
                return None;
            }
            Some((id, entry.path()))
        })
        .collect::<Vec<_>>();
    profiles.sort_by(|left, right| left.0.cmp(&right.0));
    profiles.truncate(MAX_PROVIDER_PROFILES);
    profiles
}

fn active_profile_id(env_var: &str, profiles: &[(String, PathBuf)]) -> String {
    let Some(active) = std::env::var_os(env_var).map(PathBuf::from) else {
        return "default".into();
    };
    profiles
        .iter()
        .find(|(_, path)| *path == active)
        .map(|(id, _)| id.clone())
        .unwrap_or_else(|| "default".into())
}

/// `matthias@scalablehq.com` → `scalablehq.com` → `SHQ`.
///
/// The map is the readable part: an unmapped domain still gets a code rather
/// than disappearing, so a new account is legible the day it is added.
pub(crate) fn account_code(domain: &str) -> Option<String> {
    let domain = domain.trim().trim_start_matches("DOMAIN=").trim();
    if domain.is_empty() {
        return None;
    }
    let known = [
        ("scalablehq.com", "SHQ"),
        ("scalable.so", "SSO"),
        ("machete-ventures.com", "MV"),
        ("mable.ai", "MA"),
    ];
    if let Some((_, code)) = known.iter().find(|(known, _)| *known == domain) {
        return Some((*code).to_string());
    }
    let stem = domain.split('.').next().unwrap_or(domain);
    let code: String = stem
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(3)
        .collect();
    (!code.is_empty()).then(|| code.to_uppercase())
}

/// Largest `.claude.json` worth reading for one display label.
const MAX_CLAUDE_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

/// The Claude account label: the statusline's own domain cache when a profile
/// is active, the signed-in account otherwise.
///
/// The fallback matters more than it looks. Agents are launched with per-pane
/// profile directories, so the server process itself usually has no
/// `CLAUDE_CONFIG_DIR`, and without it the row would show a quota with no
/// indication of whose it is.
fn claude_account_code(profile: Option<&str>, config_dir: Option<&Path>) -> Option<String> {
    if let Some(code) = config_dir.and_then(claude_profile_account_code) {
        return Some(code);
    }
    if let Some(profile) = profile {
        let path = statusline_cache_dir().join(format!("acctdom-claude-{profile}.env"));
        if let Ok(contents) = std::fs::read_to_string(path) {
            if let Some(code) = contents
                .lines()
                .find_map(|line| line.strip_prefix("DOMAIN=").and_then(account_code))
            {
                return Some(code);
            }
        }
    }
    signed_in_claude_account_code(config_dir)
}

fn claude_profile_account_code(config_dir: &Path) -> Option<String> {
    email_account_code(&claude_profile_email(config_dir)?)
}

fn email_account_code(email: &str) -> Option<String> {
    account_code(email.split_once('@')?.1)
}

/// The account email in a profile's `meta.env`, when it names one.
fn claude_profile_email(config_dir: &Path) -> Option<String> {
    let path = config_dir.join("meta.env");
    if fs::metadata(&path).ok()?.len() > MAX_PROFILE_METADATA_BYTES {
        return None;
    }
    let contents = fs::read_to_string(path).ok()?;
    contents.lines().find_map(|line| {
        let email = line
            .strip_prefix("EMAIL=")?
            .trim()
            .trim_matches(['\'', '"']);
        email.contains('@').then(|| email.to_owned())
    })
}

fn signed_in_claude_account_code(config_dir: Option<&Path>) -> Option<String> {
    email_account_code(&signed_in_claude_email(config_dir)?)
}

fn signed_in_claude_email(config_dir: Option<&Path>) -> Option<String> {
    let path = config_dir
        .map(|dir| dir.join(".claude.json"))
        .or_else(|| home_path(".claude.json"))?;
    if std::fs::metadata(&path).ok()?.len() > MAX_CLAUDE_CONFIG_BYTES {
        return None;
    }
    let contents = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let email = value
        .get("oauthAccount")?
        .get("emailAddress")?
        .as_str()?
        .to_owned();
    email.contains('@').then_some(email)
}

/// Full Claude account email: the profile's own record first, then the
/// signed-in account. The domain-only status cache cannot name an email.
fn claude_account_email(config_dir: Option<&Path>) -> Option<String> {
    config_dir
        .and_then(claude_profile_email)
        .or_else(|| signed_in_claude_email(config_dir))
}

/// Parses the `KEY=value` snapshot the statusline writes after every render.
pub(crate) fn parse_claude_rate_limits(
    contents: &str,
    now_unix: Option<i64>,
    age: Option<Duration>,
) -> AccountUsage {
    let mut fields = std::collections::HashMap::new();
    for line in contents.lines() {
        if let Some((key, value)) = line.split_once('=') {
            fields.insert(key.trim(), value.trim());
        }
    }
    let percent = |key: &str| {
        fields
            .get(key)
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| (0..=100).contains(value))
            .map(|value| value as u8)
    };
    let reset = |key: &str| {
        fields.get(key).and_then(|value| {
            value
                .parse::<i64>()
                .ok()
                .or_else(|| parse_utc_timestamp(value))
        })
    };
    let window = |percent_key: &str, reset_key: &str| {
        let used_percent = percent(percent_key)?;
        let resets_at = reset(reset_key).filter(|resets_at| *resets_at > 0);
        // An elapsed reset means the window rolled over and nobody has rendered
        // a statusline since; the percentage belongs to a window that is gone.
        if let (Some(resets_at), Some(now)) = (resets_at, now_unix) {
            if resets_at <= now {
                return None;
            }
        }
        Some(QuotaWindow {
            used_percent,
            resets_at,
        })
    };

    AccountUsage {
        unavailable: if fields.get("AUTH") == Some(&"refused") {
            Some(UsageUnavailable::AuthExpired)
        } else if window("R5", "R5_RST").is_none() && window("R7", "R7_RST").is_none() {
            Some(UsageUnavailable::NoStatuslineData)
        } else {
            None
        },
        account: None,
        five_hour: window("R5", "R5_RST"),
        seven_day: window("R7", "R7_RST"),
        credits: None,
        stale: age.is_some_and(|age| age >= CLAUDE_CACHE_STALE_AFTER),
        ..AccountUsage::default()
    }
}

fn load_claude_usage_from(
    path: &Path,
    profile: Option<&str>,
    config_dir: Option<&Path>,
    now_unix: Option<i64>,
    now: Instant,
) -> AccountUsage {
    let mut usage = std::fs::read_to_string(path).map_or_else(
        |_| unavailable(UsageUnavailable::NoStatuslineData),
        |contents| parse_claude_rate_limits(&contents, now_unix, file_age(path, now)),
    );
    let default_dir = home_path(".claude");
    if let Some(dir) = config_dir.or(default_dir.as_deref()) {
        if read_profile_metadata(&dir.join(".auth-state")).is_some()
            && usage.unavailable == Some(UsageUnavailable::AuthExpired)
        {
            usage.unavailable = if usage.is_empty() {
                Some(UsageUnavailable::NoStatuslineData)
            } else {
                None
            };
        }
    }
    if config_dir.or(default_dir.as_deref()).is_some_and(|dir| {
        claude_auth_expired(
            dir,
            now_unix,
            usage.unavailable == Some(UsageUnavailable::AuthExpired),
        )
    }) {
        usage.unavailable = Some(UsageUnavailable::AuthExpired);
    }
    usage.account = claude_account_code(profile, config_dir);
    usage.email = claude_account_email(config_dir);
    usage.last_refresh_unix = file_modified_unix(path);
    usage
}

/// Read only bounded metadata; credential values never leave this function.
fn claude_auth_expired(dir: &Path, now: Option<i64>, env_refused: bool) -> bool {
    let auth_state = read_profile_metadata(&dir.join(".auth-state"));
    let refused = auth_state.as_deref().map_or(env_refused, |text| {
        text.lines().any(|line| line.trim() == "AUTH=refused")
    });
    if refused {
        return true;
    }
    let Some(now) = now else {
        return false;
    };
    let Some(text) = read_profile_metadata(&dir.join(".credentials.json")) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    let Some(oauth) = value.get("claudeAiOauth") else {
        return false;
    };
    oauth
        .get("expiresAt")
        .and_then(serde_json::Value::as_i64)
        .is_some_and(|at| at < now.saturating_mul(1000))
        && oauth
            .get("refreshToken")
            .and_then(serde_json::Value::as_str)
            .is_none_or(str::is_empty)
}

fn read_profile_metadata(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?.take(MAX_PROFILE_METADATA_BYTES + 1);
    let mut text = String::new();
    file.read_to_string(&mut text).ok()?;
    (text.len() as u64 <= MAX_PROFILE_METADATA_BYTES).then_some(text)
}

fn load_all_claude_usage(
    now_unix: Option<i64>,
    now: Instant,
) -> (Vec<ProviderAccountUsage>, String) {
    let profiles = home_path(".claude-profiles")
        .map(|root| named_profile_dirs(&root, &["meta.env"]))
        .unwrap_or_default();
    let primary = active_profile_id("CLAUDE_CONFIG_DIR", &profiles);
    let details = now_unix.and_then(|now_unix| load_claude_usage_details(now_unix, now));
    let mut default_usage = load_claude_usage_from(
        &statusline_cache_dir().join("rate-limits.env"),
        None,
        None,
        now_unix,
        now,
    );
    if let Some(details) = details {
        // ccusage follows the server's active Claude environment, so these
        // optional details belong only to the selected primary account.
        if primary == "default" {
            default_usage.cost_usd = Some(details.cost_usd);
            default_usage.remaining_minutes = details.remaining_minutes;
        }
    }
    let default_label = default_usage
        .account
        .clone()
        .unwrap_or_else(|| "Default".into());
    let mut accounts = vec![ProviderAccountUsage {
        provider: QuotaProvider::Claude,
        profile_id: "default".into(),
        label: default_label,
        usage: default_usage,
    }];
    for (profile_id, config_dir) in profiles {
        let path = statusline_cache_dir().join(format!("rate-limits-{profile_id}.env"));
        let mut usage =
            load_claude_usage_from(&path, Some(&profile_id), Some(&config_dir), now_unix, now);
        if profile_id == primary {
            if let Some(details) = details {
                usage.cost_usd = Some(details.cost_usd);
                usage.remaining_minutes = details.remaining_minutes;
            }
        }
        let label = usage.account.clone().unwrap_or_else(|| profile_id.clone());
        accounts.push(ProviderAccountUsage {
            provider: QuotaProvider::Claude,
            profile_id,
            label,
            usage,
        });
    }
    (accounts, primary)
}

fn file_age(path: &Path, _now: Instant) -> Option<Duration> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
}

fn file_modified_unix(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs()
        .try_into()
        .ok()
}

/// The Codex account behind one `CODEX_HOME`, read from the `id_token`
/// the CLI already stores. No network call and no token is ever logged.
fn codex_account_code(root: &Path) -> Option<String> {
    email_account_code(&codex_account_email(root)?)
}

fn codex_account_email(root: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(root.join("auth.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let id_token = value.get("tokens")?.get("id_token")?.as_str()?;
    let claims = decode_jwt_claims(id_token)?;
    let email = claims.get("email")?.as_str()?;
    email.contains('@').then(|| email.to_owned())
}

fn cached_codex_record(path: &Path) -> Option<CodexRateLimits> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedCodexRateLimits>>> = OnceLock::new();
    let fingerprint = usage_file_fingerprint(path).ok()?;
    let cache_key = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock() {
        if let Some(cached) = cache
            .get(&cache_key)
            .filter(|cached| cached.fingerprint == fingerprint)
        {
            return cached.rate_limits.clone();
        }
    }
    let rate_limits = read_file_tail(path)
        .and_then(|contents| contents.lines().filter_map(parse_codex_record).next_back());
    if let Ok(mut cache) = cache.lock() {
        if cache.len() >= MAX_CODEX_QUOTA_CACHE_ENTRIES && !cache.contains_key(&cache_key) {
            cache.clear();
        }
        cache.insert(
            cache_key,
            CachedCodexRateLimits {
                fingerprint,
                rate_limits: rate_limits.clone(),
            },
        );
    }
    rate_limits
}

fn load_codex_usage_from(root: &Path, now_unix: Option<i64>) -> AccountUsage {
    let Ok(files) = recent_jsonl_files(&root.join("sessions"), MAX_USAGE_FILES) else {
        return AccountUsage {
            unavailable: Some(UsageUnavailable::NoSessionData),
            account: codex_account_code(root),
            email: codex_account_email(root),
            ..AccountUsage::default()
        };
    };

    let mut usage = AccountUsage {
        unavailable: Some(UsageUnavailable::NoSessionData),
        account: codex_account_code(root),
        email: codex_account_email(root),
        ..AccountUsage::default()
    };
    for path in files {
        let Some(record) = cached_codex_record(&path) else {
            continue;
        };
        let window = |minutes: u64| {
            usage_window(&record, minutes).map(|window| QuotaWindow {
                used_percent: window.used_percent.round().clamp(0.0, 100.0) as u8,
                resets_at: Some(window.resets_at),
            })
        };
        usage.five_hour = window(FIVE_HOUR_MINUTES);
        usage.seven_day = window(SEVEN_DAY_MINUTES);
        usage.credits = record.credits;
        usage.stale = [usage.five_hour, usage.seven_day]
            .into_iter()
            .flatten()
            .all(|window| {
                matches!((window.resets_at, now_unix), (Some(resets_at), Some(now)) if resets_at <= now)
            });
        if !usage.is_empty() {
            usage.unavailable = None;
            usage.last_refresh_unix = file_modified_unix(&path);
            break;
        }
    }
    usage
}

fn load_all_codex_usage(now_unix: Option<i64>) -> (Vec<ProviderAccountUsage>, String) {
    let profiles = home_path(".codex-profiles")
        .map(|root| named_profile_dirs(&root, &["config.toml", "auth.json"]))
        .unwrap_or_default();
    let primary = active_profile_id("CODEX_HOME", &profiles);
    let default_root = home_path(".codex").unwrap_or_else(|| PathBuf::from(".codex"));
    let default_usage = load_codex_usage_from(&default_root, now_unix);
    let default_label = default_usage
        .account
        .clone()
        .unwrap_or_else(|| "Default".into());
    let mut accounts = vec![ProviderAccountUsage {
        provider: QuotaProvider::Codex,
        profile_id: "default".into(),
        label: default_label,
        usage: default_usage,
    }];
    for (profile_id, root) in profiles {
        let usage = load_codex_usage_from(&root, now_unix);
        let label = usage.account.clone().unwrap_or_else(|| profile_id.clone());
        accounts.push(ProviderAccountUsage {
            provider: QuotaProvider::Codex,
            profile_id,
            label,
            usage,
        });
    }
    (accounts, primary)
}

/*
 * Account discovery above deliberately keeps the profile directory name as
 * identity. Symlinked lanes may share credentials while remaining distinct
 * launch choices, so labels and canonical paths are not deduplication keys.
 */

/// Decodes the payload segment of a JWT. Signature verification is pointless
/// here: the file is already trusted local state, and the only field read is a
/// display label.
fn decode_jwt_claims(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    let decoded = base64_url_decode(payload)?;
    serde_json::from_slice(&decoded).ok()
}

fn base64_url_decode(input: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut buffer = 0u32;
    let mut bits = 0u32;
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    for byte in input.bytes() {
        if byte == b'=' {
            break;
        }
        let value = TABLE.iter().position(|candidate| *candidate == byte)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

#[derive(Debug, serde::Deserialize)]
struct RawKimiWindow {
    used_percentage: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct RawKimiUsage {
    five_hour: Option<RawKimiWindow>,
    seven_day: Option<RawKimiWindow>,
}

pub(crate) fn parse_kimi_usage(output: &str, now_unix: Option<i64>) -> AccountUsage {
    let Ok(raw) = serde_json::from_str::<RawKimiUsage>(output) else {
        return unavailable(UsageUnavailable::HelperUnreadable);
    };
    let window = |raw: Option<RawKimiWindow>| {
        let raw = raw?;
        let used_percent = raw
            .used_percentage
            .filter(|percent| percent.is_finite() && (0.0..=100.0).contains(percent))?
            .round() as u8;
        let resets_at = raw
            .resets_at
            .as_deref()
            .filter(|value| !value.is_empty())
            .and_then(parse_utc_timestamp);
        if let (Some(resets_at), Some(now)) = (resets_at, now_unix) {
            if resets_at <= now {
                return None;
            }
        }
        Some(QuotaWindow {
            used_percent,
            resets_at,
        })
    };
    let usage = AccountUsage {
        account: None,
        five_hour: window(raw.five_hour),
        seven_day: window(raw.seven_day),
        credits: None,
        stale: false,
        last_refresh_unix: now_unix,
        ..AccountUsage::default()
    };
    if usage.is_empty() {
        unavailable(UsageUnavailable::HelperUnreadable)
    } else {
        usage
    }
}

/// Kimi has no on-disk cache of its own, so this shells out to the same
/// `kimi-usage` the statusline uses. Absent binary, non-zero exit, or garbage
/// output all mean the same thing to the bar: no Kimi segment at all.
fn load_kimi_usage(now_unix: Option<i64>) -> AccountUsage {
    // Codex refreshes itself; Kimi/agy helpers expose no auth failure signal.
    match run_helper(
        resolve_kimi_usage(),
        "--rate-limits",
        KIMI_TIMEOUT,
        KIMI_OUTPUT_LIMIT,
    ) {
        Ok(output) => parse_kimi_usage(&output, now_unix),
        Err(reason) => unavailable(reason),
    }
}

/// Drain stdout concurrently so a large helper response cannot fill its pipe
/// and masquerade as a timeout. Store at most limit + 1 bytes.
fn run_helper(
    binary: Option<PathBuf>,
    arg: &str,
    timeout: Duration,
    limit: usize,
) -> Result<String, UsageUnavailable> {
    let binary = binary.ok_or(UsageUnavailable::HelperMissing)?;
    let mut child = Command::new(binary)
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| UsageUnavailable::HelperFailed)?;
    let stdout = child.stdout.take().ok_or(UsageUnavailable::HelperFailed)?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = (|| {
            let mut bytes = Vec::new();
            let mut stdout = stdout;
            let mut buffer = [0; 4096];
            loop {
                let count = stdout.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                let remaining = (limit + 1).saturating_sub(bytes.len());
                bytes.extend_from_slice(&buffer[..count.min(remaining)]);
            }
            Ok::<_, io::Error>(bytes)
        })();
        let _ = sender.send(result);
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                // Do not join: descendants can retain stdout after the helper dies.
                return Err(if result.is_err() {
                    UsageUnavailable::HelperFailed
                } else {
                    UsageUnavailable::HelperTimedOut
                });
            }
        }
    };
    if !status.success() {
        return Err(UsageUnavailable::HelperFailed);
    }
    let bytes = receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|error| match error {
            std::sync::mpsc::RecvTimeoutError::Timeout => UsageUnavailable::HelperTimedOut,
            std::sync::mpsc::RecvTimeoutError::Disconnected => UsageUnavailable::HelperUnreadable,
        })?
        .map_err(|_| UsageUnavailable::HelperUnreadable)?;
    if bytes.len() > limit {
        return Err(UsageUnavailable::HelperUnreadable);
    }
    String::from_utf8(bytes).map_err(|_| UsageUnavailable::HelperUnreadable)
}

fn resolve_kimi_usage() -> Option<PathBuf> {
    for candidate in [".local/bin/kimi-usage", "bin/kimi-usage"] {
        if let Some(path) = home_path(candidate) {
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

#[derive(Debug, serde::Deserialize)]
struct RawAgyWindow {
    used_ratio: Option<f64>,
    reset_time: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct RawAgyUsages {
    #[serde(rename = "limit_5h")]
    five_hour: Option<RawAgyWindow>,
    #[serde(rename = "limit_7d")]
    seven_day: Option<RawAgyWindow>,
}

#[derive(Debug, serde::Deserialize)]
struct RawAgyUsage {
    usages: Option<RawAgyUsages>,
}

pub(crate) fn parse_agy_usage(output: &str, now_unix: Option<i64>) -> AccountUsage {
    let Ok(raw) = serde_json::from_str::<RawAgyUsage>(output) else {
        return unavailable(UsageUnavailable::HelperUnreadable);
    };
    let Some(usages) = raw.usages else {
        return unavailable(UsageUnavailable::HelperUnreadable);
    };
    let window = |raw: Option<RawAgyWindow>| {
        let raw = raw?;
        let ratio = raw
            .used_ratio
            .filter(|ratio| ratio.is_finite() && (0.0..=1.0).contains(ratio))?;
        let resets_at = raw
            .reset_time
            .as_deref()
            .filter(|value| !value.is_empty())
            .and_then(parse_utc_timestamp);
        if let (Some(resets_at), Some(now)) = (resets_at, now_unix) {
            if resets_at <= now {
                return None;
            }
        }
        Some(QuotaWindow {
            used_percent: (ratio * 100.0).round() as u8,
            resets_at,
        })
    };
    let usage = AccountUsage {
        five_hour: window(usages.five_hour),
        seven_day: window(usages.seven_day),
        stale: false,
        last_refresh_unix: now_unix,
        ..AccountUsage::default()
    };
    if usage.is_empty() {
        unavailable(UsageUnavailable::HelperUnreadable)
    } else {
        usage
    }
}

/// Antigravity usage is supplied by a local helper. Missing binaries, failures,
/// oversized output, and invalid JSON all leave the provider empty.
fn load_agy_usage(now_unix: Option<i64>) -> AccountUsage {
    load_agy_usage_from(resolve_agy_usage(), now_unix)
}

fn load_agy_usage_from(binary: Option<PathBuf>, now_unix: Option<i64>) -> AccountUsage {
    match run_helper(binary, "--json", AGY_TIMEOUT, AGY_OUTPUT_LIMIT) {
        Ok(output) => parse_agy_usage(&output, now_unix),
        Err(reason) => unavailable(reason),
    }
}

fn resolve_agy_usage() -> Option<PathBuf> {
    let home = home_path("")?;
    resolve_agy_usage_from(&home)
}

fn resolve_agy_usage_from(home: &Path) -> Option<PathBuf> {
    [".local/bin/agy-usage", "bin/agy-usage"]
        .into_iter()
        .map(|candidate| home.join(candidate))
        .find(|path| path.is_file())
}

/// Human reset distance: `2h45`, `3d15h`, `12m`.
pub(crate) fn reset_label(resets_at: i64, now_unix: i64) -> Option<String> {
    let remaining = resets_at.checked_sub(now_unix)?;
    if remaining <= 0 {
        return None;
    }
    let minutes = remaining / 60;
    let (hours, minutes) = (minutes / 60, minutes % 60);
    Some(match hours {
        0 => format!("{minutes}m"),
        hours if hours < 48 => format!("{hours}h{minutes:02}"),
        hours => format!("{}d{}h", hours / 24, hours % 24),
    })
}

/// Number of whole five-hour windows remaining before a weekly quota resets.
pub(crate) fn five_hour_cycles_until_reset(resets_at: Option<i64>, now_unix: i64) -> Option<i64> {
    resets_at.map(|resets_at| {
        resets_at
            .saturating_sub(now_unix)
            .max(0)
            .saturating_div(5 * 60 * 60)
    })
}

/// Refresh cadence. Quota windows move in minutes, not seconds, and the Kimi
/// read costs a process spawn, so a minute is the right price.
pub(crate) const PROVIDER_USAGE_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

pub(crate) fn snapshot_is_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|last| {
        now.checked_duration_since(last)
            .is_some_and(|elapsed| elapsed >= PROVIDER_USAGE_REFRESH_INTERVAL)
    })
}

/// Convenience for callers that only have wall-clock time.
pub(crate) fn now_unix() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
}

#[cfg(test)]
mod tests {
    #[test]
    fn usage_claude_auth_signals_are_bounded_and_do_not_expose_tokens() {
        let root = std::env::temp_dir().join(format!(
            "herdr-auth-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let cache = root.join("rate-limits-night.env");
        fs::write(&cache, "R5=81\nR5_RST=20000\nAUTH=refused\n").unwrap();
        let load = || {
            load_claude_usage_from(
                &cache,
                Some("night"),
                Some(&root),
                Some(10000),
                Instant::now(),
            )
        };
        assert_eq!(load().unavailable, Some(UsageUnavailable::AuthExpired));
        fs::write(root.join(".auth-state"), "AUTH=ok\n").unwrap();
        assert_eq!(load().unavailable, None);
        fs::write(root.join(".auth-state"), "AUTH=refused\n").unwrap();
        assert_eq!(load().unavailable, Some(UsageUnavailable::AuthExpired));
        for auth in ["ok", "missing", "error", "unknown"] {
            fs::write(root.join(".auth-state"), format!("AUTH={auth}\n")).unwrap();
            assert_eq!(load().unavailable, None);
        }
        fs::write(
            root.join(".credentials.json"),
            r#"{"claudeAiOauth":{"expiresAt":9999000,"accessToken":"synthetic-token"}}"#,
        )
        .unwrap();
        let usage = load();
        assert_eq!(usage.unavailable, Some(UsageUnavailable::AuthExpired));
        assert!(!format!("{usage:?}").contains("synthetic-token"));
        assert_eq!(usage.five_hour.unwrap().used_percent, 81);
        fs::write(
            root.join(".credentials.json"),
            r#"{"claudeAiOauth":{"expiresAt":9999000,"refreshToken":"synthetic-refresh"}}"#,
        )
        .unwrap();
        assert_eq!(load().unavailable, None);
        fs::write(
            root.join(".credentials.json"),
            r#"{"claudeAiOauth":{"expiresAt":10001000}}"#,
        )
        .unwrap();
        assert_eq!(load().unavailable, None);
        fs::write(
            root.join(".credentials.json"),
            " ".repeat(MAX_PROFILE_METADATA_BYTES as usize + 1),
        )
        .unwrap();
        assert_eq!(load().unavailable, None);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn usage_helper_parsing_records_refresh_time() {
        let kimi = parse_kimi_usage(r#"{"five_hour":{"used_percentage":12}}"#, Some(123));
        let agy = parse_agy_usage(r#"{"usages":{"limit_5h":{"used_ratio":0.12}}}"#, Some(123));
        for usage in [kimi, agy] {
            assert_eq!(usage.last_refresh_unix, Some(123));
            assert_eq!(usage.unavailable, None);
        }
    }
    #[test]
    fn usage_missing_sources_and_unreadable_helpers_have_reasons() {
        assert_eq!(
            parse_claude_rate_limits("", Some(10), None).unavailable,
            Some(UsageUnavailable::NoStatuslineData)
        );
        assert_eq!(
            load_codex_usage_from(Path::new("/nonexistent/herdr-usage"), Some(10)).unavailable,
            Some(UsageUnavailable::NoSessionData)
        );
        for parse in [parse_kimi_usage, parse_agy_usage] {
            for output in ["garbage", "{}"] {
                assert_eq!(
                    parse(output, Some(10)).unavailable,
                    Some(UsageUnavailable::HelperUnreadable)
                );
            }
        }
    }
    #[cfg(unix)]
    #[test]
    fn usage_helper_missing_failed_timeout_and_output_limit() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "herdr-helper-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        assert_eq!(
            run_helper(None, "", Duration::from_millis(20), 1024),
            Err(UsageUnavailable::HelperMissing)
        );
        let script = root.join("helper");
        for (body, expected) in [
            ("exit 1", UsageUnavailable::HelperFailed),
            ("exec sleep 1", UsageUnavailable::HelperTimedOut),
            ("printf '123456789'", UsageUnavailable::HelperUnreadable),
        ] {
            fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(
                run_helper(Some(script.clone()), "", Duration::from_millis(20), 8),
                Err(expected)
            );
        }
        fs::write(&script, "#!/bin/sh\nprintf garbage\n").unwrap();
        let output = run_helper(Some(script), "", Duration::from_secs(1), 1024).unwrap();
        assert_eq!(
            parse_kimi_usage(&output, None).unavailable,
            Some(UsageUnavailable::HelperUnreadable)
        );
        fs::remove_dir_all(root).unwrap();
    }

    use super::*;

    // Fixed wall clock shared by quota parser fixtures.
    const NOW: i64 = 1_787_992_841;

    #[test]
    fn codex_rate_limit_record_keeps_both_supported_windows() {
        let usage = parse_codex_record(
            r#"{"payload":{"rate_limits":{"primary":{"used_percent":31.0,"window_minutes":300,"resets_at":1788003000},"secondary":{"used_percent":52.0,"window_minutes":10080,"resets_at":1788307200},"credits":{"balance":"1927.95"}}}}"#,
        )
        .expect("rate limits");

        assert_eq!(
            usage_window(&usage, FIVE_HOUR_MINUTES)
                .unwrap()
                .used_percent,
            31.0
        );
        assert_eq!(
            usage_window(&usage, SEVEN_DAY_MINUTES)
                .unwrap()
                .used_percent,
            52.0
        );
        assert_eq!(usage.credits, Some(1927.95));
    }

    #[test]
    fn parses_ccusage_cost_and_remaining_minutes_from_active_block() {
        let usage = parse_ccusage_output(
            r#"{"blocks":[{"isActive":true,"endTime":"2026-09-21T14:00:00.000Z","costUSD":56.68,"projection":{"remainingMinutes":66}}]}"#,
            parse_utc_timestamp("2026-09-21T12:00:00Z").expect("valid test timestamp"),
        )
        .expect("valid ccusage JSON")
        .expect("active Claude block");

        assert_eq!(usage.cost_usd, 56.68);
        assert_eq!(usage.remaining_minutes, Some(66));
    }

    #[test]
    fn ccusage_missing_active_block_keeps_claude_details_unavailable() {
        assert_eq!(
            parse_ccusage_output(r#"{"blocks":[{"isActive":false}]}"#, NOW,)
                .expect("valid ccusage JSON"),
            None
        );
    }

    #[test]
    fn failed_ccusage_attempt_backs_off_for_thirty_minutes() {
        let start = Instant::now();
        let details = ClaudeUsageDetails {
            cost_usd: 12.5,
            remaining_minutes: Some(45),
        };
        let mut backoff = CcusageCache::default();
        let mut attempts = 0;

        assert_eq!(
            backoff.poll(start, || {
                attempts += 1;
                Err(())
            }),
            None
        );
        assert_eq!(
            backoff.poll(
                start + CCUSAGE_FAILURE_RETRY_INTERVAL - Duration::from_millis(1),
                || {
                    attempts += 1;
                    Ok(details)
                }
            ),
            None
        );
        assert_eq!(attempts, 1, "backoff must skip the helper entirely");
        assert_eq!(
            backoff.poll(start + CCUSAGE_FAILURE_RETRY_INTERVAL, || {
                attempts += 1;
                Ok(details)
            }),
            Some(details)
        );
        assert_eq!(attempts, 2);
    }

    #[test]
    fn successful_ccusage_result_is_cached_for_ten_minutes() {
        const TEN_MINUTES: Duration = Duration::from_secs(10 * 60);
        let start = Instant::now();
        let first = ClaudeUsageDetails {
            cost_usd: 12.5,
            remaining_minutes: Some(45),
        };
        let refreshed = ClaudeUsageDetails {
            cost_usd: 14.0,
            remaining_minutes: Some(30),
        };
        let mut cache = CcusageCache::default();
        let mut attempts = 0;

        assert_eq!(
            cache.poll(start, || {
                attempts += 1;
                Ok(first)
            }),
            Some(first)
        );
        assert_eq!(
            cache.poll(start + TEN_MINUTES - Duration::from_millis(1), || {
                attempts += 1;
                Ok(refreshed)
            }),
            Some(ClaudeUsageDetails {
                remaining_minutes: Some(36),
                ..first
            }),
            "a cached block keeps counting down"
        );
        assert_eq!(attempts, 1, "a fresh success must skip ccusage");
        assert_eq!(
            cache.poll(start + TEN_MINUTES, || {
                attempts += 1;
                Ok(refreshed)
            }),
            Some(refreshed)
        );
        assert_eq!(
            attempts, 2,
            "ccusage must run when the success is ten minutes old"
        );
    }

    struct UsageFixture {
        root: PathBuf,
        cache_path: PathBuf,
        claude_path: PathBuf,
        codex_path: PathBuf,
    }

    impl UsageFixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "herdr-usage-{name}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("test clock after epoch")
                    .as_nanos()
            ));
            let claude_path = root
                .join(".claude/projects/project-a/nested")
                .join("claude-session.jsonl");
            let codex_path = root
                .join(".codex/sessions/2026/09/06")
                .join("rollout-codex-session.jsonl");
            fs::create_dir_all(claude_path.parent().expect("Claude fixture parent"))
                .expect("create Claude fixture tree");
            fs::create_dir_all(codex_path.parent().expect("Codex fixture parent"))
                .expect("create Codex fixture tree");
            fs::write(
                &claude_path,
                concat!(
                    "{\"type\":\"user\",\"timestamp\":\"2026-09-05T10:00:00Z\"}\n",
                    "{\"type\":\"assistant\",\"timestamp\":\"2026-09-05T10:01:00.123Z\",",
                    "\"message\":{\"model\":\"claude-sonnet-5\",\"usage\":{",
                    "\"input_tokens\":10,\"output_tokens\":5,",
                    "\"cache_creation_input_tokens\":20,\"cache_read_input_tokens\":30}}}\n"
                ),
            )
            .expect("write Claude fixture");
            fs::write(
                &codex_path,
                concat!(
                    "{\"timestamp\":\"2026-09-05T11:00:00Z\",\"type\":\"turn_context\",",
                    "\"payload\":{\"turn_id\":\"turn-1\",\"model\":\"gpt-5.6-sol\"}}\n",
                    "{\"timestamp\":\"2026-09-05T11:01:00Z\",\"type\":\"token_usage_record\",",
                    "\"payload\":{\"turn_id\":\"turn-1\",\"session_id\":\"ignored-payload-session\",",
                    "\"usage\":{\"input_tokens\":100,\"cached_input_tokens\":60,",
                    "\"cache_write_input_tokens\":10,\"output_tokens\":7,",
                    "\"reasoning_output_tokens\":99,\"total_tokens\":107}}}\n",
                    "{\"timestamp\":\"2026-09-05T11:01:00Z\",\"type\":\"event_msg\",",
                    "\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{",
                    "\"input_tokens\":100,\"cached_input_tokens\":60,",
                    "\"cache_write_input_tokens\":10,\"output_tokens\":7}}}}\n"
                ),
            )
            .expect("write Codex fixture");
            let cache_path = root.join("state/herdr-dev").join(USAGE_CACHE_FILE);
            Self {
                root,
                cache_path,
                claude_path,
                codex_path,
            }
        }

        fn scan(&self) -> UsageSnapshot {
            collect_dashboard_usage_from(&self.root, &self.cache_path)
        }
    }

    impl Drop for UsageFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn claude_cache_yields_both_windows_and_drops_a_rolled_over_one() {
        let contents = "R5=6\nR7=56\nR5_RST=1788003000\nR7_RST=1788307200\nTS=1787992841\n";
        let usage = parse_claude_rate_limits(contents, Some(NOW), Some(Duration::from_secs(60)));
        assert_eq!(
            usage.five_hour,
            Some(QuotaWindow {
                used_percent: 6,
                resets_at: Some(1_788_003_000)
            })
        );
        assert_eq!(usage.seven_day.map(|window| window.used_percent), Some(56));
        assert!(!usage.stale);

        // Nobody has rendered a statusline since the window rolled over, so the
        // percentage describes a window that no longer exists.
        let expired = parse_claude_rate_limits(contents, Some(1_788_400_000), None);
        assert!(expired.five_hour.is_none());
        assert!(expired.seven_day.is_none());
    }

    #[test]
    fn claude_cache_accepts_rfc3339_resets_and_epoch_resets() {
        let rfc3339 = parse_claude_rate_limits(
            "R5=6\nR7=56\nR5_RST=2026-08-29T11:30:00Z\nR7_RST=2026-09-02T00:00:00+00:00\n",
            Some(NOW),
            None,
        );
        let epoch = parse_claude_rate_limits(
            "R5=6\nR7=56\nR5_RST=1788003000\nR7_RST=1788307200\n",
            Some(NOW),
            None,
        );

        assert_eq!(rfc3339.five_hour, epoch.five_hour);
        assert_eq!(rfc3339.seven_day, epoch.seven_day);
    }

    #[test]
    fn profile_discovery_requires_provider_markers_and_keeps_names() {
        let root = std::env::temp_dir().join(format!(
            "herdr-provider-profiles-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(root.join("scalable/sessions")).expect("profile tree");
        fs::write(root.join("scalable/config.toml"), "").expect("profile marker");
        fs::create_dir_all(root.join("sessions/2026/09")).expect("internal sessions tree");
        fs::create_dir_all(root.join(".bak-old")).expect("backup profile");
        fs::write(root.join(".bak-old/config.toml"), "").expect("backup marker");

        let profiles = named_profile_dirs(&root, &["config.toml", "auth.json"]);

        assert_eq!(profiles, vec![("scalable".into(), root.join("scalable"))]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn codex_usage_carries_the_account_email_when_sessions_exist() {
        let root = std::env::temp_dir().join(format!(
            "herdr-codex-usage-email-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(root.join("sessions")).expect("sessions dir");
        // {"email":"matthias@scalablehq.com"}
        fs::write(
            root.join("auth.json"),
            r#"{"tokens":{"id_token":"header.eyJlbWFpbCI6Im1hdHRoaWFzQHNjYWxhYmxlaHEuY29tIn0.signature"}}"#,
        )
        .expect("auth file");

        let usage = load_codex_usage_from(&root, Some(NOW));

        assert_eq!(usage.email.as_deref(), Some("matthias@scalablehq.com"));
        assert_eq!(usage.account.as_deref(), Some("SHQ"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn claude_profile_label_comes_from_the_meta_email_domain() {
        let root = std::env::temp_dir().join(format!(
            "herdr-claude-profile-label-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("profile root");
        fs::write(
            root.join("meta.env"),
            "PROFILE=scalablehq\nEMAIL=matthias@scalablehq.com\n",
        )
        .expect("profile metadata");

        assert_eq!(claude_profile_account_code(&root).as_deref(), Some("SHQ"));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn profile_discovery_preserves_each_symlink_name() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "herdr-provider-profile-links-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        let account = root.join("account");
        fs::create_dir_all(&account).expect("account root");
        fs::write(account.join("meta.env"), "").expect("profile marker");
        symlink(&account, root.join("lane-a")).expect("first lane");
        symlink(&account, root.join("lane-b")).expect("second lane");

        let profiles = named_profile_dirs(&root, &["meta.env"]);

        assert_eq!(
            profiles
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            ["account", "lane-a", "lane-b"]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_cache_older_than_its_budget_is_reported_stale_not_dropped() {
        let contents = "R5=6\nR7=56\nR5_RST=1788003000\nR7_RST=1788307200\n";
        let usage = parse_claude_rate_limits(contents, Some(NOW), Some(CLAUDE_CACHE_STALE_AFTER));
        assert!(usage.stale);
        assert!(usage.five_hour.is_some(), "stale still renders, dimmed");
    }

    #[test]
    fn unset_percentages_read_as_absent_rather_than_zero() {
        // The statusline writes -1 when Claude Code sent no rate limits at all.
        // Zero would draw a column claiming a fresh quota.
        let usage = parse_claude_rate_limits("R5=-1\nR7=-1\n", Some(NOW), None);
        assert!(usage.is_empty());
    }

    #[test]
    fn kimi_reshaped_output_parses_into_the_same_shape() {
        let output = r#"{"five_hour":{"used_percentage":0,"resets_at":""},
            "seven_day":{"used_percentage":24,"resets_at":"2026-08-31T06:02:04Z"}}"#;
        let usage = parse_kimi_usage(output, Some(NOW));
        assert_eq!(usage.five_hour.map(|window| window.used_percent), Some(0));
        assert_eq!(
            usage.five_hour.and_then(|window| window.resets_at),
            None,
            "an empty reset string is not a reset time"
        );
        assert_eq!(usage.seven_day.map(|window| window.used_percent), Some(24));
    }

    #[test]
    fn garbage_from_kimi_is_no_kimi_segment_rather_than_a_zero_one() {
        assert!(parse_kimi_usage("not json", Some(NOW)).is_empty());
        assert!(parse_kimi_usage(r#"{"five_hour":{"used_percentage":250}}"#, Some(NOW)).is_empty());
    }

    #[test]
    fn agy_fixture_parses_the_pro_account_five_hour_and_free_account_weekly_limits() {
        let usage = parse_agy_usage(
            include_str!("../tests/fixtures/agy-usage.sample.json"),
            None,
        );

        assert_eq!(usage.five_hour.map(|window| window.used_percent), Some(1));
        assert_eq!(
            usage.five_hour.and_then(|window| window.resets_at),
            parse_utc_timestamp("2026-09-27T17:17:35Z")
        );
        assert_eq!(usage.seven_day.map(|window| window.used_percent), Some(1));
        assert_eq!(
            usage.seven_day.and_then(|window| window.resets_at),
            parse_utc_timestamp("2026-10-04T12:17:22Z")
        );
    }

    #[test]
    fn missing_agy_helper_produces_no_usage() {
        assert!(load_agy_usage_from(None, Some(NOW)).is_empty());

        let home = std::env::temp_dir().join(format!(
            "herdr-missing-agy-helper-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("test clock after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&home).expect("create empty test home");
        assert_eq!(resolve_agy_usage_from(&home), None);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn account_codes_map_known_domains_and_still_name_unknown_ones() {
        assert_eq!(account_code("scalablehq.com").as_deref(), Some("SHQ"));
        assert_eq!(account_code("DOMAIN=scalable.so").as_deref(), Some("SSO"));
        assert_eq!(account_code("machete-ventures.com").as_deref(), Some("MV"));
        assert_eq!(account_code("newcompany.io").as_deref(), Some("NEW"));
        assert_eq!(account_code(""), None);
    }

    #[test]
    fn a_jwt_payload_decodes_to_the_account_email_domain() {
        // {"email":"matthias@scalablehq.com"}
        let token = "header.eyJlbWFpbCI6Im1hdHRoaWFzQHNjYWxhYmxlaHEuY29tIn0.signature";
        let claims = decode_jwt_claims(token).expect("payload");
        assert_eq!(
            claims.get("email").and_then(|value| value.as_str()),
            Some("matthias@scalablehq.com")
        );
    }

    #[test]
    fn reset_labels_shorten_as_the_distance_grows() {
        assert_eq!(reset_label(NOW + 720, NOW).as_deref(), Some("12m"));
        assert_eq!(reset_label(NOW + 9_900, NOW).as_deref(), Some("2h45"));
        assert_eq!(reset_label(NOW + 313_200, NOW).as_deref(), Some("3d15h"));
        assert_eq!(reset_label(NOW - 1, NOW), None);
    }

    #[test]
    fn five_hour_cycles_until_reset_round_down_and_handle_boundaries() {
        assert_eq!(five_hour_cycles_until_reset(None, NOW), None);
        assert_eq!(five_hour_cycles_until_reset(Some(NOW - 1), NOW), Some(0));
        assert_eq!(
            five_hour_cycles_until_reset(Some(NOW - 30 * 60 * 60), NOW),
            Some(0)
        );
        assert_eq!(
            five_hour_cycles_until_reset(Some(NOW + 10 * 60 * 60), NOW),
            Some(2)
        );
        assert_eq!(
            five_hour_cycles_until_reset(Some(NOW + 10 * 60 * 60 - 1), NOW),
            Some(1)
        );
    }

    #[test]
    fn peak_percent_reports_the_window_closest_to_its_ceiling() {
        let usage = AccountUsage {
            five_hour: Some(QuotaWindow {
                used_percent: 6,
                resets_at: None,
            }),
            seven_day: Some(QuotaWindow {
                used_percent: 91,
                resets_at: None,
            }),
            ..AccountUsage::default()
        };
        assert_eq!(usage.peak_percent(), Some(91));
    }

    #[test]
    fn all_provider_usage_snapshots_refresh_after_one_minute() {
        const ONE_MINUTE: Duration = Duration::from_secs(60);
        assert_eq!(PROVIDER_USAGE_REFRESH_INTERVAL, ONE_MINUTE);

        let start = Instant::now();
        assert!(snapshot_is_due(None, start));
        assert!(!snapshot_is_due(
            Some(start),
            start + ONE_MINUTE - Duration::from_millis(1)
        ));
        assert!(snapshot_is_due(Some(start), start + ONE_MINUTE));
    }

    #[test]
    fn dashboard_scan_reads_recursive_claude_and_codex_fixtures_without_double_counting() {
        let fixture = UsageFixture::new("both-providers");

        let snapshot = fixture.scan();

        assert_eq!(snapshot.files_seen, 2);
        assert_eq!(snapshot.files_read, 2);
        assert_eq!(snapshot.samples.len(), 2);
        assert_eq!(
            snapshot.samples[0],
            UsageSample {
                provider: UsageProvider::ClaudeCode,
                session_id: "claude-session".into(),
                timestamp: 1_788_602_460,
                model: "claude-sonnet-5".into(),
                input_tokens: 10,
                output_tokens: 5,
                cache_write_tokens: 20,
                cache_read_tokens: 30,
            }
        );
        assert_eq!(snapshot.samples[0].processed_tokens(), 65);
        assert_eq!(snapshot.samples[1].provider, UsageProvider::Codex);
        assert_eq!(snapshot.samples[1].session_id, "rollout-codex-session");
        assert_eq!(snapshot.samples[1].model, "gpt-5.6-sol");
        assert_eq!(snapshot.samples[1].input_tokens, 40);
        assert_eq!(snapshot.samples[1].cache_read_tokens, 60);
        assert_eq!(snapshot.samples[1].cache_write_tokens, 10);
        assert_eq!(snapshot.samples[1].output_tokens, 7);
        assert_eq!(
            snapshot
                .samples_between(1_788_602_000, 1_788_607_000)
                .count(),
            2
        );
    }

    #[test]
    fn dashboard_cache_reuses_unchanged_files_and_reads_only_the_changed_file() {
        let fixture = UsageFixture::new("changed-cache-file");
        let first = fixture.scan();
        assert_eq!(first.files_read, 2);
        assert!(fixture.cache_path.is_file());

        let cached = fixture.scan();
        assert_eq!(cached.files_seen, 2);
        assert_eq!(cached.files_read, 0);
        assert_eq!(cached.samples, first.samples);

        let mut claude = fs::read_to_string(&fixture.claude_path).expect("read Claude fixture");
        claude.push_str(concat!(
            "{\"type\":\"assistant\",\"timestamp\":\"2026-09-06T10:01:00Z\",",
            "\"message\":{\"model\":\"claude-opus-5\",\"usage\":{",
            "\"input_tokens\":1,\"output_tokens\":2,",
            "\"cache_creation_input_tokens\":3,\"cache_read_input_tokens\":4}}}\n"
        ));
        fs::write(&fixture.claude_path, claude).expect("change Claude fixture");

        let changed = fixture.scan();
        assert_eq!(changed.files_seen, 2);
        assert_eq!(changed.files_read, 1);
        assert_eq!(changed.samples.len(), 3);
        assert_eq!(
            changed
                .samples
                .iter()
                .filter(|sample| sample.provider == UsageProvider::Codex)
                .count(),
            1,
            "unchanged Codex file is restored once from cache"
        );
        let cache: UsageCache =
            serde_json::from_slice(&fs::read(&fixture.cache_path).expect("read persisted cache"))
                .expect("parse persisted cache");
        assert_eq!(cache.version, USAGE_CACHE_VERSION);
        assert_eq!(cache.files.len(), 2);
        assert!(cache.files.contains_key(&fixture.codex_path));
    }

    #[test]
    fn legacy_codex_token_count_is_used_when_incremental_records_are_absent() {
        let samples = parse_codex_usage_samples(
            concat!(
                "{\"timestamp\":\"2026-09-05T11:00:00Z\",\"type\":\"turn_context\",",
                "\"payload\":{\"turn_id\":\"turn-1\",\"model\":\"gpt-5.3-codex-spark\"}}\n",
                "{\"timestamp\":\"2026-09-05T11:01:00Z\",\"type\":\"event_msg\",",
                "\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{",
                "\"input_tokens\":80,\"cached_input_tokens\":20,",
                "\"cache_write_input_tokens\":5,\"output_tokens\":4}}}}\n"
            ),
            "legacy-session",
        );

        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].model, "gpt-5.3-codex-spark");
        assert_eq!(samples[0].input_tokens, 60);
        assert_eq!(samples[0].output_tokens, 4);
    }

    #[test]
    fn claude_subagent_path_uses_the_parent_session_id() {
        assert_eq!(
            usage_session_id(Path::new(
                "/home/.claude/projects/project/session-123/subagents/agent-456.jsonl"
            )),
            "session-123"
        );
        assert_eq!(
            usage_session_id(Path::new("/home/.codex/sessions/rollout-session-789.jsonl")),
            "rollout-session-789"
        );
    }
}
