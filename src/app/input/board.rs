use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::board::{
    Area, BoardView, Card, Column, Detail, Dialog, EditField, Editor, Goal, GoalScope, Update,
};

use super::super::App;

static REMOTE_LINE_BATCH_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

struct RemoteLineBatchPermit;

impl RemoteLineBatchPermit {
    fn try_acquire() -> Option<Self> {
        REMOTE_LINE_BATCH_ACTIVE
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::Acquire,
                std::sync::atomic::Ordering::Relaxed,
            )
            .ok()
            .map(|_| Self)
    }
}

impl Drop for RemoteLineBatchPermit {
    fn drop(&mut self) {
        REMOTE_LINE_BATCH_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
    }
}

fn spawn_remote_line_workers(
    requests: Vec<(crate::board::AgentLink, crate::fleet::HostApiRoute)>,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    note_path: std::path::PathBuf,
    fleet_generation: u64,
    request_id: u64,
    permit: RemoteLineBatchPermit,
) {
    std::thread::spawn(move || {
        let worker_count = requests.len().min(4);
        let queue = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(
            requests,
        )));
        let handles: Vec<_> = (0..worker_count)
            .map(|_| {
                let queue = std::sync::Arc::clone(&queue);
                let event_tx = event_tx.clone();
                let note_path = note_path.clone();
                std::thread::spawn(move || loop {
                    let next = queue
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .pop_front();
                    let Some((link, route)) = next else {
                        return;
                    };
                    let line = crate::fleet::read_board_remote_line(&route, &link.pane_id)
                        .unwrap_or_else(|_| "remote terminal unavailable".into());
                    if event_tx
                        .blocking_send(crate::events::AppEvent::BoardRemoteLinesFetched {
                            note_path: note_path.clone(),
                            fleet_generation,
                            request_id,
                            complete: false,
                            lines: vec![(link, line)],
                        })
                        .is_err()
                    {
                        return;
                    }
                })
            })
            .collect();
        for handle in handles {
            let _ = handle.join();
        }
        drop(permit);
        let _ = event_tx.blocking_send(crate::events::AppEvent::BoardRemoteLinesFetched {
            note_path,
            fleet_generation,
            request_id,
            complete: true,
            lines: Vec::new(),
        });
    });
}

impl App {
    pub(crate) fn refresh_board_remote_lines(&mut self) {
        let Some(view) = self.state.board_view.as_ref() else {
            return;
        };
        let now = crate::day::unix_seconds_now();
        if view.remote_line_fetch_in_flight
            || now.saturating_sub(view.last_remote_line_fetch_unix_s) < 10
        {
            return;
        }
        let mut seen = std::collections::HashSet::new();
        let requests: Vec<_> = view
            .board
            .cards
            .iter()
            .flat_map(|card| &card.agents)
            .filter(|link| link.host != self.state.agent_host_name && seen.insert((*link).clone()))
            .filter_map(|link| {
                self.state
                    .fleet_snapshot
                    .hosts
                    .iter()
                    .find(|host| {
                        host.name == link.host && host.state == crate::fleet::HostState::Reachable
                    })
                    .map(|host| (link.clone(), crate::fleet::HostApiRoute::from_host(host)))
            })
            .collect();
        if requests.is_empty() {
            return;
        }
        let Some(permit) = RemoteLineBatchPermit::try_acquire() else {
            return;
        };
        let note_path = view.note.path.clone();
        let fleet_generation = self.state.fleet_snapshot.config_generation;
        let request_id = crate::board::next_remote_line_request_id();
        if let Some(view) = self.state.board_view.as_mut() {
            view.remote_line_fetch_in_flight = true;
            view.remote_line_request_id = request_id;
            view.last_remote_line_fetch_unix_s = now;
        }
        spawn_remote_line_workers(
            requests,
            self.event_tx.clone(),
            note_path,
            fleet_generation,
            request_id,
            permit,
        );
    }

