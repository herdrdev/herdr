# libghostty-vt local patches

This file tracks intentional local changes applied on top of the vendored
`libghostty-vt` source. Remove a patch only when the vendored source commit
contains the upstream behavior and the listed verification still passes.

## 0001 default lib-vt panes to grapheme clustering

status: active

patch: `vendor/patches/libghostty-vt/0001-default-grapheme-cluster-mode.patch`

herdr issue: https://github.com/herdrdev/herdr/issues/243

upstream discussion: not opened; libghostty-vt currently exposes current mode mutation but no C API for configuring terminal default modes

upstream pr: not opened

vendored base: `c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3`

local files:

- `vendor/libghostty-vt/src/terminal/c/terminal.zig`

reason: Herdr renders terminal cells directly and requires DEC private mode
2027 to store flags, ZWJ emoji, and other multi-codepoint grapheme clusters in
one cell. This patch makes clustering active for new terminals and keeps it as
the reset default so RIS (`ESC c`) does not disable it.

remove when: libghostty-vt exposes a C API for setting default mode 2027, or
upstream makes grapheme clustering the lib-vt default, and the reset-survival
regression passes without this patch.

verification:

```sh
cargo nextest run --locked grapheme_cluster_mode_is_default_and_survives_full_reset
cargo nextest run --locked grapheme_cluster_mode_renders_flag_emoji_in_single_wide_cell
cargo nextest run --locked grapheme_cluster_mode_renders_zwj_family_in_single_wide_cell
```

## 0002 preserve proxied Kitty key metadata

status: active

patch: `vendor/patches/libghostty-vt/0002-proxied-kitty-key-metadata.patch`

herdr issue: https://github.com/herdrdev/herdr/issues/2514

upstream discussion: not opened; this extension is currently specific to terminal-proxy input

upstream pr: not opened

vendored base: `c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3`

local files:

- `vendor/libghostty-vt/include/ghostty/vt/key/event.h`
- `vendor/libghostty-vt/src/input/key.zig`
- `vendor/libghostty-vt/src/input/key_encode.zig`
- `vendor/libghostty-vt/src/input/key_mods.zig`
- `vendor/libghostty-vt/src/lib_vt.zig`
- `vendor/libghostty-vt/src/terminal/c/key_event.zig`
- `vendor/libghostty-vt/src/terminal/c/main.zig`

reason: Herdr proxies rich Kitty key reports between terminals. The source event
can contain explicit shifted/base-layout alternates and Hyper/Meta modifiers
that libghostty-vt cannot reconstruct from local physical-key and layout data.
The extension preserves those fields so Ghostty can become Herdr's single pane
key encoder without losing protocol metadata.

remove when: upstream libghostty-vt exposes equivalent proxy-event alternate
codepoints and Hyper/Meta modifier support, and Herdr's encoder parity corpus
passes without this patch.

verification:

```sh
cd vendor/libghostty-vt && zig build test-lib-vt -Dsimd=true
just test-one keyboard_corpus_survives_fragmentation_and_pane_encoding
```

## 0003 report Kitty repeat events

status: active

patch: `vendor/patches/libghostty-vt/0003-report-kitty-repeat-events.patch`

herdr issue: https://github.com/herdrdev/herdr/issues/2514

upstream discussion: not opened

upstream pr: not opened

vendored base: `c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3`

local files:

- `vendor/libghostty-vt/src/input/key_encode.zig`

reason: when Kitty event-type reporting is enabled, repeat events must remain
CSI-u events so applications can distinguish them from presses. Encoding a
repeat as plain text discards the event type at the pane boundary.

remove when: upstream libghostty-vt emits CSI-u for text-producing repeat
events whenever Kitty event-type reporting is enabled.

verification:

```sh
cd vendor/libghostty-vt && zig build test-lib-vt -Dsimd=true
just test-one keyboard_corpus_survives_fragmentation_and_pane_encoding
```

## 0004 encode extended function keys

status: active

