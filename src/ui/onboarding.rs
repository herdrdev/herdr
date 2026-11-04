use ratatui::layout::Rect;

pub(crate) const ONBOARDING_TITLE: &str = "  Pretorian";
pub(crate) const ONBOARDING_SUBTITLE: &str = "  ambiente de trabalho para agentes";
pub(crate) const ONBOARDING_DESCRIPTION: [&str; 3] = [
    "  use o mouse ou o teclado.",
    "  clique na barra lateral para trocar de espaço;",
    "  arraste as bordas e clique com o botão direito.",
];
pub(crate) const ONBOARDING_PREFIX_SUFFIX: &str = " abre atalhos · ";
pub(crate) const ONBOARDING_HELP_LABEL: &str = "?";
pub(crate) const ONBOARDING_HELP_SUFFIX: &str = " mostra atalhos e configurações";
pub(crate) const ONBOARDING_NEXT: &str = "  a seguir: configure as integrações opcionais";

pub(crate) fn onboarding_welcome_continue_rect(area: Rect) -> Rect {
    super::widgets::continue_button_rect(area)
}