    pub(crate) fn apply_board_remote_lines(
        &mut self,
        note_path: &std::path::Path,
        fleet_generation: u64,
        request_id: u64,
        complete: bool,
        lines: Vec<(crate::board::AgentLink, String)>,
    ) -> bool {
        let visible = self.state.board_view.is_some();
        let current_generation = self.state.fleet_snapshot.config_generation;
        let view = self
            .state
            .board_view
            .as_mut()
            .or(self.state.board_return.as_mut());
        let Some(view) = view.filter(|view| {
            view.note.path == note_path && view.remote_line_request_id == request_id
        }) else {
            if complete {
                self.refresh_board_remote_lines();
            }
            return false;
        };
        if complete {
            view.remote_line_fetch_in_flight = false;
        }
        if fleet_generation != current_generation {
            if complete {
                self.refresh_board_remote_lines();
            }
            return false;
        }
        let changed = !lines.is_empty();
        for (link, line) in lines {
            if view
                .board
                .cards
                .iter()
                .any(|card| card.agents.contains(&link))
            {
                view.agent_lines.insert(link, (u64::MAX, line));
            }
        }
        visible && changed
    }

    pub(crate) fn board_insert_text(&mut self, text: &str) -> bool {
        let Some(view) = self.state.board_view.as_mut() else {
            return false;
        };
        if let Some(editor) = view.editor.as_mut() {
            editor.text.push_str(text);
            return true;
        }
        let Some(dialog) = view.dialog.as_mut() else {
            return false;
        };
        match dialog {
            Dialog::Card {
                title,
                description,
                new_goal,
                field,
                ..
            } => match *field {
                0 => title.extend(text.chars().filter(|ch| *ch != '\n' && *ch != '\r')),
                1 => description.push_str(text),
                4 => new_goal.extend(text.chars().filter(|ch| *ch != '\n' && *ch != '\r')),
                _ => {}
            },
            Dialog::Goal {
                title,
                todos,
                field,
                ..
            } => match *field {
                0 => title.extend(text.chars().filter(|ch| *ch != '\n' && *ch != '\r')),
                2 => todos.push_str(text),
                _ => {}
            },
        }
        true
    }

