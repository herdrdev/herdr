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
