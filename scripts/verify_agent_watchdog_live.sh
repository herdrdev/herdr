#!/usr/bin/env sh
# Compatibility entry point; the isolated matrix harness is implemented in Python.
exec python3 "$(dirname "$0")/verify_agent_watchdog_live.py" "$@"
