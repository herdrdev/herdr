use ratatui::layout::Rect;

pub(crate) const ONBOARDING_TITLE: &str = "  herdr";
pub(crate) const ONBOARDING_SUBTITLE: &str = "  terminal workspace manager for coding agents";
pub(crate) const ONBOARDING_DESCRIPTION: [&str; 3] = [
    "  this is a mouse-first terminal.",
    "  click the sidebar to switch workspaces, drag pane",
    "  borders to resize, right-click for context menus.",
];
pub(crate) const ONBOARDING_PREFIX_LABEL: &str = "ctrl+b";
pub(crate) const ONBOARDING_PREFIX_SUFFIX: &str = " enters prefix mode · ";
pub(crate) const ONBOARDING_HELP_LABEL: &str = "?";
pub(crate) const ONBOARDING_HELP_SUFFIX: &str = " shows keybinds and settings";
pub(crate) const ONBOARDING_NEXT: &str =
    "  next: install optional agent integrations for more reliable state";

pub(crate) fn onboarding_welcome_continue_rect(area: Rect) -> Rect {
    super::widgets::continue_button_rect(area)
}
