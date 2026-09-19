# Local Windows input gauntlet (experimental)

This is local Windows qualification infrastructure, not a claim that all Windows
input works. It sends real scan-code gestures through a new Windows Terminal
window, both directly to an observer and through an attached Herdr client. It
does not substitute `pane send-keys` for host input.

## Safety and prerequisites

Use an **unlocked, isolated interactive desktop** where you will not do other
work during automated input. The runner changes foreground focus and window
size. `SendInput` has an unavoidable check-to-use focus race; a foreground check
is not an OS security boundary. The runner rejects an elevated controller and
checks the target Terminal process token before focus, resize, and injection.
An elevation-query failure is also a refusal, not permission to proceed. Start
PowerShell and Terminal **without Run as administrator**, on a dedicated desktop. There is no unattended CI job or runner permission
change in this implementation.

Required: Windows, PowerShell **7** (`pwsh`), Python 3, Rust/Cargo, and at least
one Windows Terminal channel. Stable and Preview are discovered independently
through their installed packages. Explicit paths are available when discovery
does not work. No software is installed or updated. The actual Terminal process path/version is recorded,
not inferred from the requested channel or bundled OpenConsole version. Stable
and Preview must resolve to distinct installations: the runner compares Windows
file/directory identities before launch, then checks actual image, installation,
and PID/start-time identities after activation. Aliases cannot count one install
twice. The portable report validator rejects duplicate identities too.

**F12** aborts before the next automatic gesture. Moving focus away also aborts
further automatic input. Each injected chord contains its own releases; there
are no intentionally held modifiers between calls. Do not hold physical modifiers
or mouse buttons while starting a run. Normal controller cancellation runs cleanup;
bootstrap/probe leases also expire if the controller disappears. Lease expiry is
not a replacement for a secure isolated desktop.

Clipboard tests require an **empty clipboard**. Clear it yourself only after
saving anything you need. The runner will not replace existing text, images,
rich formats, or files. It writes its synthetic text while holding the clipboard
lock and clears it afterward only if its sequence number is unchanged. A later
user clipboard update is left alone. No original clipboard contents are logged.

## Run

From the repository in PowerShell 7:

```powershell
just test-windows-input
```

The full catalogue normally exits `2` because operator-assisted and explicitly
unimplemented qualification cases remain. That is incomplete coverage, not an
automated test failure; inspect the printed matrix and retained `report.json`.

### Expected outcome

A healthy run builds and identifies the current checkout, reaches the result
matrix, and reports no `FAIL` rows or cleanup errors. The default Herdr column
should identify itself as `Win32 (Herdr)*` from captured runtime evidence. A full
run may still exit `2` and show `MANUAL`, `NOT TESTED`, or a documented host
capability limitation when a terminal channel, physical gesture, or oracle is
unavailable. A focused campaign exits `0` when every selected observation passes;
exit `1` means an assertion, harness, or cleanup failure and needs investigation.

The recipe itself is the explicit opt-in to foreground input injection. It builds
the current checkout in release mode, stages that exact binary with the pinned
ConPTY runtime, and prints its path and hash. Use `-ExePath` only to compare a
specific old/new packaged binary.

Or invoke the script directly:

```powershell
pwsh -NoProfile -File scripts/test_windows_input.ps1 `
  -ExePath 'C:\test-app\herdr.exe' -AllowInputInjection `
  -StablePath 'C:\TerminalStable\WindowsTerminal.exe' `
  -PreviewPath 'C:\TerminalPreview\WindowsTerminal.exe'
```

The default tests current Herdr's **default** input policy. Diagnostic runs may
use `-Profile win32` or `-Profile vt`; they do not replace the default run.
`-Modes native,legacy,mok2,kitty`, `-Channels`, `-Paths`, `-Cases`, `-Widths`, and
`-Heights` select a focused campaign. In a direct PowerShell invocation, supply
arrays normally:

```powershell
.\scripts\test_windows_input.ps1 -ExePath 'C:\test-app\herdr.exe' `
  -AllowInputInjection -Modes legacy,kitty -Cases mouse-interleave,mode-transitions `
  -Widths 120 -Heights 30