    pub(crate) fn handle_board_mouse(&mut self, mouse: MouseEvent) {
        use crate::ui::board::BoardHit;
        let area = self.state.view.terminal_area;
        let hit = crate::ui::board::hit_at(&self.state, area, mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(BoardHit::Card { id, .. }) = &hit {
                    if let Some(view) = self.state.board_view.as_mut() {
                        view.drag_id = Some(id.clone());
                        view.drag_moved = false;
                    }
                } else {
                    self.activate_board_hit(hit);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(view) = self.state.board_view.as_mut() {
                    if view.drag_id.is_some() {
                        view.drag_moved = true;
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let drag = self.state.board_view.as_mut().and_then(|view| {
                    let id = view.drag_id.take()?;
                    let moved = std::mem::take(&mut view.drag_moved);
                    Some((id, moved))
                });
                if let Some((id, true)) = drag {
                    let target = self.state.board_view.as_ref().and_then(|view| {
                        crate::ui::board::column_rects(
                            crate::ui::board::board_rect(area, view),
                            view.column,
                        )
                        .into_iter()
                        .find(|(_, rect)| {
                            mouse.column >= rect.x
                                && mouse.column < rect.right()
                                && mouse.row >= rect.y
                                && mouse.row < rect.bottom()
                        })
                        .map(|(column, _)| column)
                    });
                    if let Some(column) = target {
                        if let Some(view) = self.state.board_view.as_mut() {
                            let before = view.board.clone();
                            if let Some(card) = view.board.card_mut(&id) {
                                card.column = column;
                            }
                            view.column = column;
                            view.row = 0;
                            if !view.persist() {
                                view.board = before;
                            }
                        }
                    }
                } else if drag.is_some() {
                    self.activate_board_hit(hit);
                }
            }
            _ => {}
        }
    }

    fn activate_board_hit(&mut self, hit: Option<crate::ui::board::BoardHit>) {
        use crate::ui::board::BoardHit;
        match hit {
            Some(BoardHit::NewGoal) => {
                if let Some(view) = self.state.board_view.as_mut() {
                    view.dialog = Some(Dialog::Goal {
                        title: String::new(),
                        scope: GoalScope::Week,
                        todos: String::new(),
                        field: 0,
                    });
                }
            }
            Some(BoardHit::GoalPage(delta)) => self.page_board_goals(delta),
            Some(BoardHit::NewCard) => {
                if let Some(view) = self.state.board_view.as_mut() {
                    view.column = Column::Draft;
                    view.dialog = Some(Dialog::Card {
                        title: String::new(),
                        description: String::new(),
                        area: Area::Harness,
                        goal: None,
                        new_goal: String::new(),
                        field: 0,
                    });
                }
            }
            Some(BoardHit::Column(column)) => {
                if let Some(view) = self.state.board_view.as_mut() {
                    view.column = column;
                    view.row = 0;
                }
            }
            Some(BoardHit::Card { id, spawn, agent }) => {
                self.select_board_card(&id);
                if spawn {
                    self.spawn_board_card(&id);
                } else if let Some(index) = agent {
                    let link = self
                        .state
                        .board_view
                        .as_ref()
                        .and_then(|view| view.board.card(&id))
                        .and_then(|card| card.agents.get(index))
                        .cloned();
                    if let Some(link) = link {
                        self.jump_board_agent(&link);
                    }
                } else if let Some(view) = self.state.board_view.as_mut() {
                    view.detail = Some(Detail {
                        card_id: id,
                        agent_tab: false,
                        agent_row: 0,
                    });
                }
            }
            Some(BoardHit::DialogSave) => self.save_board_dialog(),
            Some(BoardHit::DialogField(index)) => {
                if let Some(view) = self.state.board_view.as_mut() {
                    let goal_ids: Vec<_> = view
                        .board
                        .goals
                        .iter()
                        .map(|goal| goal.id.clone())
                        .collect();
                    if let Some(dialog) = view.dialog.as_mut() {
                        match dialog {
                            Dialog::Card {
                                area, goal, field, ..
                            } => {
                                *field = index;
                                if index == 2 {
                                    *area = area.next();
                                }
                                if index == 3 {
                                    let current = goal.as_ref().and_then(|id| {
                                        goal_ids.iter().position(|candidate| candidate == id)
                                    });
                                    *goal = match current {
                                        None => goal_ids.first().cloned(),
                                        Some(i) => goal_ids.get(i + 1).cloned(),
                                    };
                                }
                            }
                            Dialog::Goal { scope, field, .. } => {
                                *field = index;
                                if index == 1 {
                                    *scope = scope.next();
                                }
                            }
                        }
                    }
                }
            }
            Some(BoardHit::DialogCancel) => {
                if let Some(view) = self.state.board_view.as_mut() {
                    view.dialog = None;
                }
            }
            Some(BoardHit::EditorSave) => {
                self.handle_board_editor_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
            }
            Some(BoardHit::EditorCancel) => {
                if let Some(view) = self.state.board_view.as_mut() {
                    view.editor = None;
                }
            }
            Some(BoardHit::DetailTab(agent_tab)) => {
                if let Some(detail) = self
                    .state
                    .board_view
                    .as_mut()
                    .and_then(|view| view.detail.as_mut())
                {
                    detail.agent_tab = agent_tab;
                }
            }
            Some(BoardHit::DetailAgent(index)) => {
                let link = self
                    .state
                    .board_view
                    .as_ref()
                    .and_then(|view| {
                        view.detail
                            .as_ref()
                            .and_then(|detail| view.board.card(&detail.card_id))
                    })
                    .and_then(|card| card.agents.get(index))
                    .cloned();
                if let Some(link) = link {
                    self.jump_board_agent(&link);
                }
            }
            Some(BoardHit::DetailEdit) => {
                let agent_tab = self
                    .state
                    .board_view
                    .as_ref()
                    .and_then(|view| view.detail.as_ref())
                    .is_some_and(|detail| detail.agent_tab);
                self.handle_board_detail_key(KeyEvent::new(
                    KeyCode::Char(if agent_tab { 's' } else { 'e' }),
                    KeyModifiers::NONE,
                ));
            }
            Some(BoardHit::DetailAppend) => {
                self.handle_board_detail_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
            }
            None => {}
        }
    }

    fn select_board_card(&mut self, id: &str) {
        let Some(view) = self.state.board_view.as_ref() else {
            return;
        };
        let Some(card) = view.board.card(id) else {
            return;
        };
        let column = card.column;
        let mut cards: Vec<_> = view
            .board
            .cards
            .iter()
            .filter(|card| card.column == column)
            .collect();
        if column == Column::InProgress {
            cards.sort_by_key(|card| self.state.board_lane(card));
        }
        let row = cards.iter().position(|card| card.id == id).unwrap_or(0);
        if let Some(view) = self.state.board_view.as_mut() {
            view.column = column;
            view.row = row;
        }
    }

    fn page_board_goals(&mut self, delta: i8) {
        let page_size = crate::ui::board::goal_page_size(self.state.view.terminal_area.width);
        if let Some(view) = self.state.board_view.as_mut() {
            let max_offset = view.board.goals.len().saturating_sub(page_size);
            view.goal_offset = if delta < 0 {
                view.goal_offset.saturating_sub(page_size)
            } else {
                view.goal_offset.saturating_add(page_size).min(max_offset)
            };
        }
    }

    pub(crate) fn toggle_board_view(&mut self) {
        if self.state.board_view.is_some() {
            self.state.board_view = None;
            return;
        }
        match BoardView::open() {
            Ok(view) => {
                self.state.clear_usage_view();
                self.state.symphony_detail = None;
                self.state.loop_run_history_detail = None;
                self.state.aloop_run_detail = None;
                self.state.dock_object_preview = None;
                self.state.work_view = None;
                self.state.home = None;
                self.state.inbox = None;
                self.state
                    .set_server_mode(crate::app::state::Mode::Terminal);
                self.state.board_view = Some(view);
                self.state.release_sidebar_focus_to_surface();
                self.refresh_board_remote_lines();
            }
            Err(error) => self.state.config_diagnostic = Some(format!("board: {error}")),
        }
    }

    pub(crate) fn handle_board_key(&mut self, key: KeyEvent) -> bool {
        let Some(view) = self.state.board_view.as_ref() else {
            return false;
        };
        if view.editor.is_some() {
            return self.handle_board_editor_key(key);
        }
        if view.dialog.is_some() {
            return self.handle_board_dialog_key(key);
        }
        if view.detail.is_some() {
            return self.handle_board_detail_key(key);
        }

        let selected_id = view.selected_id(&self.state);
        if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if let Some(view) = self.state.board_view.as_mut() {
                view.persist();
            }
            return true;
        }
        match key.code {
            KeyCode::Esc => {
                self.state.board_view = None;
            }
            KeyCode::Left | KeyCode::Right => {
                let delta = if key.code == KeyCode::Left { -1 } else { 1 };
                if self
                    .state
                    .board_view
                    .as_ref()
                    .is_some_and(|view| view.move_mode)
                {
                    if let Some(id) = selected_id {
                        let view = self.state.board_view.as_mut().expect("board open");
                        let before = view.board.clone();
                        let new_column = view.column.move_by(delta);
                        if let Some(card) = view.board.card_mut(&id) {
                            card.column = new_column;
                        }
                        view.column = new_column;
                        view.row = 0;
                        view.move_mode = false;
                        if !view.persist() {
                            view.board = before;
                        }
                    }
                } else if let Some(view) = self.state.board_view.as_mut() {
                    view.column = view.column.move_by(delta);
                    view.row = 0;
                }
            }
            KeyCode::Up | KeyCode::Down => {
                let count = self
                    .state
                    .board_view
                    .as_ref()
                    .map(|view| view.visible_cards(&self.state).len())
                    .unwrap_or(0);
                if let Some(view) = self.state.board_view.as_mut() {
                    view.row = if key.code == KeyCode::Up {
                        view.row.saturating_sub(1)
                    } else {
                        (view.row + 1).min(count.saturating_sub(1))
                    };
                }
            }
            KeyCode::Enter => {
                if let Some(id) = selected_id {
                    if let Some(view) = self.state.board_view.as_mut() {
                        view.detail = Some(Detail {
                            card_id: id,
                            agent_tab: false,
                            agent_row: 0,
                        });
                    }
                }
            }
            KeyCode::Char('m') => {
                if let Some(view) = self.state.board_view.as_mut() {
                    view.move_mode = true;
                }
            }
            KeyCode::Char('n') if view.column == Column::Draft => {
                self.state.board_view.as_mut().expect("board open").dialog = Some(Dialog::Card {
                    title: String::new(),
                    description: String::new(),
                    area: Area::Harness,
                    goal: None,
                    new_goal: String::new(),
                    field: 0,
                });
            }
            KeyCode::Char('g') => {
                self.state.board_view.as_mut().expect("board open").dialog = Some(Dialog::Goal {
                    title: String::new(),
                    scope: GoalScope::Week,
                    todos: String::new(),
                    field: 0,
                });
            }
            KeyCode::Char('[') => self.page_board_goals(-1),
            KeyCode::Char(']') => self.page_board_goals(1),
            KeyCode::Char('s') => {
                if let Some(id) = selected_id {
                    self.spawn_board_card(&id);
                }
            }
            _ => {}
        }
        true
    }

    fn handle_board_dialog_key(&mut self, key: KeyEvent) -> bool {
        let Some(view) = self.state.board_view.as_mut() else {
            return true;
        };
        let Some(dialog) = view.dialog.as_mut() else {
            return true;
        };
        if key.code == KeyCode::Esc {
            view.dialog = None;
            return true;
        }
        if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.save_board_dialog();
            return true;
        }
        match dialog {
            Dialog::Card {
                title,
                description,
                area,
                goal,
                new_goal,
                field,
            } => match key.code {
                KeyCode::Tab => *field = (*field + 1) % 5,
                KeyCode::BackTab => *field = (*field + 4) % 5,
                KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if *field == 2 => {
                    *area = area.next()
                }
                KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if *field == 3 => {
                    let ids: Vec<_> = view
                        .board
                        .goals
                        .iter()
                        .map(|goal| goal.id.clone())
                        .collect();
                    let index = goal
                        .as_ref()
                        .and_then(|id| ids.iter().position(|candidate| candidate == id));
                    *goal = match index {
                        None => ids.first().cloned(),
                        Some(i) => ids.get(i + 1).cloned(),
                    };
                }
                KeyCode::Char(c) => match *field {
                    0 if c != '\n' => title.push(c),
                    1 => description.push(c),
                    4 => new_goal.push(c),
                    _ => {}
                },
                KeyCode::Backspace => match *field {
                    0 => {
                        title.pop();
                    }
                    1 => {
                        description.pop();
                    }
                    4 => {
                        new_goal.pop();
                    }
                    _ => {}
                },
                KeyCode::Enter if *field == 1 => description.push('\n'),
                _ => {}
            },
            Dialog::Goal {
                title,
                scope,
                todos,
                field,
            } => match key.code {
                KeyCode::Tab => *field = (*field + 1) % 3,
                KeyCode::BackTab => *field = (*field + 2) % 3,
                KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if *field == 1 => {
                    *scope = scope.next()
                }
                KeyCode::Char(c) => match *field {
                    0 => title.push(c),
                    2 => todos.push(c),
                    _ => {}
                },
                KeyCode::Backspace => match *field {
                    0 => {
                        title.pop();
                    }
                    2 => {
                        todos.pop();
                    }
                    _ => {}
                },
                KeyCode::Enter if *field == 2 => todos.push('\n'),
                _ => {}
            },
        }
        true
    }

