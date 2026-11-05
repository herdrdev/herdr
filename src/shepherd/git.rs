//! Git tab view for Shepherd (Phase F4b).
//!
//! Follows Shepherd's UI principles:
//! - Pure state (`ShepherdGitState`) separated from runtime/I/O.
//! - Mouse-first with keyboard shortcuts (`r` to refresh, `c` to copy/show worktree path).
//! - Read-only: no write operations (push, merge, delete, checkout) in this phase.
//! - Shows worktrees, branches, clean/dirty status, ahead/behind counts, associated
//!   Shepherd agents, and PR status (if `gh` is available).

#![allow(dead_code)]

use crossterm::event::{KeyCode, KeyEvent};
use std::path::PathBuf;

// Re-export pure parsers and models from shepherd_core::git
#[allow(unused_imports)]
pub use shepherd_core::git::{
    parse_gh_pr_list_json, parse_git_status_porcelain, parse_git_worktree_porcelain,
    parse_rev_list_left_right, GitPrSummary, GitRepoStatus, GitWorktreeInfo, ParsedGitStatus,
    ParsedPr, ParsedWorktree,
};

/// Pure presentation row for a Git worktree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdGitRow {
    pub path: String,
    pub branch: String,
    pub is_main: bool,
    pub is_clean: bool,
    pub dirty_count: usize,
    pub ahead: u32,
    pub behind: u32,
    pub upstream: Option<String>,
    pub agent_id: Option<String>,
    pub pr_display: String,
    pub pr_url: Option<String>,
}

impl ShepherdGitRow {
    pub fn from_worktree_info(wt: &GitWorktreeInfo, _gh_available: bool) -> Self {
        let (pr_display, pr_url) = if let Some(ref pr) = wt.pr {
            let draft = if pr.is_draft { " [draft]" } else { "" };
            (
                format!("#{} {}{draft}", pr.number, pr.state),
                Some(pr.url.clone()),
            )
        } else {
            ("—".to_string(), None)
        };

        Self {
            path: wt.path.clone(),
            branch: wt.branch.clone(),
            is_main: wt.is_main,
            is_clean: wt.is_clean,
            dirty_count: wt.dirty_count,
            ahead: wt.ahead,
            behind: wt.behind,
            upstream: wt.upstream.clone(),
            agent_id: wt.agent_id.clone(),
            pr_display,
            pr_url,
        }
    }
}

/// Actions emitted by the Git view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShepherdGitAction {
    Refresh,
    CopyOrShowPath { path: PathBuf },
    CreateWorktree { path: String, branch: String },
    CreateBranch { branch: String, base: String },
    PublishBranch { branch: String },
    RemoveWorktree { path: String },
}

/// Pure presentation state for the Git view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdGitState {
    pub open: bool,
    pub selected: usize,
    pub scroll: u16,
    pub rows: Vec<ShepherdGitRow>,
    pub gh_available: bool,
    pub gh_note: Option<String>,
    pub error: Option<String>,
    pub last_action: Option<ShepherdGitAction>,
}

impl ShepherdGitState {
    pub fn new(rows: Vec<ShepherdGitRow>) -> Self {
        Self {
            open: false,
            selected: 0,
            scroll: 0,
            rows,
            gh_available: true,
            gh_note: None,
            error: None,
            last_action: None,
        }
    }

    pub fn update_from_status(&mut self, status: &GitRepoStatus) {
        self.rows = status
            .worktrees
            .iter()
            .map(|wt| ShepherdGitRow::from_worktree_info(wt, status.gh_available))
            .collect();
        self.gh_available = status.gh_available;
        self.gh_note = status.gh_note.clone();
        self.error = status.error.clone();
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub fn open_panel(&mut self) {
        self.open = true;
    }

    pub fn close_panel(&mut self) {
        self.open = false;
    }

    pub fn select_next(&mut self) {
        if !self.rows.is_empty() {
            self.selected = (self.selected + 1).min(self.rows.len().saturating_sub(1));
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn select_index(&mut self, index: usize) {
        if index < self.rows.len() {
            self.selected = index;
        }
    }

    pub fn selected_row(&self) -> Option<&ShepherdGitRow> {
        self.rows.get(self.selected)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<ShepherdGitAction> {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_prev();
                None
            }
            KeyCode::Home => {
                self.selected = 0;
                None
            }
            KeyCode::End => {
                if !self.rows.is_empty() {
                    self.selected = self.rows.len().saturating_sub(1);
                }
                None
            }
            KeyCode::Char('r') => {
                self.last_action = Some(ShepherdGitAction::Refresh);
                Some(ShepherdGitAction::Refresh)
            }
            KeyCode::Char('c') | KeyCode::Char('y') | KeyCode::Enter => {
                if let Some(row) = self.selected_row() {
                    let action = ShepherdGitAction::CopyOrShowPath {
                        path: PathBuf::from(&row.path),
                    };
                    self.last_action = Some(action.clone());
                    Some(action)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shepherd_git_state_navigation() {
        let rows = vec![
            ShepherdGitRow {
                path: "/repo".into(),
                branch: "master".into(),
                is_main: true,
                is_clean: true,
                ..Default::default()
            },
            ShepherdGitRow {
                path: "/repo/wt1".into(),
                branch: "shep/1-auth".into(),
                is_main: false,
                is_clean: false,
                dirty_count: 2,
                agent_id: Some("coder-1".into()),
                ..Default::default()
            },
            ShepherdGitRow {
                path: "/repo/wt2".into(),
                branch: "shep/2-ui".into(),
                is_main: false,
                is_clean: true,
                agent_id: Some("coder-2".into()),
                ..Default::default()
            },
        ];
        let mut state = ShepherdGitState::new(rows);
        assert_eq!(state.selected, 0);

        state.select_next();
        assert_eq!(state.selected, 1);
        assert_eq!(state.selected_row().unwrap().branch, "shep/1-auth");

        state.select_next();
        assert_eq!(state.selected, 2);

        // Clamped at end
        state.select_next();
        assert_eq!(state.selected, 2);

        state.select_prev();
        assert_eq!(state.selected, 1);

        state.select_prev();
        assert_eq!(state.selected, 0);

        // Clamped at 0
        state.select_prev();
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn test_shepherd_git_state_key_actions() {
        let rows = vec![ShepherdGitRow {
            path: "/path/to/worktree-1".into(),
            branch: "shep/1-auth".into(),
            is_main: false,
            is_clean: true,
            ..Default::default()
        }];
        let mut state = ShepherdGitState::new(rows);

        // Press 'r' -> Refresh
        let action = state.handle_key(KeyEvent::new(
            crossterm::event::KeyCode::Char('r'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(action, Some(ShepherdGitAction::Refresh));

        // Press 'c' -> CopyOrShowPath
        let action = state.handle_key(KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(
            action,
            Some(ShepherdGitAction::CopyOrShowPath {
                path: PathBuf::from("/path/to/worktree-1"),
            })
        );
    }
}
