// Shared with client_commands.rs so the wire method list remains checked even
// while that optional transport module is not part of this branch's build.
#[allow(dead_code)]
pub(crate) const CLIENT_SHELL_METHODS: &[&str] = &[
    "agent.search",
    "client_shell.surface.set",
    "command.invoke",
    "integration.install",
    "integration.list",
    "layout.set_split_ratio",
    "pane.clear",
    "pane.close",
    "pane.copy_motion",
    "pane.copy_search",
    "pane.edit_scrollback",
    "pane.focus",
    "pane.focus_direction",
    "pane.input.set",
    "pane.link.activate",
    "pane.link.resolve",
    "pane.rename",
    "pane.resize",
    "pane.scroll",
    "pane.selection.read",
    "pane.split",
    "pane.swap",
    "pane.zoom",
    "product_announcement.dismiss",
    "release_notes.dismiss",
    "server.reload_config",
    "tab.close",
    "tab.create",
    "tab.focus",
    "tab.move",
    "tab.rename",
    "workspace.close",
    "workspace.create",
    "workspace.focus",
    "workspace.move",
    "workspace.move_block",
    "workspace.rename",
    "worktree.create",
    "worktree.list",
    "worktree.open",
    "worktree.remove",
];

#[cfg(test)]
mod tests {
    use super::CLIENT_SHELL_METHODS;

    #[test]
    fn client_shell_method_names_are_sorted_unique() {
        assert!(CLIENT_SHELL_METHODS
            .windows(2)
            .all(|pair| pair[0] < pair[1]));
    }
}
