use super::*;
use crate::app::state::Palette;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

#[test]
fn test_topology_diagram_orchestrator_worker() {
    let pat = get_builtin_pattern("orchestrator-worker").expect("template exists");
    let wide = generate_topology_diagram(&pat, 80);
    assert!(!wide.is_empty());
    assert!(wide
        .iter()
        .any(|l| l.contains("[Orquestador]") || l.contains("brain")));
    assert!(wide
        .iter()
        .any(|l| l.contains("Worker") || l.contains("coder")));
    assert!(wide.iter().any(|l| l.contains("orders")));

    let narrow = generate_topology_diagram(&pat, 40);
    assert!(!narrow.is_empty());
    for line in &narrow {
        assert!(
            line.chars().count() <= 40,
            "line exceeds narrow width: {}",
            line
        );
    }
}

#[test]
fn test_topology_diagram_hierarchical() {
    let pat = get_builtin_pattern("hierarchical").expect("template exists");
    let wide = generate_topology_diagram(&pat, 80);
    assert!(wide
        .iter()
        .any(|l| l.contains("Director") || l.contains("Nivel 0")));
    assert!(wide
        .iter()
        .any(|l| l.contains("Managers") || l.contains("Nivel 1")));

    let narrow = generate_topology_diagram(&pat, 40);
    for line in &narrow {
        assert!(line.chars().count() <= 40);
    }
}

#[test]
fn test_topology_diagram_blackboard() {
    let pat = get_builtin_pattern("blackboard").expect("template exists");
    let wide = generate_topology_diagram(&pat, 80);
    assert!(wide
        .iter()
        .any(|l| l.contains("PIZARRA") || l.contains("blackboard")));

    let narrow = generate_topology_diagram(&pat, 40);
    for line in &narrow {
        assert!(line.chars().count() <= 40);
    }
}

#[test]
fn test_topology_diagram_market() {
    let pat = get_builtin_pattern("market").expect("template exists");
    let wide = generate_topology_diagram(&pat, 80);
    assert!(wide
        .iter()
        .any(|l| l.contains("Requesters") || l.contains("ask")));
    assert!(wide
        .iter()
        .any(|l| l.contains("Market-Maker") || l.contains("bid")));

    let narrow = generate_topology_diagram(&pat, 40);
    for line in &narrow {
        assert!(line.chars().count() <= 40);
    }
}

#[test]
fn test_patterns_section_pure_render_and_gallery() {
    let mut state = ShepherdPatternsSectionState::default();
    let palette = Palette::catppuccin();
    let area = Rect::new(0, 0, 100, 30);
    let mut buffer = Buffer::empty(area);

    let hits = render_patterns_section(&mut buffer, area, &state, &palette);
    assert!(
        !hits.list_hits.rows.is_empty(),
        "gallery should display patterns"
    );
    assert!(
        hits.detail_hits.is_some(),
        "selected pattern detail should render"
    );

    // Filter switching
    state.filter_index = 1; // Project
    assert_eq!(state.filtered_indices().len(), 0);
    state.filter_index = 3; // Builtin
    assert_eq!(state.filtered_indices().len(), 4);
}

#[test]
fn test_pattern_editor_live_validation_and_guards() {
    let pat = get_builtin_pattern("orchestrator-worker").expect("template exists");
    let mut editor = PatternEditorState::from_pattern(&pat, PatternSource::Project);
    assert!(editor.is_valid(), "builtin pattern must start valid");

    // Provoke an error: empty name
    editor.name = String::new();
    editor.revalidate();
    assert!(!editor.is_valid(), "empty pattern name must be invalid");

    // Restore name
    editor.name = "mi-patron".into();
    editor.revalidate();
    assert!(editor.is_valid());

    // Provoke topology error: remove all workers
    editor.agents.retain(|a| a.role == "brain");
    editor.revalidate();
    assert!(
        !editor.is_valid(),
        "orchestrator without workers must be invalid"
    );

    // Add back worker
    editor.add_agent();
    let w = editor.agents.last_mut().unwrap();
    w.role = "coder".into();
    w.group = Some("worker-pool".into());
    w.consumes = vec!["orders".into()];
    w.publishes = vec!["results".into()];
    editor.revalidate();
    assert!(editor.is_valid(), "valid topology restored");
}

#[test]
fn test_patterns_section_state_machine_and_sub_actions() {
    let mut state = ShepherdPatternsSectionState::default();

    // 1. New from template
    state.start_new_from_template();
    assert!(state.has_sub_action());
    assert!(state.cancel_sub_action());
    assert!(!state.has_sub_action());

    // 2. Duplicate
    state.start_duplicate();
    assert!(state.has_sub_action());
    assert!(state.cancel_sub_action());

    // 3. Delete built-in is blocked
    state.start_delete();
    assert!(!state.has_sub_action(), "cannot delete builtin");
    assert!(state.error.is_some());

    // 4. Edit
    state.start_edit();
    assert!(state.has_sub_action());
    let (handled, _) = state.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(handled);
    assert!(!state.has_sub_action(), "Esc cancels edit");
}

