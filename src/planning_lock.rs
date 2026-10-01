//! Server-owned planning lock state and password verification.

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use serde::{Deserialize, Serialize};
use std::io::{self, Write as _};
use std::path::Path;

pub const MIN_PASSWORD_CHARS: usize = 24;
pub const UNLOCK_MINUTES: [u8; 5] = [5, 10, 15, 20, 30];
pub const CONFIG_FILE_NAME: &str = "planning-lock.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogFlow {
    SetupChooseDiscussionTab,
    SetupPassword,
    SetupConfirmPassword,
    UnlockPassword,
    ChooseDuration,
    Manage,
    ChooseDiscussionTab,
    DisablePassword,
    DisableConfirmation,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Dialog {
    pub flow: DialogFlow,
    pub input: String,
    pub password: Option<String>,
    pub selected_tab: usize,
    pub error: Option<String>,
}

impl std::fmt::Debug for Dialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dialog")
            .field("flow", &self.flow)
            .field("input_chars", &self.input.chars().count())
            .field("has_password", &self.password.is_some())
            .field("selected_tab", &self.selected_tab)
            .field("error", &self.error)
            .finish()
    }
}

impl Dialog {
    pub fn new(flow: DialogFlow) -> Self {
        Self {
            flow,
            input: String::new(),
            password: None,
            selected_tab: 0,
            error: None,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Config {
    password_hash: String,
    discussion_tab_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unlock_until_unix_s: Option<u64>,
}

/// Safe projection sent to attached clients. Password material stays in the
/// server's private lock file and is never part of an API snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Snapshot {
    pub locked: bool,
    pub discussion_tab_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unlock_until_unix_s: Option<u64>,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct PlanningLock {
    config: Option<Config>,
    corrupt: bool,
    remote_snapshot: Option<Option<Snapshot>>,
}

impl std::fmt::Debug for PlanningLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanningLock")
            .field("configured", &self.config.is_some())
            .field("corrupt", &self.corrupt)
            .field("remote_managed", &self.remote_snapshot.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    PasswordTooShort,
    EmptyDiscussionTab,
    InvalidPassword,
    InvalidConfirmation,
    InvalidDuration,
    Locked,
    HashingUnavailable,
    CorruptConfig,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::PasswordTooShort => "password must be at least 24 characters",
            Self::EmptyDiscussionTab => "choose a discussion tab",
            Self::InvalidPassword => "incorrect password",
            Self::InvalidConfirmation => "type \"turn off\" to disable planning lock",
            Self::InvalidDuration => "choose 5, 10, 15, 20 or 30 minutes",
            Self::Locked => "the planning lock is active",
            Self::HashingUnavailable => "could not securely store the planning lock password",
            Self::CorruptConfig => "planning lock config is invalid; delete planning-lock.json on the server to reset it",
        };
        f.write_str(message)
    }
}

impl std::error::Error for Error {}

impl PlanningLock {
    pub fn load(path: &Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => Self::from_json(&bytes).unwrap_or_else(|error| {
                tracing::error!(path = %path.display(), %error, "planning lock config is invalid; keeping the lock closed");
                Self {
                    config: None,
                    corrupt: true,
                    remote_snapshot: None,
                }
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                tracing::error!(path = %path.display(), %error, "planning lock config cannot be read; keeping the lock closed");
                Self {
                    config: None,
                    corrupt: true,
                    remote_snapshot: None,
                }
            }
        }
    }

