mod alt_screen_read;
pub mod autodetect;
#[cfg(unix)]
pub(crate) mod client_accept;
pub(crate) mod client_commands;
pub(crate) mod client_shell;
pub(crate) mod client_shell_graphics;
pub(crate) mod client_transport;
pub(crate) mod clients;
pub(crate) mod clipboard_image;
#[cfg(unix)]
pub(crate) mod handoff;
pub mod headless;
pub(crate) mod keybindings;
pub(crate) mod notifications;
pub(crate) mod pane_input;
#[cfg(test)]
mod render_scale_benchmark;
pub(crate) mod render_stream;
pub mod socket_paths;
pub(crate) mod terminal_attach;