#[test]
fn test_patterns_section_80x24_dimensions_no_panic() {
    let state = ShepherdPatternsSectionState::default();
    let palette = Palette::catppuccin();
    let area = Rect::new(0, 0, 80, 24);
    let mut buffer = Buffer::empty(area);

    let hits = render_patterns_section(&mut buffer, area, &state, &palette);
    assert!(!hits.list_hits.rows.is_empty());
}

#[test]
fn test_pattern_editor_field_focus_and_navigation() {
    let pat = get_builtin_pattern("orchestrator-worker").expect("template exists");
    let mut editor = PatternEditorState::from_pattern(&pat, PatternSource::Project);

    // Section 0: Metadata
    assert_eq!(editor.active_section, EditorSection::Metadata);
    assert_eq!(editor.active_field, 0);
    editor.cycle_forward();
    assert_eq!(editor.active_field, 1);
    editor.toggle_active_choice(); // cycles kind
    assert_eq!(editor.kind, PatternKind::Hierarchical);
    editor.cycle_forward();
    assert_eq!(editor.active_field, 2);
    editor.toggle_active_choice(); // toggles global
    assert!(editor.is_global);
    editor.cycle_forward();
    assert_eq!(editor.active_field, 3);
    editor.cycle_forward();
    assert_eq!(editor.active_field, 0);
    editor.cycle_backward();
    assert_eq!(editor.active_field, 3);

    // Text typing in description
    editor.insert_char('!');
    assert!(editor.description.ends_with('!'));
    editor.delete_char();
    assert!(!editor.description.ends_with('!'));

    // Switch to Agents section
    editor.active_section = EditorSection::Agents;
    editor.active_field = 0;
    assert_eq!(editor.field_count_for_section(), 11); // F12: +start
    editor.cycle_forward();
    assert_eq!(editor.active_field, 1);
}

#[test]
fn test_pattern_editor_agent_add_remove_and_reorder() {
    let pat = get_builtin_pattern("blackboard").expect("template exists");
    let mut editor = PatternEditorState::from_pattern(&pat, PatternSource::Project);
    let initial_count = editor.agents.len();
    assert_eq!(initial_count, 3);

    // Add agent
    editor.add_agent();
    assert_eq!(editor.agents.len(), initial_count + 1);
    assert_eq!(editor.selected_agent, initial_count);

    // Move up
    editor.move_agent_up();
    assert_eq!(editor.selected_agent, initial_count - 1);

    // Move down
    editor.move_agent_down();
    assert_eq!(editor.selected_agent, initial_count);

    // Remove agent
    editor.remove_agent();
    assert_eq!(editor.agents.len(), initial_count);

    // Cannot remove last agent if only 1 remains
    editor.agents.truncate(1);
    editor.selected_agent = 0;
    editor.remove_agent();
    assert_eq!(editor.agents.len(), 1, "cannot delete sole remaining agent");
}

#[test]
fn test_pattern_editor_start_checkbox_toggles_and_is_saved() {
    let pat = get_builtin_pattern("market").expect("template exists");
    let mut editor = PatternEditorState::from_pattern(&pat, PatternSource::Project);
    editor.active_section = EditorSection::Agents;
    editor.selected_agent = 0; // market-maker: not a starter
    editor.active_field = 10;
    assert_eq!(editor.field_count_for_section(), 11);
    assert!(!editor.agents[0].start);
    editor.toggle_active_choice();
    assert!(editor.agents[0].start);
    editor.toggle_active_choice();
    assert!(!editor.agents[0].start);
    // The requester template agent keeps its explicit `start: true` through the editor.
    assert!(editor.agents.iter().any(|a| a.id == "requester" && a.start));
}

#[test]
fn test_pattern_editor_topic_add_and_remove() {
    let pat = get_builtin_pattern("orchestrator-worker").expect("template exists");
    let mut editor = PatternEditorState::from_pattern(&pat, PatternSource::Project);
    let init_topics = editor.topics.len();

    editor.add_topic();
    assert_eq!(editor.topics.len(), init_topics + 1);
    assert_eq!(editor.selected_topic, init_topics);

    editor.remove_topic();
    assert_eq!(editor.topics.len(), init_topics);

    editor.topics.truncate(1);
    editor.selected_topic = 0;
    editor.remove_topic();
    assert_eq!(editor.topics.len(), 1, "cannot delete sole remaining topic");
}

