# libghostty-vt local patches

This file tracks intentional local changes applied on top of the vendored
`libghostty-vt` source. Remove a patch only when the vendored source commit
contains the upstream behavior and the listed verification still passes.

## 0002 expose modifyOtherKeys mode through terminal data

status: active

patch: `vendor/patches/libghostty-vt/0002-expose-modify-other-keys-mode.patch`

herdr issue: none; fixes the performance regression exposed by
https://github.com/herdrdev/herdr/pull/2303

upstream discussion: not opened

upstream pr: not opened

vendored base: `44f2a44df7e8c4a0c6df3f7d872ef3d7ead88e51`

local files:

- `vendor/libghostty-vt/include/ghostty/vt/terminal.h`
- `vendor/libghostty-vt/src/terminal/c/terminal.zig`

reason: Herdr must know whether xterm modifyOtherKeys mode 2 is active to
request printable key releases from the outer terminal. The formatter API can
recover this fact only by formatting the active screen and scrollback. A typed
terminal-data query exposes the authoritative scalar without formatting or
allocation. The local query uses value 41; upstream now owns the previous
local value 33 for VT processing errors.

remove when: the vendored source exposes an equivalent scalar query for
modifyOtherKeys mode 2 and Herdr can use it without this patch.

verification:

```sh
just test-one modify_other_keys
just test-one host_report_all_supplies_printable_releases_for_event_type_only_panes
just maintenance-test
just ui-hot-path-architecture-test
```

The former grapheme-default patch is replaced by upstream's public
`GHOSTTY_TERMINAL_OPT_MODE_DEFAULT` API. Herdr configures mode 2027 through that
API and tests that RIS restores it after a child disables it. The Wuffs C-only
mirror fix from Ghostty PR 13789 is also included in this vendored base.

## 0004 fix hosted Wuffs builds

status: active

patch: `vendor/patches/libghostty-vt/0004-fix-hosted-wuffs-builds.patch`

herdr issue: none; preserves Windows cross-compilation and non-SIMD hosted builds

upstream discussion: not opened

upstream pr: not opened

vendored base: `44f2a44df7e8c4a0c6df3f7d872ef3d7ead88e51`

local files:

- `vendor/libghostty-vt/pkg/wuffs/build.zig`
- `vendor/libghostty-vt/pkg/wuffs/src/main.zig`

reason: Wuffs now needs MSVC libc headers when targeting Windows. Zig's
`--libc` configuration reaches C compilation, but the translate-c dependency
requires its own explicit configuration. Forward the same file so cross-builds
can use an actual Windows SDK instead of changing the target ABI or skipping
compilation. Native builds without a libc override are unchanged.

The no-libc Wuffs module also exports hidden weak calloc/free stubs. On hosted
Linux with SIMD disabled, those definitions override the Rust executable's
libc allocator, causing immediate allocation failures. Limit the stubs to
freestanding targets; hosted embedders resolve these symbols through libc.

remove when: upstream forwards the build's libc configuration to the Wuffs
translator and prevents hosted allocator interposition, and both Windows
cross-compilation and non-SIMD native tests pass without this patch.

verification:

```sh
LIBGHOSTTY_VT_WINDOWS_LIBC=/path/to/windows-libc.txt just windows-lint
LIBGHOSTTY_VT_SIMD=false just test-one ghostty
just maintenance-test
```

## 0005 bounded word selection for wrapped link activation

status: active

patch: `vendor/patches/libghostty-vt/0005-bounded-word-selection.patch`

herdr issue: https://github.com/herdrdev/herdr/issues/1282

upstream discussion: not opened

upstream pr: not opened; related merged PR https://github.com/ghostty-org/ghostty/pull/10132
implements URL selection in the application layer, not the libghostty C API.

vendored base: `44f2a44df7e8c4a0c6df3f7d872ef3d7ead88e51`

local files:

- `vendor/libghostty-vt/include/ghostty/vt/selection.h`
- `vendor/libghostty-vt/src/lib_vt.zig`
- `vendor/libghostty-vt/src/terminal/Screen.zig`
- `vendor/libghostty-vt/src/terminal/c/main.zig`
- `vendor/libghostty-vt/src/terminal/c/selection.zig`

