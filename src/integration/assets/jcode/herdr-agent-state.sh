#!/bin/sh
# managed by herdr; reinstalling the integration replaces this file.
# HERDR_INTEGRATION_ID=jcode
# HERDR_INTEGRATION_VERSION=1

# Jcode currently has one command slot per lifecycle event. Herdr saves any
# pre-existing session_start command beside this script and forwards the event
# to it, preserving the user's observer while adding session identity.
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" 2>/dev/null && pwd) || exit 0
previous_hook_file="$script_dir/herdr-session-start.previous"

# Preserve the exact shell semantics of the observer Jcode previously ran.
# This happens independently of Python so installing Herdr never disables it.
if [ -s "$previous_hook_file" ]; then
    previous_command=$(cat -- "$previous_hook_file" 2>/dev/null) || previous_command=
    if [ -n "$previous_command" ]; then
        (sh -c "$previous_command" </dev/null >/dev/null 2>&1 &)
    fi
fi

command -v python3 >/dev/null 2>&1 || exit 0
python3 - <<'PY'
import json
import os
import socket
import time

if os.environ.get("HERDR_ENV") != "1":
    raise SystemExit(0)

pane_id = os.environ.get("HERDR_PANE_ID")
socket_path = os.environ.get("HERDR_SOCKET_PATH")
session_id = os.environ.get("JCODE_HOOK_SESSION_ID")
if not pane_id or not socket_path or not session_id:
    raise SystemExit(0)

hook_source = os.environ.get("JCODE_HOOK_SOURCE")
if hook_source in ("create", "attach"):
    session_start_source = "startup"
elif hook_source == "resume":
    session_start_source = "resume"
else:
    session_start_source = None

seq = time.time_ns()
params = {
    "pane_id": pane_id,
    "source": "herdr:jcode",
    "agent": "jcode",
    "seq": seq,
    "agent_session_id": session_id,
}
if session_start_source is not None:
    params["session_start_source"] = session_start_source

request = json.dumps(
    {
        "id": f"herdr:jcode:{seq}",
        "method": "pane.report_agent_session",
        "params": params,
    }
)

try:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(0.5)
        client.connect(socket_path)
        client.sendall((request + "\n").encode())
        client.recv(4096)
except Exception:
    pass
PY
