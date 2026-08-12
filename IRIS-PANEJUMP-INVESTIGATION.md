# Investigation: focused-pane viewport jump on background output (WSL2, d57cefb)

**herdr version:** `0.8.0-iris.20260805.d57cefb8`  
**Environment:** WSL2 (Linux 6.6.87.2 microsoft-standard-WSL2)  
**Filed by:** Iris platform team (tryiris-ai)  
**Status:** DRAFT — §2 mechanism refuted at d57cefb in core scroll/resize; alternate surfaces listed

---

## Symptom

Focused pane repeatedly jumps to the bottom while a background pane produces output. The "jump to
bottom" affordance fires continuously. Calmed by Ctrl-L / window resize / detach+reattach but
returns immediately under load. Focused pane's own output is static (verified via byte-count log).
Correlation: jumping is worst while a background pane runs a heavy build; single `herdr server`
process, five concurrent panes each with `tail -F` on its own log.

## §2 Hypothesis

herdr auto-scrolls the focused pane to the bottom on PTY output events originating in other panes
(or triggers a full layout recompute that yanks the focused viewport down). Confirmed by
correlation; mechanism refuted below.

## §2 verdict: REFUTED for core scroll/resize/render at d57cefb8

Code-analysis (read-only, zig absent so no live build) shows two independent guards neutralize the
mechanism in herdr Rust core:

**Guard 1 — size-guarded resize (pane.rs:2498-2505):** `PaneRuntime::resize` early-returns when
dimensions are unchanged. The every-frame resize (`headless.rs:4082-4088`) is a no-op unless a
pane's geometry actually changes. Background output doesn't change geometry.

**Guard 2 — invariant scrollbar gutter (panes.rs:34-45):** `stable_terminal_inner_rect` always
reserves the scrollbar column when `pane_scrollbars` is on — only the *drawing* is conditional.
So a pane's size fed to `resize()` changes only on a genuine layout event, never on another pane's
output.

**Offset preserved on resize (terminal.rs:1455-1507, 2553-2568):** Even when resize does fire, it
saves and restores `offset_from_bottom`.

**No output path calls scroll-to-bottom:** Every `scroll_reset()` caller is a user-input path
(keystrokes, mouse, attach events) — `src/app/input/terminal.rs:191, 254`,
`src/app/input/mod.rs:530`, `src/server/headless.rs:369, 387, 407`. None fire on PTY output.

## Alternate candidate surfaces (not yet investigated — zig required to build)

1. **Multi-client foreground/observer size disagreement** in `render_and_stream`
   (`headless.rs:4036`) — foreground vs observer clients may report different terminal sizes; if
   so, the size guard passes and a real resize fires, potentially resetting the offset.
   **Leading hypothesis for live investigation.**
2. **WSL2 outer-terminal SIGWINCH propagation** — whether a resize signal from the WSL2 host
   terminal reaches all panes even when only the outer terminal changes size.
3. **Config interaction** — `ui.pane_scrollbars` + mouse wheel routing under multiple clients.

## Build requirements (Iris engineers: do NOT overwrite ~/.local/bin/herdr without explicit approval)

- Rust via `rustup`, channel `1.96.1` (`rust-toolchain.toml`)
- **Zig 0.15.2** — required by `vendor/libghostty-vt/build.zig.zon`
- `cargo build --release` → copy `target/release/herdr` to a **separate test path**

Repro once built: two panes — one running `yes` or `tail -F` on an appended scratch file, the
other scrolled up; observe whether the scrolled pane jumps while output fires in the other.