reason: Ctrl+click must resolve a wrapped token beyond the visible viewport
without scanning an arbitrarily long logical line. The new, opt-in API shares
one cell-inspection budget across both directions and returns no selection on
exhaustion, never a truncated link. Its scan skips wide-character spacer cells.
The existing word-selection functions and option layouts remain unchanged;
only Herdr's link activation uses the new function.

remove when: upstream provides an equivalent bounded, wrap-aware selection API
that handles wide-character spacers, and Herdr passes the tests below using it
without this patch.

verification:

```sh
just test-one link_target
just test-one link_activation
just test-one ctrl_click
just check
```

## 0006 clear screen while preserving the cursor line

status: active

patch: `vendor/patches/libghostty-vt/0006-clear-screen-preserving-cursor-line.patch`

herdr issue: none; requested in https://github.com/herdrdev/herdr/discussions/545

upstream discussion: not opened

upstream pr: not opened

vendored base: `44f2a44df7e8c4a0c6df3f7d872ef3d7ead88e51`

local files:

- `vendor/libghostty-vt/include/ghostty/vt/terminal.h`
- `vendor/libghostty-vt/src/lib_vt.zig`
- `vendor/libghostty-vt/src/terminal/c/main.zig`
- `vendor/libghostty-vt/src/terminal/c/terminal.zig`

reason: Herdr needs an explicit screen/history clear that preserves the cursor's
visible soft-wrapped line without writing to the child or interrupting a partial
VT sequence. The new C function operates directly on the screen, leaves alternate
screens untouched, clears image placements, and marks the result dirty.

remove when: the vendored C API provides an equivalent parser-independent clear
operation preserving the visible cursor line, and Herdr passes the checks below
using it without this patch.

verification:

```sh
just test-one clear_pane
just maintenance-test
just check
```

## 0007 experimental encoded PNG and immutable source retention

status: active

patch: `vendor/patches/libghostty-vt/0007-experimental-png-retention.patch`

herdr issue: none; maintainer-directed native Kitty forwarding experiment

upstream discussion: not opened

upstream pr: not opened

vendored base: `44f2a44df7e8c4a0c6df3f7d872ef3d7ead88e51`

local files:

- `vendor/libghostty-vt/include/ghostty/vt/terminal.h`
- `vendor/libghostty-vt/include/ghostty/vt/kitty_graphics.h`
- `vendor/libghostty-vt/src/terminal/c/terminal.zig`
- `vendor/libghostty-vt/src/terminal/c/kitty_graphics.zig`
- `vendor/libghostty-vt/src/terminal/kitty/graphics_image.zig`
- `vendor/libghostty-vt/src/terminal/kitty/graphics_exec.zig`
- `vendor/libghostty-vt/src/terminal/kitty/graphics_storage.zig`

reason: An explicitly enabled embedding mode retains structurally validated,
quiet PNG uploads as encoded bytes, avoiding pixel decoding before forwarding
through a multiplexer. The existing raw getters retain their meaning; separate
getters expose retained PNG bytes. Queries and response-bearing uploads retain
full validation, and animation operations materialize pixels transactionally.
Storage reserves both encoded bytes and expected decoded size.

This is default-off and experimental: CRC-valid corrupt compressed pixels may
be rejected later than in normal mode, including after placement. Quiet mode
suppresses replies, not validation semantics; this patch is not a claim of full
protocol-equivalent transparent forwarding. Herdr exercises this mode only in
tests; production PNG uploads retain full decoding and validation.

A separate default-off snapshot callback retains host-owned immutable raw RGBA
file backing before reading pixels. Herdr installs this callback automatically
on Linux, using same-filesystem CoW snapshots. It never retains a mutable producer pathname. Unsupported snapshots
use the original loader; animation materializes pixels transactionally. Backing
ownership and bounded reads are explicit in the embedding ABI.

remove when: upstream provides an equivalent opt-in owned encoded-image
representation and immutable host-backed raw sources with bounded storage,
strict query handling and lazy pixel materialization, or this experiment is retired.

verification:

```sh
just test-one native_source
just test-one png_forward_tests
just test-one kitty_png_replacement
just test-one kitty_file_image_survives
(cd vendor/libghostty-vt && zig build test-lib-vt -Dtest-filter='experimental PNG')
just check
```
