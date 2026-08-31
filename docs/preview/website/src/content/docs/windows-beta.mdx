---
title: Windows support
description: Native Windows support, workflows, and known limitations.
---

Native Windows support is generally available.

Herdr on Windows uses ConPTY and Windows process/runtime behavior instead of the Unix PTY model Herdr was originally built around. Most core workflows are supported, but some capabilities differ from Linux and macOS or remain platform-dependent. Windows may receive more platform-specific fixes as those remaining gaps close.

Install Herdr natively on Windows with PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"
```

If endpoint security blocks that fileless PowerShell command, open Command Prompt and run:

```cmd
curl.exe -fsSLo install.cmd https://herdr.dev/install.cmd && install.cmd && del install.cmd
```

Windows builds are available through both stable and preview update channels. New installs use stable by default, and stable is recommended for normal use. Existing preview installs stay on preview until you run `herdr channel set stable`. If an older preview build rejects that command, run `herdr update` once on preview and retry. Preview provides newer, less-tested fixes from `master` and may regress; opt in with `herdr channel set preview` only when you want that tradeoff.

The installer stores releases under `%USERPROFILE%\.herdr\packages\standalone\releases`, points `%LOCALAPPDATA%\Programs\Herdr\bin` at the current release, and keeps a small number of older releases so running processes do not block updates.

For internal testing, `HERDR_MANIFEST_URL` can point the installer at a custom manifest instead of Herdr's stable or preview manifest. Set `HERDR_CHANNEL=preview` with a custom preview manifest.

## Supported on Windows

| Capability | Status |
| --- | --- |
| Local persistent sessions | supported |
| Native panes through ConPTY | supported |
| Windows Terminal / PowerShell app attach | supported |
| `herdr --remote` to Linux/macOS hosts | supported |
| Remote clipboard images and image-file drops | supported |
| `cmd.exe` panes | supported |
| Native keyboard and mouse input | supported |
| Startup cwd and workspace labels | supported |
| Pane launch cwd | supported |
| Agent command discovery | supported |
| Supported agent self-report integrations | supported |
| Agent process-tree detection | supported |
| Git/worktree detection from known cwd | supported |
| System notifications and MP3 sounds | supported |
| Plugins | preview |
| Pane screen history | supported |
| Nested launch override | supported |

Local persistent sessions continue running after the client detaches or its terminal window closes. Servers and pane processes launched through Windows OpenSSH also survive logout; run `herdr` again to reconnect.

Windows agent process detection scans descendants of the pane shell and recognizes direct agents plus common command wrappers, including npm/Node and Git Bash process chains. It follows Git Bash-launched agents across emulated `exec` boundaries, but it is not the same as Unix foreground process-group detection.

Windows integration installation currently supports Pi, OMP, Claude Code, Codex, GitHub Copilot CLI, OpenCode, Kilo Code CLI, Droid, Kimi Code CLI, Qoder CLI, and Antigravity CLI. Availability is narrower than on Unix; Herdr hides or rejects integrations whose install format is not supported on Windows. Devin CLI process and screen detection work on Windows, but its optional session-identity hook cannot currently be installed.

Plugins support `windows` as a manifest platform in preview. GitHub install, local link, build commands, actions, events, and plugin panes are best-effort on Windows. Commands are argv commands and must be Windows-compatible; Node package shims such as `npm`, `bun`, and `node` are expected to work when they are on `PATH`, while Unix-only examples using `sh` or Bash need Windows-specific alternatives. Platform filters skip unsupported build commands and return `platform_unsupported` for unsupported actions or panes.

## Partial support

| Capability | Status |
| --- | --- |
| Live cwd after shell `cd` | partial |
| Live cwd via shell integration/OSC7 | supported |
| Clipboard image paste to agents in local panes | terminal/agent dependent |
| CJK IME composition anchoring | partial |
| Prefix input-source switching (Korean IME) | partial |
| Kitty graphics rendering | terminal dependent |
| Host cursor rendering | partial |

Herdr can launch panes in the right directory and create the initial workspace from the directory where you started Herdr. After startup, the process field Herdr can inspect does not reliably track later logical `cd` changes in PowerShell. Use Herdr integrations or prompt shell integration for live cwd reporting.

During `herdr --remote`, the configured remote image paste key reads a Windows clipboard image and transfers it to the remote host. Dropping one local image file into Windows Terminal also transfers that file and pastes its remote path.

Prefix input-source switching is available as an opt-in experiment for the Korean IME. It switches Hangul input to English while prefix commands are active and restores the previous mode afterward:

```toml
[experimental]
switch_ascii_input_source_in_prefix = true
```

Other Windows IMEs are not supported by this option yet. See [Configuration](/docs/configuration/#prefix-input-source-switching).

Some Windows agents can receive `ctrl+v` and read clipboard images directly. Herdr's own clipboard-image reader is not wired into local native Windows panes, so agent-native image paste remains dependent on the terminal and agent. Agent image-paste shortcuts such as `alt+v` do not add a Herdr-managed local clipboard bridge. Remote clipboard image bridging is supported separately through `herdr --remote` to Linux and macOS hosts.

Kitty graphics is experimental and depends on the outer terminal. When `experimental.kitty_graphics = true`, Herdr emits Kitty graphics protocol output on Windows as it does on other platforms. This path has been exercised with Windows WezTerm hosting Herdr through WSL, but native Windows terminal and ConPTY combinations are not all verified. Windows Terminal does not expose the Kitty graphics path Herdr uses. Leave the option disabled unless the outer terminal supports Kitty graphics and you are testing that combination.

## Known caveats

### Cursor rendering

Herdr relies on ConPTY for native Windows panes. The Windows terminal cursor path can expose intermediate positions while a multiplexer repaints the screen. A native cursor may flicker, jump, or briefly remain at an old position during active output. This behavior also reproduces in other native Windows terminal multiplexers and with direct VT cursor-position stress tests, so Herdr cannot eliminate it while preserving native cursor behavior.

To prioritize visual stability, the default `host_cursor = "auto"` draws Herdr's cursor as terminal cell content on native Windows and WSL. Other Linux and macOS clients continue to use the native terminal cursor. The drawn Windows cursor is steady and non-blinking, but it does not provide the outer terminal's native blink, shape, or cursor color.

Windows does not use a drawn cursor to position IME composition and candidate UI. Korean, Japanese, or Chinese IME UI may therefore appear at the wrong location. If this affects you, opt back into the outer terminal cursor:

```toml
[ui]
host_cursor = "native"
```

Native mode restores the IME anchor, but it can reintroduce occasional cursor flicker, jumps, or stale cursor positions during active output.

### Keyboard and mouse

Windows terminals do not all report modified keys in the same shape. Herdr preserves mouse reporting and `ctrl+j` in Windows Terminal and Alacritty on Windows. The native Windows input path also preserves physical key presses, repeats, releases, standalone Escape, and `shift+enter` through default ConPTY panes. Modified keys still depend on the outer terminal reporting a distinct key event; if it reports `shift+enter` as plain Enter, Herdr can only forward plain Enter.

Windows packages include Microsoft's current app-local ConPTY runtime because the system ConPTY on older Windows 10 builds drops Kitty keyboard protocol sequences used by agents such as Kimi and Pi. Set `HERDR_WINDOWS_CONPTY=system` before starting Herdr only when diagnosing a compatibility problem with the bundled runtime.

## Copy and paste

Herdr's pane text copy works on Windows. Drag-select text inside a pane to copy through Herdr.

For text paste, use `ctrl+shift+v` in Windows Terminal. Multiline text paste is bracketed so shells and agent prompts receive it as one paste instead of submitting each line separately. Hold `shift` and right-click to use the outer terminal paste action instead of sending the click through Herdr.

## Not supported on Windows

| Capability | Status |
| --- | --- |
| Direct terminal attach (`herdr terminal attach`) | unsupported |
| Windows as a `herdr --remote` target host | unsupported |
| Live server handoff | unsupported |
| Unix file-descriptor handoff | unsupported |
| Unix foreground process groups | unsupported |
| Herdr clipboard image bridge in local native panes | unsupported |
| Signed binary / SmartScreen avoidance | unsupported |

From Windows Terminal, use the same remote command as Linux and macOS:

```powershell
herdr --remote workbox
```

The target host must run Linux or macOS. Herdr uses the installed Windows OpenSSH client and your SSH configuration. Windows OpenSSH does not use Herdr's Unix control-socket reuse, so key authentication through Windows `ssh-agent` is recommended to avoid repeated prompts during remote setup.

Windows updates run through the Windows installer and update the versioned install junction. Restart running Herdr sessions after updating. Live handoff is Unix-only.

## Reporting Windows issues

Include:

- Herdr version.
- Windows version.
- Terminal app.
- Shell, such as PowerShell or cmd.
- Whether you used a named `HERDR_SESSION`.
- Relevant Herdr logs.
- Exact steps to reproduce.