    /// Persists the authoritative lock file atomically with owner-only access.
    /// An empty state removes the file, which is also the documented recovery
    /// path for a forgotten password.
    pub fn persist(&self, path: &Path) -> io::Result<()> {
        if self.corrupt {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                Error::CorruptConfig,
            ));
        }
        let Some(bytes) = self
            .to_json()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        else {
            return match std::fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            };
        };

        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;

        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("planning-lock.json");
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp_path = parent.join(format!(".{name}.{}-{nonce}.tmp", std::process::id()));
        let write = (|| -> io::Result<()> {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp_path)?;
            file.write_all(&bytes)?;
            file.sync_all()
        })();
        if let Err(error) = write {
            let _ = std::fs::remove_file(&temp_path);
            return Err(error);
        }
        if let Err(error) = crate::platform::replace_file_durably(&temp_path, path) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(error);
        }
        Ok(())
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes).map(|config| Self {
            config: Some(config),
            corrupt: false,
            remote_snapshot: None,
        })
    }

    pub fn to_json(&self) -> Result<Option<Vec<u8>>, serde_json::Error> {
        self.config
            .as_ref()
            .map(serde_json::to_vec_pretty)
            .transpose()
    }

    pub fn snapshot(&self, now_unix_s: u64) -> Option<Snapshot> {
        if let Some(snapshot) = &self.remote_snapshot {
            return snapshot.clone().map(|mut snapshot| {
                snapshot.locked |= snapshot
                    .unlock_until_unix_s
                    .is_none_or(|deadline| now_unix_s >= deadline);
                snapshot
            });
        }
        if self.corrupt {
            return Some(Snapshot {
                locked: true,
                discussion_tab_id: String::new(),
                unlock_until_unix_s: None,
            });
        }
        let config = self.config.as_ref()?;
        let unlock_until_unix_s = config
            .unlock_until_unix_s
            .filter(|deadline| now_unix_s < *deadline);
        Some(Snapshot {
            locked: unlock_until_unix_s.is_none(),
            discussion_tab_id: config.discussion_tab_id.clone(),
            unlock_until_unix_s,
        })
    }

    pub fn apply_remote_snapshot(&mut self, snapshot: Option<Snapshot>) {
        self.remote_snapshot = Some(snapshot);
        self.corrupt = false;
    }

    pub fn fail_closed_remote(&mut self) {
        self.apply_remote_snapshot(Some(Snapshot {
            locked: true,
            discussion_tab_id: String::new(),
            unlock_until_unix_s: None,
        }));
    }

    pub fn is_locked(&self, now_unix_s: u64) -> bool {
        self.corrupt
            || self
                .snapshot(now_unix_s)
                .is_some_and(|snapshot| snapshot.locked)
    }

    pub fn discussion_tab_id(&self) -> Option<&str> {
        self.config
            .as_ref()
            .map(|config| config.discussion_tab_id.as_str())
    }

    pub fn configured(&self) -> bool {
        self.config.is_some() || self.corrupt
    }

    pub fn permits_tab(&self, tab_id: &str, now_unix_s: u64) -> bool {
        !self.corrupt
            && (!self.is_locked(now_unix_s)
                || self
                    .config
                    .as_ref()
                    .is_some_and(|config| config.discussion_tab_id == tab_id))
    }

    pub fn configure(&mut self, password: &str, discussion_tab_id: &str) -> Result<(), Error> {
        if self.corrupt {
            return Err(Error::CorruptConfig);
        }
        if password.chars().count() < MIN_PASSWORD_CHARS {
            return Err(Error::PasswordTooShort);
        }
        if discussion_tab_id.trim().is_empty() {
            return Err(Error::EmptyDiscussionTab);
        }
        self.config = Some(Config {
            password_hash: hash_password(password)?,
            discussion_tab_id: discussion_tab_id.to_owned(),
            unlock_until_unix_s: None,
        });
        self.corrupt = false;
        Ok(())
    }

    pub fn verify_password(&self, password: &str) -> Result<(), Error> {
        let config = self.config.as_ref().ok_or(Error::InvalidPassword)?;
        let parsed =
            PasswordHash::new(&config.password_hash).map_err(|_| Error::InvalidPassword)?;
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .map_err(|_| Error::InvalidPassword)
    }

    pub fn unlock(
        &mut self,
        password: &str,
        duration_minutes: u8,
        now_unix_s: u64,
    ) -> Result<(), Error> {
        if !UNLOCK_MINUTES.contains(&duration_minutes) {
            return Err(Error::InvalidDuration);
        }
        self.verify_password(password)?;
        let config = self.config.as_mut().ok_or(Error::InvalidPassword)?;
        config.unlock_until_unix_s =
            Some(now_unix_s.saturating_add(u64::from(duration_minutes) * 60));
        Ok(())
    }

    pub fn set_discussion_session(&mut self, tab_id: &str, now_unix_s: u64) -> Result<(), Error> {
        if self.is_locked(now_unix_s) {
            return Err(Error::Locked);
        }
        if tab_id.trim().is_empty() {
            return Err(Error::EmptyDiscussionTab);
        }
        if let Some(config) = self.config.as_mut() {
            config.discussion_tab_id = tab_id.to_owned();
        }
        Ok(())
    }

    pub fn disable(&mut self, password: &str, typed_confirmation: &str) -> Result<(), Error> {
        self.verify_password(password)?;
        if typed_confirmation != "turn off" {
            return Err(Error::InvalidConfirmation);
        }
        self.config = None;
        Ok(())
    }
}

