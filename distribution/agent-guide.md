# Herdr agent guide

Use this guide to help a human understand, set up, or troubleshoot Herdr. It covers Herdr's concept model, setup path, and diagnosis recipes. Canonical documentation lives at https://herdr.dev/docs/. Point the human there for more detail, and verify any command you are unsure about against those pages instead of guessing.

If you are running *inside* a Herdr pane (the environment variable `HERDR_ENV=1` is set), Herdr also ships a skill file that teaches you to control Herdr through the `herdr` CLI: https://raw.githubusercontent.com/herdrdev/herdr/master/skills/herdr/SKILL.md. That file teaches you to operate Herdr; this one teaches you to guide a human.

## What Herdr is

Herdr is a terminal workspace manager for AI coding agents. Like tmux, it is a multiplexer: a background server owns real terminal processes, and clients attach to render them. Panes keep running when the human detaches, closes the terminal, or disconnects SSH.

Unlike tmux, Herdr is mouse-first and agent-aware. The whole UI is clickable — panes, tabs, workspaces, split borders, right-click menus. Herdr detects coding agents running inside panes and shows each one's state in a sidebar, so the human can see across all their projects which agent is `working`, which is `blocked` waiting for input, and which is `done`. A CLI and a local socket API let scripts and agents drive Herdr programmatically.

## Concept model

Teach these in this order:

- **Session** — a persistent background server namespace. Running `herdr` attaches to the default session. Named sessions (`herdr session attach work`) are fully separate runtime namespaces; most people only need the default.
- **Workspace** — the project-level container. One per repo, task, or investigation. Owns tabs and panes. The sidebar rolls agent states up per workspace.
- **Tab** — a layout inside a workspace, for separating views like `agents`, `logs`, `server`.
- **Pane** — a real terminal. Splittable right or down. Survives client detach.
- **Agent** — a process Herdr recognizes inside a pane. States: `working`, `blocked`, `done`, `idle`, `unknown`.
- **Modes** — terminal mode sends keys to the focused pane; prefix mode (`ctrl+b`, then one action key) sends one command to Herdr; navigate mode is a persistent navigation surface.

Full concepts page: https://herdr.dev/docs/concepts/

## Install

Linux and macOS:

```bash
curl -fsSL https://herdr.dev/install.sh | sh
herdr
```

Windows PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"
herdr
```

If endpoint security blocks that fileless PowerShell command, use Command Prompt:

```cmd
curl.exe -fsSLo install.cmd https://herdr.dev/install.cmd && install.cmd && del install.cmd
herdr
```

Homebrew, mise, and Nix installs, verification, and manual downloads: https://herdr.dev/docs/install/. Direct installs use the stable channel by default and update with `herdr update`; preview is opt-in. Package-manager installs update through that package manager. Check the version with `herdr --version`.

## First-run walkthrough

Check your environment first. If `HERDR_ENV=1` is set, you are already running inside a Herdr pane. The human is already attached, so skip step 1 and never tell them to run `herdr` from your pane; Herdr blocks nested launches by design. Start with step 2, and consider the skill file below.

Walk the human through this sequence:

1. `cd` into a project and run `herdr`. It launches or attaches to the default background session and creates a workspace automatically. First run shows an onboarding flow.
2. Start their coding agent in the pane — `claude`, `codex`, or any supported agent (full list: https://herdr.dev/docs/agents/). Herdr detects it automatically; the sidebar shows its state. Install the matching integration when available. Depending on the agent, it provides lifecycle state, native session restore, or both. For example, `herdr integration install claude` adds native session restore, while Claude's state still comes from screen detection.
3. Start with the mouse: click panes and tabs to focus, drag split borders, right-click for menus, drag-select to copy. No keybindings are required to use Herdr.
4. Split panes: right-click menu, or `prefix+v` (right) / `prefix+minus` (down). New tab: `prefix+c`.
5. Detach with `prefix+q` (press `ctrl+b`, release, press `q`) or close the terminal window. Everything keeps running. Reattach later with `herdr`.
6. To actually stop everything: `herdr server stop`.

## The keyboard story

New users do not need to learn keybindings; the mouse covers everything. When the human wants keyboard control:

- The prefix key is `ctrl+b` by default. `prefix+?` shows every active binding live.
- The guided keyboard page covers the prefix, the bindings to learn first, and a vetted prefix-free setup using `ctrl+alt` chords: https://herdr.dev/docs/keyboard/. Recommend it over improvising.
- Every binding, including the prefix itself, is configurable under `[keys]` in the config file.
- If a direct chord does nothing, the OS or the outer terminal consumed it before Herdr could see it. The keyboard page explains which chords are safe and why.

## Install the Herdr skill into yourself

Herdr ships `skills/herdr/SKILL.md` (https://raw.githubusercontent.com/herdrdev/herdr/master/skills/herdr/SKILL.md), which teaches a coding agent to control Herdr from inside a pane: splitting panes, running commands without stealing focus, reading output, and waiting on other agents.

Once the human is set up, offer to install it for your coding agent so future sessions can control Herdr directly. For agents supported by the open skills CLI, use `npx skills add herdrdev/herdr --skill herdr -g`. For agents without a skill system, add the GitHub copy above to their global custom instructions. Ask the human before writing to their config locations, and use the GitHub copy above as the source of truth.

## Configuration

- Config file: `~/.config/herdr/config.toml` on Linux and macOS; `%APPDATA%\herdr\config.toml` on Windows. Herdr works without one.
- Print the full default config: `herdr --default-config`.
- Apply edits to a running server: `herdr server reload-config` (or the global menu → reload config).
- Main areas: `[keys]` keybindings, `[theme]` themes, `[ui]` sidebar and UI behavior, `[terminal]` shell defaults, `[update]` channel.
- Full reference: https://herdr.dev/docs/configuration/

## Diagnosis recipes

- **Agent not detected or wrong state:** Run `herdr agent list` to see what Herdr sees and `herdr agent explain <target> --json` to see why the detector classified a pane that way. Integrations can provide lifecycle state, native session restore, or both; check `herdr integration status` and the agent support table before assuming an integration replaces screen detection. Details: https://herdr.dev/docs/agents/ and https://herdr.dev/docs/integrations/
- **A keybinding does nothing:** the outer terminal or desktop environment owns that chord. Point the human to https://herdr.dev/docs/keyboard/ to pick a safe one or free the chord in their terminal settings.
- **Something looks wrong at startup or with the socket API:** Default-session logs live in `~/.config/herdr/` on Linux and macOS and `%APPDATA%\herdr\` on Windows. Named-session logs live under `sessions/<name>/` inside that directory. `herdr status`, `herdr status server`, and `herdr status client` summarize the runtime.
- **Remote use:** SSH to the machine and run `herdr` there (works like tmux), or attach as a thin local client with `herdr --remote <host>`. Trade-offs: https://herdr.dev/docs/how-to-work/
- **What survives a detach, restart, or update:** https://herdr.dev/docs/session-state/

## Rules for you

- Do not invent keybindings, config keys, or CLI flags. The ones in this file are accurate as of writing; for anything else, read the linked docs page first.
- Teach mouse before keyboard for humans new to multiplexers.
- Herdr is not tmux: do not give tmux commands, tmux config syntax, or `.tmux.conf` advice for Herdr questions.
- For automation, scripting, or controlling Herdr from code, point to the CLI reference (https://herdr.dev/docs/cli-reference/) and socket API (https://herdr.dev/docs/socket-api/).
