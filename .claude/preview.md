# Preview a Herdr PR on ub2

1. Build in the PR worktree: `cargo build --release`. Confirm `target/release/herdr --version` includes `+fork.<head sha12>`.
2. **Approve:** get Matthias's per-change approval before upgrading; this restarts the running server. Stop until approved.
3. Upgrade only with `herdr-session-rescue upgrade --binary target/release/herdr`.
4. Verify `herdr status server` reports the new `+fork.<head sha12>` version, then run `sha256sum target/release/herdr ~/.local/bin/herdr` and confirm the hashes match.
5. Open the preview for the human: `open-url "$(herdr-link "$HERDR_PANE_ID")"`. Never use `wezterm cli`.

## Rollback

Upgrade with `herdr-session-rescue upgrade --binary <previous known-good artifact>`, or restore a listed snapshot with `herdr-session-rescue list` followed by `herdr-session-rescue restore <snapshot>`.
