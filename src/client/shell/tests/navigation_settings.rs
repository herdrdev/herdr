use super::*;
use crate::config::NavigationHistoryScope;

const CHOICES: [&str; 4] = [
    "across spaces",
    "current space only",
    "mouse Back/Forward enabled",
    "mouse Back/Forward disabled",
];

struct TemporaryConfig {
    path: std::path::PathBuf,
    previous_path: Option<std::ffi::OsString>,
}

impl TemporaryConfig {
    fn new(content: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "herdr-navigation-settings-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, content).unwrap();
        let previous_path = std::env::var_os(crate::config::CONFIG_PATH_ENV_VAR);
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);
        Self {
            path,
            previous_path,
        }
    }

    fn load(&self) -> Config {
        toml::from_str(&std::fs::read_to_string(&self.path).unwrap()).unwrap()
    }
}

impl Drop for TemporaryConfig {
    fn drop(&mut self) {
        match self.previous_path.as_ref() {
            Some(path) => std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, path),
            None => std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR),
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

fn state_with_visits(config: &Config) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(config));
    let mut projected = snapshot();
    let mut second_tab = projected.tabs[0].clone();
    second_tab.tab_id = "tab_2".into();
    second_tab.number = 2;
    second_tab.focused = false;
    projected.tabs.push(second_tab);
    let mut second_pane = projected.panes[0].clone();
    second_pane.pane_id = "pane_2".into();
    second_pane.tab_id = "tab_2".into();
    second_pane.focused = false;
    projected.panes.push(second_pane);
    state.set_snapshot(Box::new(projected.clone()));
    projected.revision += 1;
    projected.focused_tab_id = Some("tab_2".into());
    projected.focused_pane_id = Some("pane_2".into());
    projected.workspaces[0].active_tab_id = "tab_2".into();
    projected.tabs[0].focused = false;
    projected.tabs[1].focused = true;
    projected.panes[0].focused = false;
    projected.panes[1].focused = true;
    let mut pane_surface = surface();
    pane_surface.projection_revision = projected.revision;
    pane_surface.panes[0].pane_id = "pane_2".into();
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(pane_surface);
    state
}

fn click(state: &mut ClientShellState, rect: Rect) -> ClientShellInput {
    assert!(rect.width > 0 && rect.height > 0, "missing mouse target");
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x + rect.width / 2,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })])
}

fn assert_client_only(outcome: &ClientShellInput) {
    assert!(outcome.actions.is_empty(), "{:?}", outcome.actions);
    assert!(outcome.requests.is_empty(), "{:?}", outcome.requests);
}

fn open_navigation(state: &mut ClientShellState) {
    state.open_settings_overlay();
    state.compose(106, 18).unwrap();
    let tab = state
        .hits
        .settings_tabs
        .iter()
        .find(|(_, section)| *section == ClientSettingsSection::Navigation)
        .unwrap()
        .0;
    assert_client_only(&click(state, tab));
}

fn choice_rect(state: &ClientShellState, index: usize) -> Rect {
    state
        .hits
        .settings_choices
        .iter()
        .find(|(_, choice)| *choice == index)
        .unwrap()
        .0
}

fn choice_text(state: &ClientShellState, frame: &FrameData, index: usize) -> String {
    let rect = choice_rect(state, index);
    assert!(rect.bottom() <= frame.height);
    let start = usize::from(rect.y) * usize::from(frame.width) + usize::from(rect.x);
    frame.cells[start..start + usize::from(rect.width)]
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect()
}

fn assert_current_choices(state: &ClientShellState, frame: &FrameData, scope: usize, mouse: usize) {
    for (index, label) in CHOICES.iter().enumerate() {
        let text = choice_text(state, frame, index);
        assert!(text.contains(label), "{text:?}");
        assert_eq!(
            text.contains('✓'),
            index == scope || index == mouse,
            "{text:?}"
        );
    }
}

#[test]
fn short_navigation_settings_keep_every_selected_choice_visible() {
    let mut state = state_with_visits(&Config::default());
    open_navigation(&mut state);
    for (index, label) in CHOICES.iter().enumerate() {
        let frame = state.compose(106, 18).unwrap();
        assert_eq!(state.hits.settings_choices.len(), 4);
        let text = choice_text(&state, &frame, index);
        assert!(text.contains(&format!("▸ {label}")), "{text:?}");
        assert_current_choices(&state, &frame, 0, 2);
        if index + 1 < CHOICES.len() {
            assert_client_only(&state.handle_input_bytes(b"j"));
        }
    }
}

#[test]
fn navigation_settings_mouse_and_keyboard_apply_persist_without_endpoint_reload() {
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let temporary = TemporaryConfig::new("[ui]\ncopy_on_select = false\n");
    let mut state = state_with_visits(&temporary.load());
    open_navigation(&mut state);
    let frame = state.compose(106, 18).unwrap();
    assert_current_choices(&state, &frame, 0, 2);

    let choice = choice_rect(&state, 1);
    assert_client_only(&click(&mut state, choice));
    let frame = state.compose(106, 18).unwrap();
    assert!(choice_text(&state, &frame, 1).contains("▸ current space only"));
    assert_current_choices(&state, &frame, 0, 2);
    let apply = state.hits.overlay_primary;
    assert_client_only(&click(&mut state, apply));
    assert_eq!(
        temporary.load().ui.navigation_history_scope,
        NavigationHistoryScope::CurrentSpace
    );
    assert_eq!(
        state.config.navigation_history_scope,
        NavigationHistoryScope::CurrentSpace
    );
    assert!(!temporary.load().ui.copy_on_select);
    assert!(!state.config.copy_on_select);
    let frame = state.compose(106, 18).unwrap();
    assert_current_choices(&state, &frame, 1, 2);

    assert_client_only(&state.handle_input_bytes(b"j"));
    assert_client_only(&state.handle_input_bytes(b"j"));
    let frame = state.compose(106, 18).unwrap();
    assert!(choice_text(&state, &frame, 3).contains("▸ mouse Back/Forward disabled"));
    assert_client_only(&state.handle_input_bytes(b"\r"));
    let saved = temporary.load();
    assert_eq!(
        saved.ui.navigation_history_scope,
        NavigationHistoryScope::CurrentSpace
    );
    assert!(!saved.ui.mouse_history_navigation);
    assert!(!state.config.mouse_history_navigation);
    let frame = state.compose(106, 18).unwrap();
    assert_current_choices(&state, &frame, 1, 3);

    assert_client_only(&state.handle_input_bytes(b"\x1b"));
    assert!(state.overlay.is_none());
    assert_client_only(&state.handle_input_bytes(b"\x1b[<128;5;3M"));
    assert_client_only(&state.handle_input_bytes(b"\x02"));
    let back = state.handle_input_bytes(b"\x1b[D");
    assert!(back.requests.is_empty());
    assert!(matches!(
        back.actions.as_slice(),
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(&request.method, crate::api::schema::Method::TabFocus(target) if target.tab_id == "tab_1")
    ));
}