    fn save_board_dialog(&mut self) {
        let Some(view) = self.state.board_view.as_mut() else {
            return;
        };
        let Some(dialog) = view.dialog.take() else {
            return;
        };
        let retry = dialog.clone();
        let before = view.board.clone();
        match dialog {
            Dialog::Card {
                title,
                description,
                area,
                goal,
                new_goal,
                ..
            } => {
                let title = title.trim();
                if title.is_empty() || title.contains('\n') {
                    view.error = Some("title must be one non-empty line".into());
                    view.dialog = Some(retry);
                    return;
                }
                let new_goal_id = if new_goal.trim().is_empty() {
                    None
                } else {
                    match crate::board::new_id("goal") {
                        Ok(id) => Some(id),
                        Err(error) => {
                            view.error = Some(error);
                            view.dialog = Some(retry);
                            return;
                        }
                    }
                };
                let card_id = match crate::board::new_id("card") {
                    Ok(id) => id,
                    Err(error) => {
                        view.error = Some(error);
                        view.dialog = Some(retry);
                        return;
                    }
                };
                if let Some(id) = &new_goal_id {
                    view.board.goals.push(Goal {
                        id: id.clone(),
                        title: new_goal.trim().into(),
                        scope: GoalScope::Week,
                    });
                }
                let goal_id = new_goal_id.or(goal);
                view.board.cards.push(Card {
                    id: card_id,
                    title: title.into(),
                    description,
                    area,
                    column: Column::Draft,
                    goal_id,
                    agent_summary: String::new(),
                    updates: Vec::new(),
                    agents: Vec::new(),
                });
                view.column = Column::Draft;
                view.row = view
                    .board
                    .cards
                    .iter()
                    .filter(|card| card.column == Column::Draft)
                    .count()
                    .saturating_sub(1);
            }
            Dialog::Goal {
                title,
                scope,
                todos,
                ..
            } => {
                let title = title.trim();
                if title.is_empty() || title.contains('\n') {
                    view.error = Some("goal title must be one non-empty line".into());
                    view.dialog = Some(retry);
                    return;
                }
                let id = match crate::board::new_id("goal") {
                    Ok(id) => id,
                    Err(error) => {
                        view.error = Some(error);
                        view.dialog = Some(retry);
                        return;
                    }
                };
                let todos: Vec<_> = todos
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .collect();
                let card_ids: Vec<_> = match todos
                    .iter()
                    .map(|_| crate::board::new_id("card"))
                    .collect::<Result<_, _>>()
                {
                    Ok(ids) => ids,
                    Err(error) => {
                        view.error = Some(error);
                        view.dialog = Some(retry);
                        return;
                    }
                };
                view.board.goals.push(Goal {
                    id: id.clone(),
                    title: title.into(),
                    scope,
                });
                for (todo, card_id) in todos.into_iter().zip(card_ids) {
                    view.board.cards.push(Card {
                        id: card_id,
                        title: todo.into(),
                        description: String::new(),
                        area: Area::Harness,
                        column: Column::Draft,
                        goal_id: Some(id.clone()),
                        agent_summary: String::new(),
                        updates: Vec::new(),
                        agents: Vec::new(),
                    });
                }
                view.column = Column::Draft;
            }
        }
        if !view.persist() {
            view.board = before;
            view.dialog = Some(retry);
        }
    }