patch: `vendor/patches/libghostty-vt/0004-encode-extended-function-keys.patch`

herdr issue: https://github.com/herdrdev/herdr/issues/2514

upstream discussion: not opened

upstream pr: not opened

vendored base: `c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3`

local files:

- `vendor/libghostty-vt/src/input/function_keys.zig`
- `vendor/libghostty-vt/src/input/key_encode.zig`

reason: libghostty-vt models F13-F25 but its legacy encoder has no entries for
them, silently suppressing keys that Herdr receives through Kitty input. The
extension uses the standard xterm/terminfo sequences, corrects modified F3 to
that same standard, and composes additional modifiers with each extended key's
implicit Shift or Control modifier. Modified F3 therefore shares the
`CSI 1;modifier R` byte shape used by a cursor position report, but terminal
input and terminal responses travel in opposite directions and are interpreted
in that context.

remove when: upstream libghostty-vt encodes F13-F25 in legacy mode with the
standard xterm sequences and modifier composition, and emits the standard
modified F3 sequence.

verification:

```sh
cd vendor/libghostty-vt && zig build test-lib-vt -Dsimd=true
just test-one keyboard_corpus_survives_fragmentation_and_pane_encoding
```

## 0005 encode terminal proxy key events deterministically

status: active

patch: `vendor/patches/libghostty-vt/0005-proxy-key-encoding.patch`

herdr issue: https://github.com/herdrdev/herdr/issues/2514

upstream discussion: not opened; this extension defines a terminal-proxy input
mode rather than changing Ghostty's local terminal input policy

upstream pr: not opened

vendored base: `c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3`

local files:

- `vendor/libghostty-vt/include/ghostty/vt/key/encoder.h`
- `vendor/libghostty-vt/src/input/key_encode.zig`
- `vendor/libghostty-vt/src/terminal/c/key_encode.zig`

reason: Herdr receives semantic key events from another terminal. Applying the
server host's macOS Option and Command conventions to those events makes the
same input encode differently on macOS and Linux. Proxy mode trusts the event's
modifiers and generated text, and preserves complete Alt-prefixed UTF-8
without changing Ghostty's default local input behavior. Herdr reapplies the
caller-owned option after every terminal-state refresh.

remove when: upstream libghostty-vt exposes equivalent host-independent proxy
encoding semantics and Herdr's cross-platform keyboard corpus passes without
this patch.

verification:

```sh
cd vendor/libghostty-vt && zig build test-lib-vt -Dsimd=true
just test-one keyboard_corpus_survives_fragmentation_and_pane_encoding
uv run python -m unittest scripts.test_vendor_libghostty_vt
```

## 0006 support Kitty function keys through F35

status: active

patch: `vendor/patches/libghostty-vt/0006-extended-function-keys-f35.patch`

herdr issue: https://github.com/herdrdev/herdr/issues/2514

upstream discussion: not opened

upstream pr: not opened

vendored base: `c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3`

local files:

- `vendor/libghostty-vt/include/ghostty/vt/key/event.h`
- `vendor/libghostty-vt/src/input/function_keys.zig`
- `vendor/libghostty-vt/src/input/key.zig`
- `vendor/libghostty-vt/src/input/key_encode.zig`
- `vendor/libghostty-vt/src/input/kitty.zig`

reason: the Kitty protocol defines F13-F35, but libghostty-vt stops its key
model at F25. Herdr can receive F26-F35 from its host terminal, so dropping
those events makes the proxy incomplete. The extension appends ABI values,
preserves existing key constants, and maps F26-F35 to their standard Kitty and
legacy xterm sequences.

remove when: upstream libghostty-vt models and encodes F26-F35 and Herdr's
keyboard corpus passes without this patch.

verification:

```sh
cd vendor/libghostty-vt && zig build test-lib-vt -Dsimd=true
just test-one ghostty_encodes_all_kitty_extended_function_keys
just test-one keyboard_corpus_survives_fragmentation_and_pane_encoding
```
