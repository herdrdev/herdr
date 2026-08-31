---
title: Install Herdr
description: Install, update, and verify Herdr on Linux, macOS, and Windows.
---

Herdr publishes stable-channel binaries for Linux, macOS, and Windows. Windows is generally available, with documented platform-specific limitations and ongoing fixes.

## Install

On Linux or macOS, run:

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

On Windows, run:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"
```

If endpoint security blocks that fileless PowerShell command, open Command Prompt and run:

```cmd
curl.exe -fsSLo install.cmd https://herdr.dev/install.cmd && install.cmd && del install.cmd
```

The installer downloads the release binary for your platform and places it on your PATH. New direct installs use the stable update channel. Existing Windows preview installs stay on preview until you switch them with `herdr channel set stable`. The Windows installer uses versioned install folders and updates a `current` junction, so updates do not need to overwrite a running `herdr.exe`.

## Install with Homebrew

If you already use Homebrew:

```bash
brew install herdr
```

## Install with mise

If you already use mise:

```bash
mise use -g herdr
```

If mise reports `herdr not found in mise tool registry`, update mise and retry. Older mise versions predate the Herdr registry entry; `mise use -g github:herdrdev/herdr` works as a temporary fallback.

## Install with Nix

If you already use Nix, Herdr provides a flake that builds Herdr from source:

```bash
nix run github:herdrdev/herdr/v0.x.y
nix build github:herdrdev/herdr/v0.x.y
nix profile install github:herdrdev/herdr/v0.x.y
```

Replace `v0.x.y` with the latest release tag. You can omit the tag to track `master`, but release tags are recommended for normal installs.

The flake also exposes a development shell:

```bash
nix develop github:herdrdev/herdr
```

Use the same Nix workflow to update Herdr. For a profile install, list your profile entries and upgrade the Herdr entry:

```bash
nix profile list
nix profile upgrade <index-or-name>
```

If Herdr is an input in your own flake, update that input and rebuild your system, Home Manager, or development environment:

```bash
nix flake update herdr
```

## Download manually

You can also download a binary from [GitHub releases](https://github.com/herdrdev/herdr/releases).

Choose the asset that matches your system:

| System | Asset |
| --- | --- |
| Linux x86_64 | `herdr-linux-x86_64` |
| Linux aarch64 | `herdr-linux-aarch64` |
| macOS Intel | `herdr-macos-x86_64` |
| macOS Apple silicon | `herdr-macos-aarch64` |
| Windows x86_64 | `herdr-windows-x86_64.zip` |

On Linux or macOS, make it executable and move it somewhere on your PATH.

```bash
chmod +x herdr-linux-x86_64
mv herdr-linux-x86_64 ~/.local/bin/herdr
```

### Windows archive

Stable releases and preview prereleases both include `herdr-windows-x86_64.zip`. The archive contains `herdr.exe` and its app-local ConPTY runtime. Keep the extracted directory together; do not copy only `herdr.exe`.

## Verify

Start Herdr:

```bash
herdr
```

If your shell cannot find `herdr`, restart the terminal or check that the install directory is on your PATH.

## Update

Herdr checks for new releases and notifies you in the app. You can update manually:

```bash
herdr update
```

Use `herdr update` only for installs managed by Herdr's own installer. Update Homebrew, mise, and Nix installs through those package managers instead.

Direct installs on Linux, macOS, and Windows use the stable update channel by default. To opt into preview builds from `master`, set the channel:

```bash
herdr channel set preview
```

Switch a direct install back to stable the same way:

```bash
herdr channel set stable
```

For direct installs, changing channels checks the selected channel and installs its latest binary. If that update fails, run `herdr update` to retry from the configured channel.

Preview builds are regularly published GitHub prereleases from the current development branch. They are useful when you want fixes before the next stable release, but they can regress. Homebrew, mise, and Nix installs do not use the preview channel.

Stable is the recommended channel for normal Windows use. Preview receives fixes sooner but can regress, so opt in only when you want that tradeoff. Existing Windows preview installs remain on preview until you switch them explicitly. If an older preview build rejects `herdr channel set stable`, run `herdr update` once on preview, then retry the channel switch.

By default, `herdr update` installs the new binary and leaves compatible running sessions alone. If an update changes Herdr's client/server protocol, Herdr asks whether to stop the old server after installing. Stop the old server to use the new version. Stopping the server exits its pane processes. For the default session, run `herdr server stop`, then run `herdr` again. For a named session, run `herdr session stop <name>`, then run `herdr session attach <name>` again.

To opt into experimental live server handoff for supported running sessions, run:

```bash
herdr update --handoff
```

Live handoff does not apply to Homebrew, mise, or Nix package-manager updates. For those installs, update with the package manager, then restart that Herdr session when you are ready to use the new server. If a running session still uses the old server, stop it with `herdr server stop` or `herdr session stop <name>`, then run Herdr again.

## Requirements

Stable-channel binaries are available for Linux, macOS, and Windows x86_64. See [Windows support](/docs/windows-beta/) for supported workflows and known limitations. Windows ARM64 runs the x86_64 build under Windows emulation.
