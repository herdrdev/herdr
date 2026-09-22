#!/bin/sh
# managed by herdr; reinstalling the integration replaces this file.
# HERDR_INTEGRATION_ID=kiro
# HERDR_INTEGRATION_VERSION=1

set -eu

[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
if [ -n "${HERDR_BIN_PATH:-}" ]; then
    [ -x "$HERDR_BIN_PATH" ] || exit 0
else
    command -v herdr >/dev/null 2>&1 || exit 0
fi
command -v python3 >/dev/null 2>&1 || exit 0

python3 -c '
import json
import os
import subprocess
import sys

MAX_SAFE_INTEGER = 9_007_199_254_740_991


def nonempty_string(value):
    return isinstance(value, str) and bool(value)


def positive_integer(value):
    return type(value) is int and value > 0


def positive_safe_integer(value):
    return positive_integer(value) and value <= MAX_SAFE_INTEGER


def client_is_alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OSError:
        return False
    return True


try:
    payload = json.load(sys.stdin)
    if not isinstance(payload, dict):
        raise ValueError

    session_id = payload.get("session_id")
    cwd = payload.get("cwd")
    location = payload.get("session_location")
    client_pid = payload.get("client_pid")
    transition_seq = payload.get("transition_seq")
    if (
        payload.get("hook_event_name") != "SessionChange"
        or not nonempty_string(session_id)
        or not nonempty_string(cwd)
        or location not in ("local", "remote")
        or not positive_integer(client_pid)
        or not positive_safe_integer(transition_seq)
        or not client_is_alive(client_pid)
    ):
        raise ValueError

    command = os.environ.get("HERDR_BIN_PATH") or "herdr"
    common_args = [
        command,
        "pane",
        "report-agent-session" if location == "local" else "release-agent",
        os.environ["HERDR_PANE_ID"],
        "--source",
        "herdr:kiro-v3",
        "--agent",
        "kiro",
        "--seq",
        str(transition_seq),
    ]
    if location == "local":
        common_args.extend(
            [
                "--agent-session-id",
                session_id,
                "--session-start-source",
                "select",
            ]
        )

    subprocess.run(
        common_args,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=1,
        check=False,
    )
except Exception:
    pass
' 2>/dev/null || true
