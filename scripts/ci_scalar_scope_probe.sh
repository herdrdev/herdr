#!/usr/bin/env bash
set -euo pipefail

for scope in all bin bin all; do
    # Match the normal build state immediately before CI switches to scalar mode.
    env -u LIBGHOSTTY_VT_SIMD -u LIBGHOSTTY_VT_OPTIMIZE \
        cargo nextest run --locked --no-run
    target_args=()
    if [[ "$scope" == bin ]]; then
        target_args=(--bin herdr)
    fi
    /usr/bin/time -f "CI_SCALAR_PROBE scope=$scope seconds=%e" \
        env LIBGHOSTTY_VT_SIMD=false LIBGHOSTTY_VT_OPTIMIZE=ReleaseSafe \
        cargo nextest run --locked "${target_args[@]}" ghostty \
            --status-level fail --final-status-level fail \
            --failure-output final --success-output never
done