    fn handle_board_detail_key(&mut self, key: KeyEvent) -> bool {
        let Some(view) = self.state.board_view.as_mut() else {
            return true;
        };
        let Some(detail) = view.detail.as_mut() else {
            return true;
        };
        let Some(card) = view.board.card(&detail.card_id) else {
            view.detail = None;
            return true;
        };
        match key.code {
            KeyCode::Esc => view.detail = None,
            KeyCode::Tab | KeyCode::BackTab => detail.agent_tab = !detail.agent_tab,
            KeyCode::Up => detail.agent_row = detail.agent_row.saturating_sub(1),
            KeyCode::Down => {
                detail.agent_row = (detail.agent_row + 1).min(card.agents.len().saturating_sub(1))
            }
            KeyCode::Char('e') if !detail.agent_tab => {
                view.editor = Some(Editor {
                    field: EditField::HumanReplace,
                    text: card.description.clone(),
                })
            }
            KeyCode::Char('a') if !detail.agent_tab => {
                view.editor = Some(Editor {
                    field: EditField::HumanAppend,
                    text: String::new(),
                })
            }
            KeyCode::Char('s') if detail.agent_tab => {
                view.editor = Some(Editor {
                    field: EditField::AgentSummary,
                    text: card.agent_summary.clone(),
                })
            }
            KeyCode::Char('a') if detail.agent_tab => {
                view.editor = Some(Editor {
                    field: EditField::AgentUpdate,
                    text: String::new(),
                })
            }
            KeyCode::Enter => {
                let target = card.agents.get(detail.agent_row).cloned();
                if let Some(agent) = target {
                    self.jump_board_agent(&agent);
                }
            }
            _ => {}
        }
        true
    }