#[test]
fn test_patterns_section_hit_testing_mouse_clicks() {
    let mut state = ShepherdPatternsSectionState::default();
    let palette = Palette::catppuccin();
    let area = Rect::new(0, 0, 100, 30);
    let mut buffer = Buffer::empty(area);

    let hits = render_patterns_section(&mut buffer, area, &state, &palette);

    // Click on second row
    if let Some(&(row_rect, _)) = hits.list_hits.rows.get(1) {
        let (handled, _) = state.handle_mouse(row_rect.x + 1, row_rect.y, &hits);
        assert!(handled);
        assert_eq!(state.selected, 1);
    }

    // Click on filter tab
    if let Some(&(tab_rect, tab_idx)) = hits.list_hits.filter_tabs.get(2) {
        let (handled, _) = state.handle_mouse(tab_rect.x + 1, tab_rect.y, &hits);
        assert!(handled);
        assert_eq!(state.filter_index, tab_idx);
    }

    // Start edit and test editor hits
    state.start_edit();
    let mut buffer2 = Buffer::empty(area);
    let edit_hits = render_patterns_section(&mut buffer2, area, &state, &palette);
    assert!(edit_hits.editor_hits.is_some());

    let eh = edit_hits.editor_hits.as_ref().unwrap();
    // Click on cancel button
    if let Some(cancel_rect) = eh.cancel_button {
        let (handled, _) = state.handle_mouse(cancel_rect.x + 1, cancel_rect.y, &edit_hits);
        assert!(handled);
        assert!(!state.has_sub_action(), "cancel button closes editor");
    }
}

#[test]
fn test_pattern_operations_with_isolated_tempdirs() {
    use shepherd_core::patterns::{delete_pattern, load_pattern, save_pattern};

    let temp_base = std::env::temp_dir().join(format!(
        "shepherd-patterns-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let proj_root = temp_base.join("repo");
    let glob_dir = temp_base.join("config");
    std::fs::create_dir_all(&proj_root).unwrap();
    std::fs::create_dir_all(&glob_dir).unwrap();

    // 1. Create a custom pattern
    let mut pat = get_builtin_pattern("orchestrator-worker").unwrap();
    pat.name = "custom-flow".into();
    pat.description = "Custom orchestrator with modified worker vendor".into();
    if let Some(worker) = pat.agents.get_mut(1) {
        worker.vendor = Some("codex".into());
        worker.prompt = Some("Custom persona for worker".into());
    }

    // 2. Save pattern to project
    let path =
        save_pattern(&pat, false, &proj_root, Some(&glob_dir)).expect("save pattern succeeds");
    assert!(path.is_file());

    // 3. Load pattern and verify customized agent fields
    let (loaded, src, loaded_path) = load_pattern("custom-flow", &proj_root, Some(&glob_dir))
        .expect("load saved pattern succeeds");
    assert_eq!(src, PatternSource::Project);
    assert_eq!(loaded_path.unwrap(), path);
    assert_eq!(loaded.name, "custom-flow");
    assert_eq!(loaded.agents[1].vendor.as_deref(), Some("codex"));
    assert_eq!(
        loaded.agents[1].prompt.as_deref(),
        Some("Custom persona for worker")
    );

    // 4. Delete pattern from project
    let deleted_src = delete_pattern("custom-flow", &proj_root, Some(&glob_dir))
        .expect("delete pattern succeeds");
    assert_eq!(deleted_src, PatternSource::Project);
    assert!(!path.exists(), "pattern file must be deleted");

    // 5. Pattern no longer exists
    assert!(load_pattern("custom-flow", &proj_root, Some(&glob_dir)).is_err());

    let _ = std::fs::remove_dir_all(&temp_base);
}

#[test]
fn test_pattern_run_execution_flow_via_key_and_mouse() {
    let mut state = ShepherdPatternsSectionState::default();
    let pat = get_builtin_pattern("orchestrator-worker").expect("template exists");
    state.update_detail(PatternDetail {
        pattern: pat,
        source: PatternSource::Builtin,
        path: None,
        problems: vec![],
    });
    state.patterns = vec![PatternSummary {
        name: "orchestrator-worker".into(),
        description: "OW".into(),
        kind: PatternKind::OrchestratorWorker,
        source: PatternSource::Builtin,
        agents_count: 4,
        topics_count: 2,
        shadowed: false,
        path: None,
    }];

    // Press 'r' to start execution form
    let (handled, act) = state.handle_key(&crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('r'),
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(handled);
    assert!(act.is_none());
    assert!(matches!(state.sub_action, Some(PatternSubAction::Run(_))));

    // Type task description
    for c in "Implement F8d".chars() {
        state.handle_key(&crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        ));
    }

    // Tab to worktree field
    state.handle_key(&crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::NONE,
    ));

    // Type worktree path
    for c in "/tmp/wt-f8d".chars() {
        state.handle_key(&crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        ));
    }

    // Render pure section and verify run_form_hits are populated
    let area = Rect::new(0, 0, 100, 30);
    let mut buffer = Buffer::empty(area);
    let palette = Palette::catppuccin();
    let hits = render_patterns_section(&mut buffer, area, &state, &palette);
    assert!(hits.run_form_hits.is_some());

    // Press Enter to submit execution
    let (handled_submit, act_submit) = state.handle_key(&crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(handled_submit);
    assert_eq!(
        act_submit,
        Some(ShepherdPatternsAction::RunPattern {
            pattern: "orchestrator-worker".into(),
            task: Some("Implement F8d".into()),
            worktree: Some("/tmp/wt-f8d".into()),
        })
    );
    assert!(state.sub_action.is_none());
}
