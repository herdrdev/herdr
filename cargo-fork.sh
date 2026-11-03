#!/bin/bash
# Runs cargo with the toolchain this fork needs (Rust 1.96.1 via rustup, Zig 0.16 for ghostty-vt).
export PATH="$HOME/.cargo/bin:$PATH"
export ZIG="${ZIG:-$HOME/.local/opt/zig-x86_64-linux-0.16.0/zig}"
cd "$(dirname "$0")" && exec cargo "$@"
