# portable-pty local patches

This file tracks intentional local changes applied on top of the vendored
`portable-pty` source. Remove a patch only when the upstream crate contains an
equivalent fix or exposes an option that lets Herdr keep the same behavior.

## 0001 control ConPTY loading

status: active

patch: `vendor/patches/portable-pty/0001-control-conpty-loading.patch`

herdr issues:

- https://github.com/herdrdev/herdr/issues/761
- https://github.com/herdrdev/herdr/issues/1533

upstream discussion: https://github.com/microsoft/terminal/issues/17452

upstream pr: none

vendored base: `portable-pty 0.9.0`

local files:

- `vendor/portable-pty/Cargo.toml`
- `vendor/portable-pty/Cargo.toml.orig`
- `vendor/portable-pty/src/win/psuedocon.rs`

reason: `portable-pty` intentionally probes a bare `conpty.dll` through the DLL
search path. Herdr must never load another application's DLL from `PATH`. The
Windows package ships a pinned Microsoft ConPTY runtime under `conpty/`; Herdr
verifies the exact DLL and x64/ARM64 host hashes, rejects reparse points and
unexpected files, then loads the DLL by absolute path with its dependency search
limited to that directory and System32. Installations without a bundle continue
using the ConPTY exports from the already loaded `kernel32.dll`. Set
`HERDR_WINDOWS_CONPTY=system` to bypass the bundle during compatibility
recovery.

remove when: upstream `portable-pty` exposes hash-verified app-local and system
ConPTY selection with constrained DLL loading and no bare DLL search, or Herdr
replaces the Windows PTY backend.

verification:

```sh
python3 -m unittest scripts.test_vendor_portable_pty
```

On Windows, run the enhanced-input CI probe against the packaged `conpty/`
bundle. It verifies the exact OpenConsole process path on Server 2022 and 2025,
rejects a tampered bundle, and verifies the explicit system fallback.

## 0002 expose Windows raw command tails

status: active

patch: `vendor/patches/portable-pty/0002-windows-raw-command-tail.patch`

herdr issue: https://github.com/herdrdev/herdr/issues/1041

upstream discussion: none

upstream pr: none

vendored base: `portable-pty 0.9.0`

local files:

- `vendor/portable-pty/src/cmdbuilder.rs`

reason: Herdr needs to launch `cmd.exe /d /c` with the user-authored command
tail parsed as shell text. `portable-pty` represents commands as argv and
ArgvQuote escapes embedded quotes, which changes how `cmd.exe` parses the raw
command string.

remove when: upstream `portable-pty` exposes Windows raw command-line tail
support or Herdr replaces this launch path.

verification:

```sh
python3 -m unittest scripts.test_vendor_portable_pty
```

On Windows, also run `cargo test raw_arg_appends_unescaped_windows_command_tail`.

## 0003 reject malformed Windows environments

status: active

patch: `vendor/patches/portable-pty/0003-reject-malformed-windows-environments.patch`

herdr issue: https://github.com/herdrdev/herdr/issues/3430

upstream discussions:

- https://github.com/wezterm/wezterm/issues/4364
- https://github.com/warpdotdev/warp/commit/2992d02e3e38af697c83a3bd6f20003f54ffe066

upstream pr: none

vendored base: `portable-pty 0.9.0`

local files:

- `vendor/portable-pty/src/cmdbuilder.rs`

reason: Windows process environments may contain a registry value with an
empty name or a non-string type. `winreg 0.10` converts `REG_MULTI_SZ` to an
`OsString` that can contain embedded nulls. `portable-pty` serialized those
values unchanged, producing an invalid environment block that makes
`CreateProcessW` fail with `ERROR_INVALID_PARAMETER` (87). Import only Windows
environment string types and omit entries that cannot form one complete
`name=value\0` record.

remove when: upstream `portable-pty` both imports only valid Windows environment
string types and prevents malformed names or values from corrupting the process
environment block, or Herdr replaces this launch path.

verification:

```sh
python3 -m unittest scripts.test_vendor_portable_pty
```

On Windows, also run:

```sh
cargo test --manifest-path vendor/portable-pty/Cargo.toml windows_environment_rejects_malformed_entries
```