fn hash_password(password: &str) -> Result<String, Error> {
    let mut salt_bytes = [0u8; 16];
    // getrandom is already a direct dependency used by Herdr for runtime IDs.
    getrandom::fill(&mut salt_bytes).map_err(|_| Error::HashingUnavailable)?;
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|_| Error::HashingUnavailable)?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| Error::HashingUnavailable)
}

#[cfg(test)]
mod tests {
    use super::{Error, PlanningLock, UNLOCK_MINUTES};

    const PASSWORD: &str = "a planning password with 32 chars";

    fn configured() -> PlanningLock {
        let mut lock = PlanningLock::default();
        lock.configure(PASSWORD, "session-1").expect("configure");
        lock
    }

    #[test]
    fn password_is_hashed_and_wrong_password_is_rejected() {
        let lock = configured();
        let encoded = lock.to_json().expect("serialize").expect("configured");
        assert!(!String::from_utf8_lossy(&encoded).contains(PASSWORD));
        assert_eq!(
            lock.verify_password("wrong password"),
            Err(Error::InvalidPassword)
        );
        assert_eq!(lock.verify_password(PASSWORD), Ok(()));
    }

    #[test]
    fn passwords_shorter_than_24_characters_are_rejected() {
        let mut lock = PlanningLock::default();
        assert_eq!(
            lock.configure("too short", "session-1"),
            Err(Error::PasswordTooShort)
        );
    }

    #[test]
    fn each_allowed_unlock_duration_sets_the_expected_deadline() {
        for minutes in UNLOCK_MINUTES {
            let mut lock = configured();
            lock.unlock(PASSWORD, minutes, 1_000).expect("unlock");
            assert_eq!(
                lock.snapshot(1_000)
                    .and_then(|snapshot| snapshot.unlock_until_unix_s),
                Some(1_000 + u64::from(minutes) * 60)
            );
        }
    }

    #[test]
    fn unsupported_duration_and_wrong_password_do_not_unlock() {
        let mut lock = configured();
        assert_eq!(lock.unlock(PASSWORD, 7, 1_000), Err(Error::InvalidDuration));
        assert_eq!(
            lock.unlock("wrong password", 5, 1_000),
            Err(Error::InvalidPassword)
        );
        assert!(lock.is_locked(1_000));
    }

    #[test]
    fn unlock_expires_at_its_deadline_and_relocks() {
        let mut lock = configured();
        lock.unlock(PASSWORD, 5, 1_000).expect("unlock");
        assert!(!lock.is_locked(1_299));
        assert!(lock.is_locked(1_300));
    }

    #[test]
    fn locked_state_allows_only_the_discussion_session() {
        let lock = configured();
        assert!(lock.permits_tab("session-1", 1_000));
        assert!(!lock.permits_tab("session-2", 1_000));
    }

    #[test]
    fn changing_discussion_session_requires_unlock_and_password() {
        let mut lock = configured();
        assert_eq!(
            lock.set_discussion_session("session-2", 1_000),
            Err(Error::Locked)
        );
        lock.unlock(PASSWORD, 5, 1_000).expect("unlock");
        lock.set_discussion_session("tab-2", 1_001)
            .expect("change discussion session");
        assert!(lock.permits_tab("tab-2", 1_001));
    }

    #[test]
    fn turning_off_requires_the_password_and_typed_phrase() {
        let mut lock = configured();
        assert!(lock.disable(PASSWORD, "turn off now").is_err());
        assert!(lock.snapshot(1_000).is_some());
        lock.disable(PASSWORD, "turn off").expect("disable");
        assert_eq!(lock.snapshot(1_000), None);
    }

    #[test]
    fn serialized_config_round_trips_without_exposing_the_password() {
        let original = configured();
        let bytes = original.to_json().expect("serialize").expect("configured");
        let restored = PlanningLock::from_json(&bytes).expect("parse");
        assert_eq!(restored, original);
        assert!(!String::from_utf8_lossy(&bytes).contains(PASSWORD));
    }

    #[test]
    fn missing_file_is_unlocked_and_empty_state_deletes_the_config_file() {
        let path = std::env::temp_dir().join(format!(
            "herdr-planning-lock-{}.json",
            crate::config::test_unique_suffix()
        ));
        assert!(PlanningLock::load(&path).snapshot(1_000).is_none());

        let lock = configured();
        lock.persist(&path).expect("persist lock");
        assert!(PlanningLock::load(&path).is_locked(1_000));

        PlanningLock::default().persist(&path).expect("remove lock");
        assert!(!path.exists());
    }
}
