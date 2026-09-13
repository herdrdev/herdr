#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

# Additional clang arguments can supply a resource directory on minimal hosts.
"${BINDGEN:-bindgen}" vendor/libghostty-vt/include/ghostty/vt.h \
  --allowlist-type 'Ghostty.*' \
  --allowlist-function 'ghostty_.*' \
  --allowlist-var 'GHOSTTY_.*' \
  --with-derive-default \
  --output src/ghostty/bindings.rs \
  -- -Ivendor/libghostty-vt/include "$@"