```

`-Paths herdr-remote` runs the current client against its disposable local
server with the remote clipboard bridge active. It needs no SSH host. A focused
clipboard-image qualification is:

```powershell
.\scripts\test_windows_input.ps1 -AllowInputInjection `
  -Channels stable -Paths direct,herdr-remote -Modes legacy `
  -Cases clipboard-image,clipboard-mixed -Widths 80 -Heights 24
```

Dead-key acute composition is automatic when the target Terminal thread's
active layout exposes that physical mapping. The runner discovers and injects
the real scan-code chord; it never substitutes pasted or Unicode-packet text.
`-Manual` enables AltGr, IME, and the remaining guided composition cases. The operator must activate the
indicated layout/IME, perform the gesture in the test window, then return to the
controller and press Enter. Finish each prompt within the 90-second lease.
Record the layout you actually selected in your qualification notes; the report
also includes the observed foreground thread layout handle. Do not paste text
for a composition case. The runner does not install/change global layouts.

You can inspect the full catalogue without Windows or desktop interaction:

```powershell
pwsh -NoProfile -File scripts/test_windows_input.ps1 -ExePath unused -MatrixOnly
```

Every run needs a **new** output directory. By default it is
`.local/windows-input/<unique-id>/`. Do not reuse an old report directory.

## What the implementation measures

- Native console records, including down/up, repeat, virtual/scan codes, Unicode,
  modifier state, and raw payloads for other native record types.
- Legacy VT, modifyOtherKeys level 2, and Kitty disambiguation observer modes.
  These are **pane consumer modes**, not names for Herdr's outer reader.
- Physical-style Enter/Shift+Enter and other modifier chords, navigation and word
  movement chords, control keys, and editing keys. The oracle checks bytes or
  records; it does not claim that a particular editor implements word selection
  correctly merely because Ctrl+Shift+Right reached it.
- Actual Windows clipboard paste via Ctrl+V, including LF/CRLF/CR, whitespace,
  BMP Unicode/combining characters, and escape-looking text.
  A host binding or multiline-paste confirmation dialog can intercept the gesture;
  the runner does not dismiss unexpected dialogs or rebind Terminal shortcuts.
- Image-only Ctrl+V through the remote clipboard bridge, plus a mixed image/text
  clipboard check that proves Windows Terminal and Herdr preserve the text paste.
  The report records `paste_origin`, classified from the client's own mapper trace
  (`terminal-paste`, `empty-paste`, `key-event`, or `none`). A staged image only
  passes when the origin is a positively observed `empty-paste`: a paste the
  terminal issued for clipboard text fails, and a missing or inconclusive trace is
  `inconclusive` rather than a qualification (#4314).
- A full case pass at an observed 120×30 host size; keyboard/paste sentinels at
  **80, 119, 120, 121, 132, 160, 240 columns**, at 24 and 50 rows; then return to
  80 columns. This exercises narrow→wide→narrow resizing of the actual outer
  window. Both outer and pane dimensions are captured. Herdr chrome means pane
  width is not the same as outer width. An unreachable size is not a pass.
- Automatic active-layout dead-key acute input, plus guided AltGr, extended
  accent, and IME commits, with exact expected committed text.
- Ordered typing, mouse-motion reports, and bracketed paste in one capture, plus
  click/wheel reporting before and after a real focus cycle, the same mouse
  sentinel after resize, and legacy→modifyOtherKeys→Kitty→modifyOtherKeys→legacy
  transitions without restart.

The focus/resize mouse check runs in Windows Terminal. It guards Herdr's recovery
sequence but does not certify the Tabby/Alacritty host-specific report in #4284.
The catalogue also lists explicit **qualification gaps**: mouse drag and
right-edge coordinate mapping; visual reflow/wrapping; native held-key repeat;
lock/keypad combinations; dead-key cancellation; IME cancellation; capture/config
reload and attach cycles; injected setup/recovery faults; supplementary-plane and
confirmation-triggering burst paste; non-image file clipboard integrations; and
positive host-scrollback evidence for native PageUp/PageDown. These are recorded
`not_run` or `inconclusive`, not fabricated successes. They need
separate fixtures/oracles before becoming automated assertions. The catalogue is
broad; the automated run does **not cover every row**.

Injected PageUp/PageDown remains `inconclusive` through Herdr. On Windows
Terminal 1.24, both `SendInput` and `keybd_event` produced zeroed non-key records
even in the direct-host baseline, so absence of pane input cannot prove that
Herdr consumed the key. Conclusive qualification requires literal physical input
plus a positive pane scroll-offset change; that guided check is not automated yet.

## Evidence and verdicts

The retained directory contains:

- `matrix.json`: all cases, profiles, and declared byte/record expectations;
- `observations.json`: raw captures, per-case identity, measured dimensions,
  readiness/focus flags, errors, and cleanup results;
- `report.json`: interpreted verdicts and counts;
- per-window plans, nonce-bound observer acknowledgements, initial/final console
  modes, and bootstrap/probe error records.

The console ends with a capability matrix derived only from that run's captured
observations. Filtered, unavailable, or operator-assisted cases remain
`NOT TESTED` or `MANUAL`; the full evidence and reasons remain in `report.json`.
The Herdr column is labelled Win32 only after the current binary's selected
reader decodes a real nonce-owned Win32 serialized record. Configuration defaults
alone do not select the label; missing runtime evidence prints `UNKNOWN`.

Direct-host and through-Herdr observations are labelled separately. A direct-host
failure is **not automatically a Herdr bug**. Each row labels its failure scope as
`direct_host` or `through_herdr_not_yet_attributed`. For example, a terminal may
intercept Alt+Enter or use a different control-key encoding.

One explicit known-gap rule labels nonce-bound, complete **WT 1.24/1.25
direct-host mOK captures** of Shift/Ctrl/Ctrl+Shift+Enter or Shift+Tab as
`unsupported` only when they contain the exact legacy bytes. It does not exempt
other tests or versions, an empty/malformed capture, Kitty, or any through-Herdr
failure. Through-Herdr still must preserve the requested gesture.
Unsupported remains incomplete coverage, not green. Review captures before
adding other capability exceptions; never bless lost Shift information in Herdr
just to make a report green.

Comparisons consume the **complete captured sequence**, including a quiet interval
to catch trailing duplicates/releases. Paste must have one intact bracketed
wrapper and the complete payload. CR/LF variants are compared as logical newlines;
this is not proof of byte-exact line-ending policy. Native keys require their
non-modifier down/up pair, modifier state, and repeat count.

Exit codes:

- `0`: all observed assertions passed and no recorded coverage gaps/errors;
- `1`: an assertion, harness, or cleanup failed;
- `2`: coverage is incomplete (including deliberately unimplemented catalogue
  rows, missing hosts, or unavailable geometry).

An empty or missing report never means success. None of these statuses certifies
an entire Windows host.

## Cleanup and native handoff

Every window has a fresh nonce, named session, isolated config root, known
PowerShell pane shell, and observer acknowledgement. Inherited Herdr socket,
backend, session, and SSH variables are cleared in the actual launched process,
not just in the controller. Setup APIs only create/focus the observer pane.

Cleanup targets the named session, retained child-process handles, and the exact
nonce-bearing Terminal window. There is no process-name-wide kill or sweep of
newly appeared OpenConsole processes. Artifacts are retained. Forced cleanup and
console-mode mismatches are reported, not hidden behind a passing key test.

For release-risk input changes, qualify Stable and Preview, real focus-loss/F12
interruption, partial startup, clipboard changes, cleanup, and measured width
boundaries. Use old and new Herdr binaries in **separate runs** to establish that
a known regression is caught. Do not overwrite installations or reuse an earlier
server. Portable unit tests validate the catalogue and verdict logic; they do not
replace native Windows evidence.

The existing `windows_conpty_enhanced_input_probe.ps1` remains the downstream API
control. Existing Rust keyboard/Windows-translator tests remain the deterministic
parser controls. This runner complements them rather than cross-producting their
fixtures or pretending they establish host behavior.
