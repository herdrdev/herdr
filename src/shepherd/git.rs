//! Git tab view for Shepherd (Phase F4b).
//!
//! Follows Shepherd's UI principles:
//! - Pure state (`ShepherdGitState`) separated from runtime/I/O.
//! - Pure render (`render_shepherd_git_panel`) taking `&ShepherdGitState` and `&Palette`.
//! - Mouse-first with keyboard shortcuts (`r` to refresh, `c` to copy/show worktree path).
//! - Read-only: no write operations (push, merge, delete, checkout) in this phase.
//! - Shows worktrees, branches, clean/dirty status, ahead/behind counts, associated
//!   Shepherd agents, and PR status (if `gh` is available).

#![allow(dead_code)]

use std::path::PathBuf;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};

use crate::app::state::Palette;
use super::panel::contains_point;

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
    pub fn from_worktree_info(wt: &GitWorktreeInfo, gh_available: bool) -> Self {
        let (pr_display, pr_url) = if let Some(ref pr) = wt.pr {
            let draft = if pr.is_draft { " [draft]" } else { "" };
            (format!("#{} {}{draft}", pr.number, pr.state), Some(pr.url.clone()))
        } else if gh_available {
            ("—".to_string(), None)
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

    pub fn handle_click(
        &mut self,
        col: u16,
        row: u16,
        render: &ShepherdGitRender,
    ) -> Option<ShepherdGitAction> {
        if contains_point(render.refresh_button, col, row) {
            self.last_action = Some(ShepherdGitAction::Refresh);
            return Some(ShepherdGitAction::Refresh);
        }
        if contains_point(render.copy_button, col, row) {
            if let Some(r) = self.selected_row() {
                let action = ShepherdGitAction::CopyOrShowPath {
                    path: PathBuf::from(&r.path),
                };
                self.last_action = Some(action.clone());
                return Some(action);
            }
        }
        for (rect, idx) in &render.rows {
            if contains_point(*rect, col, row) {
                self.select_index(*idx);
                return None;
            }
        }
        None
    }
}

/// Recorded rendering geometry for hit testing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdGitRender {
    pub area: Rect,
    pub refresh_button: Rect,
    pub copy_button: Rect,
    pub rows: Vec<(Rect, usize)>,
}