    fn handle_board_editor_key(&mut self, key: KeyEvent) -> bool {
        let Some(view) = self.state.board_view.as_mut() else {
            return true;
        };
        let Some(editor) = view.editor.as_mut() else {
            return true;
        };
        if key.code == KeyCode::Esc {
            view.editor = None;
            return true;
        }
        if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::CONTROL) {
            let before = view.board.clone();
            let Some(editor) = view.editor.take() else {
                return true;
            };
            let retry = editor.clone();
            let Some(detail) = view.detail.as_ref() else {
                return true;
            };
            if let Some(card) = view.board.card_mut(&detail.card_id) {
                match editor.field {
                    EditField::HumanReplace => card.description = editor.text,
                    EditField::HumanAppend => {
                        if !card.description.is_empty() {
                            card.description.push('\n');
                        }
                        card.description.push_str(&editor.text);
                    }
                    EditField::AgentSummary => card.agent_summary = editor.text,
                    EditField::AgentUpdate => {
                        if !editor.text.trim().is_empty() {
                            card.updates.push(Update {
                                at: crate::day::unix_seconds_now(),
                                text: editor.text,
                            });
                        }
                    }
                }
            }
            if !view.persist() {
                view.board = before;
                view.editor = Some(retry);
            }
            return true;
        }
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                editor.text.push(c)
            }
            KeyCode::Backspace => {
                editor.text.pop();
            }
            KeyCode::Enter => editor.text.push('\n'),
            _ => {}
        }
        true
    }

    fn jump_board_agent(&mut self, agent: &crate::board::AgentLink) {
        if agent.host != self.state.agent_host_name {
            self.state.board_return = self.state.board_view.take();
            self.open_fleet_host_focused(&agent.host, Some(&agent.pane_id));
            return;
        }
        let Some((ws_idx, pane_id)) = self.parse_current_public_pane_id(&agent.pane_id) else {
            return;
        };
        self.state.board_return = self.state.board_view.take();
        self.focus_pane_internal_via_api(ws_idx, pane_id);
    }

    fn spawn_board_card(&mut self, id: &str) {
        let Some(view) = self.state.board_view.as_ref() else {
            return;
        };
        let Some(card) = view.board.card(id).cloned() else {
            return;
        };
        if card.column == Column::Draft || !card.agents.is_empty() {
            return;
        }
        let directory = match card.area {
            Area::Personal => view
                .note
                .path
                .parent()
                .and_then(std::path::Path::parent)
                .map(std::path::Path::to_path_buf),
            Area::Scalable | Area::Harness => {
                let repo_name = if card.area == Area::Scalable {
                    "scalablev2"
                } else {
                    "scalable-agent-fleet"
                };
                self.state
                    .projects
                    .iter()
                    .flat_map(|project| &project.repos)
                    .find(|repo| repo.name == repo_name)
                    .map(|repo| repo.path.clone())
                    .or_else(|| {
                        std::env::var_os("HOME").map(|home| {
                            std::path::PathBuf::from(home).join("Repos").join(repo_name)
                        })
                    })
            }
        };
        let Some(directory) = directory.filter(|path| path.is_dir()) else {
            if let Some(view) = self.state.board_view.as_mut() {
                view.error = Some(format!("no checkout configured for {}", card.area.label()));
            }
            return;
        };
        let mut home = self.state.new_home_state();
        home.set_directory(directory);
        home.workspace = crate::app::home::HomeWorkspace::CurrentCheckout;
        home.target = crate::app::home::HomeTarget::NewSpace;
        home.prompt = if card.description.trim().is_empty() {
            card.title.clone()
        } else {
            format!("{}\n\n{}", card.title, card.description)
        };
        let plan = match home.dispatch_plan() {
            Ok(plan) => plan,
            Err(error) => {
                if let Some(view) = self.state.board_view.as_mut() {
                    view.error = Some(error);
                }
                return;
            }
        };
        if let Err(error) = self.dispatch_home_composer(plan) {
            if let Some(view) = self.state.board_view.as_mut() {
                view.error = Some(error.to_string());
            }
            return;
        }
        let pane_id = self.state.active.and_then(|ws_idx| {
            let pane = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
            self.public_pane_id(ws_idx, pane)
        });
        let Some(pane_id) = pane_id else {
            if let Some(view) = self.state.board_view.as_mut() {
                view.error = Some("agent started, but its pane could not be linked".into());
            }
            self.state.board_return = self.state.board_view.take();
            return;
        };
        if let Some(view) = self.state.board_view.as_mut() {
            let link = crate::board::AgentLink {
                host: self.state.agent_host_name.clone(),
                pane_id,
            };
            if let Some(card) = view.board.card_mut(id) {
                card.agents.push(link.clone());
                card.column = Column::InProgress;
            }
            if !view.persist()
                && view
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("changed in Obsidian"))
            {
                view.persist_spawn_link(id, link);
            }
            self.state.board_return = self.state.board_view.take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_line_batch_remains_bounded_across_board_reopens() {
        let first = RemoteLineBatchPermit::try_acquire().expect("first board fetch");
        assert!(RemoteLineBatchPermit::try_acquire().is_none());
        drop(first);
        let reopened = RemoteLineBatchPermit::try_acquire().expect("reopen can fetch after finish");
        drop(reopened);
    }

    #[test]
    fn stale_remote_line_result_cannot_change_reopened_board() {
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let date = time::Date::from_calendar_date(2026, time::Month::September, 28).expect("date");
        let note =
            crate::board::WeekNote::for_date(std::path::Path::new("/vault"), date).expect("note");
        let link = crate::board::AgentLink {
            host: "ub1".into(),
            pane_id: "w:p1".into(),
        };
        let board = crate::board::Board {
            goals: Vec::new(),
            cards: vec![Card {
                id: "card".into(),
                title: "Review".into(),
                description: String::new(),
                area: Area::Harness,
                column: Column::InProgress,
                goal_id: None,
                agent_summary: String::new(),
                updates: Vec::new(),
                agents: vec![link.clone()],
            }],
        };
        let mut reopened = BoardView::test_new(note.clone(), board);
        reopened.remote_line_request_id = 2;
        reopened.remote_line_fetch_in_flight = true;
        app.state.board_view = Some(reopened);
        let generation = app.state.fleet_snapshot.config_generation;
        assert!(!app.apply_board_remote_lines(
            &note.path,
            generation,
            1,
            true,
            vec![(link.clone(), "stale".into())]
        ));
        let view = app.state.board_view.as_ref().expect("board");
        assert!(view.remote_line_fetch_in_flight);
        assert!(!view.agent_lines.contains_key(&link));
        assert!(app.apply_board_remote_lines(
            &note.path,
            generation,
            2,
            false,
            vec![(link.clone(), "current".into())]
        ));
        let view = app.state.board_view.as_ref().expect("board");
        assert!(view.remote_line_fetch_in_flight);
        assert_eq!(
            view.agent_lines.get(&link).map(|(_, line)| line.as_str()),
            Some("current")
        );
        assert!(!app.apply_board_remote_lines(&note.path, generation, 2, true, Vec::new()));
        assert!(
            !app.state
                .board_view
                .as_ref()
                .expect("board")
                .remote_line_fetch_in_flight
        );
    }
}
