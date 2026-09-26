use crate::app;

pub(crate) fn app_keybindings(app: &app::App) -> crate::config::LiveKeybindConfig {
    crate::config::LiveKeybindConfig {
        prefix: app.state.prefix_keys.clone(),
        keybinds: app.state.keybinds.clone(),
    }
}

pub(crate) fn apply_keybindings(
    app: &mut app::App,
    keybindings: &crate::config::LiveKeybindConfig,
) {
    app.state.prefix_keys = keybindings.prefix.clone();
    app.state.keybinds = keybindings.keybinds.clone();
}