/// Pure render function for the Git view.
pub fn render_shepherd_git_panel(
    buffer: &mut Buffer,
    area: Rect,
    state: &ShepherdGitState,
    palette: &Palette,
) -> Option<ShepherdGitRender> {
    if area.height < 6 || area.width < 20 {
        return Some(ShepherdGitRender {
            area,
            ..Default::default()
        });
    }

    let inner = area;
    let stack = crate::ui::modal_stack_areas(inner, 2, 2, 0, 1);
    let base = Style::default().fg(palette.text).bg(palette.panel_bg);

    // 1. Header (row 0: buttons, row 1: subtitle)
    let button_bar = Rect::new(
        stack.header.x + 1,
        stack.header.y,
        stack.header.width.saturating_sub(2),
        1,
    );

    let mut btn_x = button_bar.x;
    let refresh_btn = Rect::new(btn_x, button_bar.y, 16, 1);
    render_git_btn(buffer, refresh_btn, "↻ Refrescar (r)", palette.accent, palette);
    btn_x = btn_x.saturating_add(18);

    let copy_btn = Rect::new(btn_x, button_bar.y, 18, 1);
    render_git_btn(buffer, copy_btn, "📋 Copiar ruta (c)", palette.blue, palette);

    // Subtitle (row 1)
    let gh_text = if !state.gh_available {
        "gh no disponible"
    } else {
        "gh activo"
    };
    let sub_title = format!(
        "{} worktree(s) · {} · solo lectura",
        state.rows.len(),
        gh_text
    );
    buffer.set_stringn(
        stack.header.x + 1,
        stack.header.y + 1,
        &sub_title,
        usize::from(stack.header.width.saturating_sub(2)),
        base.fg(palette.overlay0),
    );

    // 2. Body content (worktree rows)
    let body = stack.content;
    let mut rendered_rows = Vec::new();

    if state.rows.is_empty() {
        let msg = if let Some(ref err) = state.error {
            err.as_str()
        } else {
            "No se encontraron worktrees en el repositorio."
        };
        buffer.set_stringn(
            body.x + 1,
            body.y + 1,
            msg,
            usize::from(body.width.saturating_sub(2)),
            base.fg(if state.error.is_some() { palette.red } else { palette.overlay0 }),
        );
    } else {
        // Table columns layout:
        // Col 0..2: bullet "● " or "· "
        // Col 2..24: RAMA (22 chars)
        // Col 24..38: ESTADO (14 chars)
        // Col 38..48: SYNC (10 chars)
        // Col 48..60: AGENTE (12 chars)
        // Col 60..74: PR (14 chars)
        // Col 74..: RUTA
        let header_y = body.y;
        let col_rama = body.x + 2;
        let col_estado = col_rama + 22;
        let col_sync = col_estado + 14;
        let col_agente = col_sync + 10;
        let col_pr = col_agente + 12;
        let col_ruta = col_pr + 14;

        let th_style = Style::default()
            .fg(palette.overlay1)
            .bg(palette.panel_bg)
            .add_modifier(Modifier::BOLD);

        if col_rama < body.right() {
            buffer.set_stringn(col_rama, header_y, "RAMA", 20, th_style);
        }
        if col_estado < body.right() {
            buffer.set_stringn(col_estado, header_y, "ESTADO", 12, th_style);
        }
        if col_sync < body.right() {
            buffer.set_stringn(col_sync, header_y, "SYNC", 8, th_style);
        }
        if col_agente < body.right() {
            buffer.set_stringn(col_agente, header_y, "AGENTE", 10, th_style);
        }
        if col_pr < body.right() {
            buffer.set_stringn(col_pr, header_y, "PR", 12, th_style);
        }
        if col_ruta < body.right() {
            buffer.set_stringn(col_ruta, header_y, "RUTA", usize::from(body.right().saturating_sub(col_ruta)), th_style);
        }

        let row_start_y = header_y + 1;
        let max_rows = usize::from(body.height.saturating_sub(1));

        for (idx, wt) in state.rows.iter().enumerate().take(max_rows) {
            let row_y = row_start_y + idx as u16;
            let row_rect = Rect::new(body.x, row_y, body.width, 1);
            rendered_rows.push((row_rect, idx));

            let is_selected = idx == state.selected;
            let row_bg = if is_selected {
                palette.surface0
            } else {
                palette.panel_bg
            };
            let row_style = Style::default().fg(palette.text).bg(row_bg);

            // Fill row background
            for x in body.x..body.right() {
                buffer[(x, row_y)].set_style(row_style);
            }

            // Bullet
            let bullet = if is_selected { "● " } else { "· " };
            let bullet_style = if is_selected {
                Style::default().fg(palette.accent).bg(row_bg).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.overlay0).bg(row_bg)
            };
            buffer.set_stringn(body.x + 1, row_y, bullet, 2, bullet_style);

            // Branch + [main]
            let branch_display = if wt.is_main {
                format!("{} [main]", wt.branch)
            } else {
                wt.branch.clone()
            };
            let branch_style = if is_selected {
                row_style.add_modifier(Modifier::BOLD)
            } else {
                row_style
            };
            if col_rama < body.right() {
                buffer.set_stringn(col_rama, row_y, &branch_display, 21, branch_style);
            }

            // Clean / Dirty status
            let (status_text, status_fg) = if wt.is_clean {
                ("limpio".to_string(), palette.green)
            } else {
                (format!("sucio ({})", wt.dirty_count), palette.yellow)
            };
            if col_estado < body.right() {
                buffer.set_stringn(
                    col_estado,
                    row_y,
                    &status_text,
                    13,
                    Style::default().fg(status_fg).bg(row_bg),
                );
            }

            // Sync (ahead / behind)
            let (sync_text, sync_fg) = if wt.ahead == 0 && wt.behind == 0 {
                if wt.upstream.is_some() {
                    ("al día".to_string(), palette.overlay0)
                } else {
                    ("—".to_string(), palette.overlay0)
                }
            } else {
                (format!("+{}/-{}", wt.ahead, wt.behind), palette.accent)
            };
            if col_sync < body.right() {
                buffer.set_stringn(
                    col_sync,
                    row_y,
                    &sync_text,
                    9,
                    Style::default().fg(sync_fg).bg(row_bg),
                );
            }

            // Shepherd Agent
            let agent_text = wt.agent_id.as_deref().unwrap_or("—");
            let agent_fg = if wt.agent_id.is_some() {
                palette.accent
            } else {
                palette.overlay0
            };
            if col_agente < body.right() {
                buffer.set_stringn(
                    col_agente,
                    row_y,
                    agent_text,
                    11,
                    Style::default().fg(agent_fg).bg(row_bg),
                );
            }

            // PR
            let pr_fg = if wt.pr_display != "—" {
                palette.green
            } else {
                palette.overlay0
            };
            if col_pr < body.right() {
                buffer.set_stringn(
                    col_pr,
                    row_y,
                    &wt.pr_display,
                    13,
                    Style::default().fg(pr_fg).bg(row_bg),
                );
            }

            // Path
            if col_ruta < body.right() {
                buffer.set_stringn(
                    col_ruta,
                    row_y,
                    &wt.path,
                    usize::from(body.right().saturating_sub(col_ruta)),
                    Style::default().fg(palette.overlay1).bg(row_bg),
                );
            }
        }
    }

    // 3. Footer (shortcuts / help)
    if let Some(footer_area) = stack.footer {
        let help_text = "r refrescar  ·   c copiar/mostrar ruta  ·   Tab cambiar pestaña  ·   Esc cerrar";
        buffer.set_stringn(
            footer_area.x + 1,
            footer_area.y,
            help_text,
            usize::from(footer_area.width.saturating_sub(2)),
            base.fg(palette.overlay0),
        );
    }

    Some(ShepherdGitRender {
        area,
        refresh_button: refresh_btn,
        copy_button: copy_btn,
        rows: rendered_rows,
    })
}

