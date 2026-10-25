//! Fallback completion recap from omp's own `history.db`.
//!
//! omp versions that do not write turn-bound `completion-recap` records still
//! save every idle recap to the `session_recaps` table of `history.db`, which
//! omp keeps beside its `sessions` directory. A row carries only the session id
//! and a creation time in whole seconds, so the caller shows it only when it
//! was written after the session's newest conversational entry.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension};

/// How long a read waits for omp's writer before giving up on the recap.
const BUSY_TIMEOUT: Duration = Duration::from_millis(100);

pub(super) struct HistoryRecap {
    pub(super) text: String,
    /// Creation time rounded down to the second, in epoch milliseconds.
    pub(super) created_at_ms: u64,
}

/// `history.db` beside the `sessions` directory that holds `session_file`.
fn history_db(session_file: &Path) -> Option<PathBuf> {
    session_file
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name == "sessions"))
        .and_then(Path::parent)
        .map(|agent_dir| agent_dir.join("history.db"))
        .filter(|db| db.is_file())
}

/// The newest recap omp saved for `session_id`, if its history database has one.
pub(super) fn latest_recap(session_file: &Path, session_id: &str) -> Option<HistoryRecap> {
    let db = history_db(session_file)?;
    let read = || -> rusqlite::Result<Option<HistoryRecap>> {
        let connection = Connection::open_with_flags(
            &db,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection
            .query_row(
                "SELECT recap, created_at FROM session_recaps \
                 WHERE session_id = ?1 ORDER BY created_at DESC, id DESC LIMIT 1",
                [session_id],
                |row| {
                    let seconds: i64 = row.get(1)?;
                    Ok(HistoryRecap {
                        text: row.get(0)?,
                        created_at_ms: u64::try_from(seconds).unwrap_or(0).saturating_mul(1000),
                    })
                },
            )
            .optional()
    };
    match read() {
        Ok(recap) => recap,
        Err(error) => {
            tracing::debug!(db = %db.display(), %error, "omp history recap unavailable");
            None
        }
    }
}
