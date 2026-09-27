use super::App;

impl App {
    /// The single config-edit path: read, rewrite only what `update` touches,
    /// write atomically. Every setting the UI saves goes through it, so no
    /// surface can invent its own partial rewrite of the user's file.
    pub(super) fn update_config_file<F>(&mut self, error_context: &str, update: F) -> bool
    where
        F: FnOnce(&str) -> String,
    {
        #[cfg(test)]
        if std::env::var_os(crate::config::CONFIG_PATH_ENV_VAR).is_none() {
            return false;
        }

        let path = crate::config::config_path();
        if let Err(error) = crate::config::update_file_at(&path, error_context, update) {
            crate::logging::config_write_failed(&path, error_context, &error);
            self.state.config_diagnostic = Some(error);
            self.config_diagnostic_deadline =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
            return false;
        }

        true
    }

    /// Persist one UI edit and reload, so the change is live in the same frame
    /// the operator made it.
    pub(super) fn save_config_edit(&mut self, edit: crate::app::settings_general::ConfigEdit) {
        use crate::app::settings_general::ConfigEdit;

        let saved = match edit {
            ConfigEdit::Notifications {
                delivery,
                sound_enabled,
            } => self.update_config_file("notifications", |content| {
                let value = match delivery {
                    crate::config::ToastDelivery::Off => "\"off\"",
                    crate::config::ToastDelivery::Herdr => "\"herdr\"",
                    crate::config::ToastDelivery::Terminal => "\"terminal\"",
                    crate::config::ToastDelivery::System => "\"system\"",
                };
                let content =
                    crate::config::upsert_section_value(content, "ui.toast", "delivery", value);
                let content = crate::config::remove_section_key(&content, "ui.toast", "enabled");
                crate::config::upsert_section_bool(&content, "ui.sound", "enabled", sound_enabled)
            }),
            ConfigEdit::Bool {
                section,
                key,
                value,
            } => self.update_config_file(key, |content| {
                crate::config::upsert_section_bool(content, section, key, value)
            }),
            ConfigEdit::Integer {
                section,
                key,
                value,
            } => self.update_config_file(key, |content| {
                crate::config::upsert_section_value(content, section, key, &value.to_string())
            }),
            ConfigEdit::Text {
                section,
                key,
                value,
            } => self.update_config_file(key, |content| {
                crate::config::upsert_section_value(content, section, key, &format!("\"{value}\""))
            }),
        };
        if saved {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn toggle_notifications(&mut self) {
        let enabled = self.state.notifications_enabled();
        let next_enabled = !enabled;
        if !self.state.local_sound_playback {
            self.state.request_client_notification_config = Some(next_enabled);
        }
        self.save_config_edit(crate::app::settings_general::ConfigEdit::Notifications {
            delivery: if enabled {
                crate::config::ToastDelivery::Off
            } else {
                self.state.last_non_off_toast_delivery
            },
            sound_enabled: next_enabled,
        });
        if self.state.notifications_enabled() != next_enabled {
            self.state.request_client_notification_config = None;
        }
    }

    pub(super) fn toggle_sidebar_area(&mut self, area: crate::config::SidebarArea) {
        let value = !self.state.sidebar_areas.is_visible(area);
        self.save_config_edit(crate::app::settings_general::ConfigEdit::Bool {
            section: "ui.sidebar.areas",
            key: area.config_key(),
            value,
        });
        if area == crate::config::SidebarArea::Notes
            && self.state.sidebar_sections_layout
            && !self.state.sidebar_areas.is_visible(area)
        {
            self.state.set_notepad_focus(false);
        }
    }

    /// Persist one captured keybinding and reload.
    ///
    /// Built-ins go through the same single config-edit path every other
    /// settings row uses; user actions go through 6b's atomic `[[actions]]`
    /// rewrite, because their key lives inside their own table.
    pub(super) fn save_keybinding(
        &mut self,
        target: &crate::app::settings_keybindings::KeybindTarget,
        key: &str,
    ) -> Result<(), String> {
        use crate::app::settings_keybindings::KeybindTarget;

        match target {
            KeybindTarget::BuiltIn { field } => {
                let field = *field;
                let value = format!("\"{key}\"");
                if !self.update_config_file(field, |content| {
                    crate::config::upsert_section_value(content, "keys", field, &value)
                }) {
                    return Err(format!("failed to save {field}"));
                }
            }
            KeybindTarget::UserAction { name } => {
                let path = crate::config::config_path();
                let mut config = match std::fs::read_to_string(&path) {
                    Ok(content) => toml::from_str::<crate::config::Config>(&content)
                        .map_err(|error| format!("Config parse failed: {error}"))?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        crate::config::Config::default()
                    }
                    Err(error) => return Err(format!("Config read failed: {error}")),
                };
                let Some(action) = config
                    .actions
                    .iter_mut()
                    .find(|action| action.name.trim().eq_ignore_ascii_case(name.trim()))
                else {
                    return Err(format!("action {name:?} is no longer in the config"));
                };
                action.key = Some(key.to_string());
                crate::config::write_actions_atomically(&path, &config.actions)
                    .map_err(|error| format!("Action save failed: {error}"))?;
            }
        }

        let report = self.apply_config_from_disk(false);
        if report.status == crate::config::ConfigReloadStatus::Failed {
            return Err(report
                .diagnostics
                .first()
                .cloned()
                .unwrap_or_else(|| "Config reload failed".into()));
        }
        Ok(())
    }

    pub(super) fn mark_onboarding_complete(&mut self) {
        self.update_config_file("onboarding setting", |content| {
            crate::config::upsert_top_level_bool(content, "onboarding", false)
        });
    }

    pub(super) fn save_theme(&mut self, name: &str) {
        if self.update_config_file("theme", |content| {
            let content = crate::config::upsert_section_value(
                content,
                "theme",
                "name",
                &format!("\"{name}\""),
            );
            crate::config::upsert_section_bool(&content, "theme", "auto_switch", false)
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_status_indicators(&mut self, style: crate::config::StatusIndicatorStyle) {
        if self.update_config_file("status indicators", |content| {
            crate::config::upsert_section_value(
                content,
                "ui",
                "status_indicators",
                &format!("\"{}\"", style.as_str()),
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_sound(&mut self, enabled: bool) {
        if self.update_config_file("sound setting", |content| {
            crate::config::upsert_section_bool(content, "ui.sound", "enabled", enabled)
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_toast_delivery(&mut self, delivery: crate::config::ToastDelivery) {
        let value = match delivery {
            crate::config::ToastDelivery::Off => "\"off\"",
            crate::config::ToastDelivery::Herdr => "\"herdr\"",
            crate::config::ToastDelivery::Terminal => "\"terminal\"",
            crate::config::ToastDelivery::System => "\"system\"",
        };
        if self.update_config_file("toast setting", |content| {
            let content =
                crate::config::upsert_section_value(content, "ui.toast", "delivery", value);
            crate::config::remove_section_key(&content, "ui.toast", "enabled")
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_agent_border_labels(&mut self, enabled: bool) {
        if self.update_config_file("agent border labels", |content| {
            crate::config::upsert_section_bool(
                content,
                "ui",
                "show_agent_labels_on_pane_borders",
                enabled,
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_agent_panel_sort(&mut self, sort: crate::app::state::AgentPanelSort) {
        let value = match sort {
            crate::app::state::AgentPanelSort::Spaces => {
                crate::config::AgentPanelSortConfig::Spaces.as_str()
            }
            crate::app::state::AgentPanelSort::Priority => {
                crate::config::AgentPanelSortConfig::Priority.as_str()
            }
        };
        if self.update_config_file("agent panel sort", |content| {
            crate::config::upsert_section_value(
                content,
                "ui",
                "agent_panel_sort",
                &format!("\"{value}\""),
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::App;

    #[cfg(unix)]
    fn scratch_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-config-io-{}",
            crate::config::test_unique_suffix()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[cfg(unix)]
    #[test]
    fn notification_toggle_restores_last_delivery_through_a_symlink() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let dir = scratch_dir();
        let target = dir.join("generated.toml");
        let link = dir.join("config.toml");
        std::fs::write(
            &target,
            "[ui.toast]\ndelivery = \"system\"\n[ui.sound]\nenabled = true\n",
        )
        .expect("seed");
        std::os::unix::fs::symlink("generated.toml", &link).expect("symlink");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &link);
        let config = crate::config::Config::load().config;
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );

        app.toggle_notifications();
        assert_eq!(
            app.state.toast_config.delivery,
            crate::config::ToastDelivery::Off
        );
        assert!(!app.state.sound.enabled);

        app.toggle_notifications();
        assert_eq!(
            app.state.toast_config.delivery,
            crate::config::ToastDelivery::System
        );
        assert!(app.state.sound.enabled);
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("link metadata")
                .file_type()
                .is_symlink(),
            "toggle replaced the managed symlink"
        );
        let saved: crate::config::Config =
            toml::from_str(&std::fs::read_to_string(&target).expect("read managed target"))
                .expect("saved config parses");
        assert_eq!(
            saved.ui.toast.delivery,
            crate::config::ToastDelivery::System
        );
        assert!(saved.ui.sound.enabled);

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn sections_sidebar_area_toggle_hides_notes_and_round_trips_config() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let dir = scratch_dir();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[ui.sidebar]\nlayout = \"sections\"\n[ui.sidebar.areas]\nnotes = true\n[notepad]\nenabled = true\nheight = 4\n",
        )
        .expect("seed config");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);
        let config = crate::config::Config::load().config;
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.hyperspace.enabled = false;
        app.state.notepad.focused = true;
        let sidebar = ratatui::layout::Rect::new(0, 0, 36, 24);
        assert!(crate::ui::sidebar::sidebar_notepad_rect(&app.state, sidebar).height > 0);

        app.toggle_sidebar_area(crate::config::SidebarArea::Notes);

        assert_eq!(
            crate::ui::sidebar::sidebar_notepad_rect(&app.state, sidebar),
            ratatui::layout::Rect::default(),
            "a hidden notes area must reserve no panel rows"
        );
        assert!(
            !app.state.notepad.focused,
            "hiding Notes releases editor focus"
        );
        assert!(
            app.state.notepad_request.is_some(),
            "releasing focus queues the note save"
        );
        let saved = crate::config::Config::load().config;
        assert!(!saved.ui.sidebar.areas.notes);
        let reloaded = App::new(
            &saved,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        assert!(!reloaded.state.sidebar_areas.notes);
        assert_eq!(
            crate::ui::sidebar::sidebar_notepad_rect(&reloaded.state, sidebar),
            ratatui::layout::Rect::default()
        );

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn current_sidebar_layout_ignores_sections_area_config() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let dir = scratch_dir();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[ui.sidebar]\nlayout = \"current\"\n[ui.sidebar.areas]\nnotes = false\n[notepad]\nenabled = true\nheight = 4\n",
        )
        .expect("seed config");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);
        let config = crate::config::Config::load().config;
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.hyperspace.enabled = false;
        assert!(!app.state.sidebar_areas.notes);
        assert!(crate::ui::sidebar::sidebar_area_is_visible(
            &app.state,
            crate::config::SidebarArea::Notes
        ));
        assert!(
            crate::ui::sidebar::sidebar_notepad_rect(
                &app.state,
                ratatui::layout::Rect::new(0, 0, 36, 24)
            )
            .height
                > 0
        );

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn config_edit_rewrites_only_the_touched_key() {
        let original = "# a comment kept verbatim\n[ui]\nsidebar_width = 30\nconfirm_close = true\n\n[session]\nsettle_after_days = 7\n";
        let updated = crate::config::upsert_section_bool(original, "ui", "confirm_close", false);

        assert_eq!(
            updated,
            "# a comment kept verbatim\n[ui]\nsidebar_width = 30\nconfirm_close = false\n\n[session]\nsettle_after_days = 7\n"
        );
    }
}

#[cfg(test)]
mod keybinding_capture_tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::{
        settings_keybindings::{settings_keybinding_rows, KeybindTarget},
        state::SettingsSection,
        App,
    };

    struct Scratch {
        env: crate::config::TestConfigEnvGuard,
        directory: std::path::PathBuf,
        path: std::path::PathBuf,
    }

    impl Scratch {
        fn new(seed: &str) -> Self {
            let mut env = crate::config::TestConfigEnvGuard::acquire();
            let directory = std::env::temp_dir().join(format!(
                "herdr-keybind-capture-{}",
                crate::config::test_unique_suffix()
            ));
            std::fs::create_dir_all(&directory).expect("temp config directory");
            let path = directory.join("config.toml");
            std::fs::write(&path, seed).expect("seed config");
            env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);
            Self {
                env,
                directory,
                path,
            }
        }

        fn app(&self) -> App {
            App::new(
                &crate::config::Config::load().config,
                true,
                None,
                tokio::sync::mpsc::unbounded_channel().1,
                crate::api::EventHub::default(),
            )
        }

        fn read(&self) -> String {
            std::fs::read_to_string(&self.path).expect("config")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            self.env.remove(crate::config::CONFIG_PATH_ENV_VAR);
            std::fs::remove_dir_all(&self.directory).ok();
        }
    }

    fn select_row(app: &mut App, matches: impl Fn(&KeybindTarget) -> bool) -> usize {
        let row = settings_keybinding_rows(&app.state)
            .into_iter()
            .position(|row| row.target.as_ref().is_some_and(&matches))
            .expect("row exists");
        app.state.settings.list.select(row);
        row
    }

    #[test]
    fn keybinding_capture_writes_a_free_chord_and_refuses_a_bound_one() {
        let scratch = Scratch::new("# keep this comment\n[ui]\nsidebar_width = 31\n");
        let mut app = scratch.app();
        crate::app::input::open_settings_at(&mut app.state, SettingsSection::Keybindings);
        let row = select_row(&mut app, |target| {
            *target == KeybindTarget::BuiltIn { field: "usage" }
        });

        // Enter arms the row rather than saving anything.
        app.handle_settings_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(
            app.state
                .settings
                .keybind_capture
                .as_ref()
                .map(|capture| capture.row),
            Some(row)
        );

        // A chord that navigate mode reserves is refused on the row.
        app.handle_settings_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::empty()));
        assert!(app
            .state
            .settings
            .keybind_capture
            .as_ref()
            .and_then(|capture| capture.error.as_deref())
            .is_some());
        assert_eq!(
            scratch.read(),
            "# keep this comment\n[ui]\nsidebar_width = 31\n"
        );

        // A free chord is written under [keys] and reloaded.
        app.handle_settings_key(KeyEvent::new(KeyCode::F(9), KeyModifiers::empty()));
        assert_eq!(app.state.settings.keybind_capture, None);
        let saved = scratch.read();
        assert!(saved.starts_with("# keep this comment\n"), "{saved}");
        assert!(saved.contains("[keys]"), "{saved}");
        assert!(saved.contains("usage = \"f9\""), "{saved}");
        assert_eq!(app.state.keybinds.usage.label().as_deref(), Some("f9"));
    }

    #[test]
    fn keybinding_capture_writes_a_user_action_key_into_its_actions_table() {
        let scratch = Scratch::new("[[actions]]\nname = \"tests\"\ncommand = \"just test\"\n");
        let mut app = scratch.app();
        crate::app::input::open_settings_at(&mut app.state, SettingsSection::Keybindings);
        assert_eq!(app.state.keybinds.user_actions.len(), 1);
        select_row(
            &mut app,
            |target| matches!(target, KeybindTarget::UserAction { name } if name == "tests"),
        );

        app.handle_settings_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        app.handle_settings_key(KeyEvent::new(KeyCode::F(9), KeyModifiers::empty()));

        assert_eq!(app.state.settings.keybind_capture, None);
        let saved = scratch.read();
        assert!(saved.contains("key = \"f9\""), "{saved}");
        assert!(!saved.contains("[keys]"), "{saved}");
        assert_eq!(
            app.state.keybinds.user_actions[0]
                .bindings
                .label()
                .as_deref(),
            Some("f9")
        );
    }

    #[test]
    fn esc_cancels_a_capture_without_writing() {
        let scratch = Scratch::new("[ui]\nsidebar_width = 31\n");
        let mut app = scratch.app();
        crate::app::input::open_settings_at(&mut app.state, SettingsSection::Keybindings);
        select_row(&mut app, |target| {
            *target == KeybindTarget::BuiltIn { field: "usage" }
        });

        app.handle_settings_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        app.handle_settings_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));

        assert_eq!(app.state.settings.keybind_capture, None);
        assert_eq!(app.state.server_mode(), crate::app::Mode::Settings);
        assert_eq!(scratch.read(), "[ui]\nsidebar_width = 31\n");
    }
}

#[cfg(test)]
mod general_round_trip_tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::{settings_general::GeneralRow, state::SettingsSection, App};

    /// Every General row must survive the full loop the operator sees: press
    /// enter, have the key written to the config file, reload the file, and
    /// read the new value back off live state.
    #[test]
    fn every_general_row_round_trips_through_a_temp_config_file() {
        for (index, row) in GeneralRow::ALL.iter().enumerate() {
            let mut env = crate::config::TestConfigEnvGuard::acquire();
            let directory = std::env::temp_dir().join(format!(
                "herdr-general-round-trip-{}",
                crate::config::test_unique_suffix()
            ));
            std::fs::create_dir_all(&directory).expect("temp config directory");
            let path = directory.join("config.toml");
            std::fs::write(&path, "# keep this comment\n").expect("seed config");
            env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

            let mut app = App::new(
                &crate::config::Config::load().config,
                true,
                None,
                tokio::sync::mpsc::unbounded_channel().1,
                crate::api::EventHub::default(),
            );
            crate::app::input::open_settings_at(&mut app.state, SettingsSection::General);
            app.state.settings.list.select(index);

            let (section, key) = row.config_key();
            let before = row.value(&app.state);
            let expected = crate::app::settings_general::cycle_general_row(&app.state, *row);
            app.handle_settings_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
            let saved = std::fs::read_to_string(&path).expect("saved config");

            match expected {
                None => {
                    // A row with no in-place edit must not write anything, and
                    // must still name a real key for the operator to edit.
                    assert_eq!(
                        saved, "# keep this comment\n",
                        "{key} wrote without an edit"
                    );
                    assert!(
                        !row.is_editable(),
                        "{key} is editable but cycles to nothing"
                    );
                }
                Some(_) => {
                    assert!(
                        saved.starts_with("# keep this comment\n"),
                        "{key} rewrote unrelated bytes: {saved}"
                    );
                    assert!(saved.contains(&format!("[{section}]")), "{key}: {saved}");
                    assert!(saved.contains(&format!("{key} = ")), "{key}: {saved}");
                    assert_ne!(
                        row.value(&app.state),
                        before,
                        "{section}.{key} did not take effect after the reload"
                    );

                    // Read the file back through a fresh load: the value the
                    // row shows must come from the file, not from the click.
                    let reloaded = crate::config::Config::load().config;
                    let mut fresh = App::new(
                        &reloaded,
                        true,
                        None,
                        tokio::sync::mpsc::unbounded_channel().1,
                        crate::api::EventHub::default(),
                    );
                    fresh.state.settings.list.select(index);
                    assert_eq!(
                        row.value(&fresh.state),
                        row.value(&app.state),
                        "{key} did not survive a reload from disk"
                    );
                }
            }

            env.remove(crate::config::CONFIG_PATH_ENV_VAR);
            std::fs::remove_dir_all(&directory).ok();
        }
    }
}