fn render_git_btn(
    buffer: &mut Buffer,
    rect: Rect,
    label: &str,
    accent: Color,
    _palette: &Palette,
) {
    if rect.width < 3 || rect.height < 1 {
        return;
    }
    let btn_style = Style::default()
        .fg(Color::Black)
        .bg(accent)
        .add_modifier(Modifier::BOLD);
    buffer.set_stringn(rect.x, rect.y, label, usize::from(rect.width), btn_style);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_palette() -> Palette {
        Palette::catppuccin()
    }

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

    #[test]
    fn test_shepherd_git_render_pure_buffer() {
        let palette = test_palette();
        let rows = vec![
            ShepherdGitRow {
                path: "/repo/main".into(),
                branch: "master".into(),
                is_main: true,
                is_clean: true,
                ahead: 0,
                behind: 0,
                upstream: Some("origin/master".into()),
                agent_id: None,
                pr_display: "—".into(),
                pr_url: None,
                ..Default::default()
            },
            ShepherdGitRow {
                path: "/repo/wt-coder".into(),
                branch: "shep/10-task".into(),
                is_main: false,
                is_clean: false,
                dirty_count: 3,
                ahead: 2,
                behind: 0,
                upstream: None,
                agent_id: Some("coder-10".into()),
                pr_display: "#42 OPEN".into(),
                pr_url: Some("https://github.com/pr/42".into()),
            },
        ];
        let mut state = ShepherdGitState::new(rows);
        state.gh_available = true;

        let area = Rect::new(0, 0, 96, 24);
        let mut buffer = Buffer::empty(area);

        let render_hits = render_shepherd_git_panel(&mut buffer, area, &state, &palette).unwrap();
        assert_eq!(render_hits.rows.len(), 2);

        let content = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Verify button bar and columns
        assert!(content.contains("Refrescar (r)"));
        assert!(content.contains("Copiar ruta (c)"));
        assert!(content.contains("RAMA"));
        assert!(content.contains("ESTADO"));
        assert!(content.contains("SYNC"));
        assert!(content.contains("AGENTE"));
        assert!(content.contains("PR"));
        assert!(content.contains("RUTA"));

        // Verify rows
        assert!(content.contains("master [main]"));
        assert!(content.contains("limpio"));
        assert!(content.contains("shep/10-task"));
        assert!(content.contains("sucio (3)"));
        assert!(content.contains("coder-10"));
        assert!(content.contains("#42 OPEN"));
    }

    #[test]
    fn test_shepherd_git_tiny_dimensions_no_panic() {
        let palette = test_palette();
        let state = ShepherdGitState::default();

        for &(w, h) in &[(0, 0), (5, 5), (10, 5), (19, 5), (20, 6)] {
            let mut buffer = Buffer::empty(Rect::new(0, 0, w, h));
            let _ = render_shepherd_git_panel(&mut buffer, Rect::new(0, 0, w, h), &state, &palette);
        }
    }

    #[test]
    fn test_shepherd_git_mouse_click_handling() {
        let rows = vec![
            ShepherdGitRow {
                path: "/repo/main".into(),
                branch: "master".into(),
                ..Default::default()
            },
            ShepherdGitRow {
                path: "/repo/wt-coder".into(),
                branch: "shep/1-coder".into(),
                ..Default::default()
            },
        ];
        let mut state = ShepherdGitState::new(rows);
        let palette = test_palette();
        let area = Rect::new(0, 0, 96, 24);
        let mut buffer = Buffer::empty(area);

        let render = render_shepherd_git_panel(&mut buffer, area, &state, &palette).unwrap();

        // Click row 1
        let row1_rect = render.rows[1].0;
        state.handle_click(row1_rect.x + 2, row1_rect.y, &render);
        assert_eq!(state.selected, 1);

        // Click refresh button
        let action = state.handle_click(render.refresh_button.x + 1, render.refresh_button.y, &render);
        assert_eq!(action, Some(ShepherdGitAction::Refresh));

        // Click copy button
        let action = state.handle_click(render.copy_button.x + 1, render.copy_button.y, &render);
        assert_eq!(
            action,
            Some(ShepherdGitAction::CopyOrShowPath {
                path: PathBuf::from("/repo/wt-coder"),
            })
        );
    }
}
