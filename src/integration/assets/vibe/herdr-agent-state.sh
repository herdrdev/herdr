#!/bin/sh
# managed by herdr; reinstalling the integration replaces this file.
# HERDR_INTEGRATION_ID=vibe
# HERDR_INTEGRATION_VERSION=1

action="${1:-}"
case "$action" in
  working|blocked|idle) ;;
  *) exit 0 ;;
esac

[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

python3 -c '
import json
import os
import socket
import sys
import time

action = sys.argv[1]
try:
    payload = json.load(sys.stdin)
except Exception:
    payload = {}

session_id = payload.get("session_id")
if not isinstance(session_id, str) or not session_id:
    session_id = None

seq = time.time_ns()
params = {
    "pane_id": os.environ["HERDR_PANE_ID"],
    "source": "herdr:vibe",
    "agent": "vibe",
    "state": action,
    "seq": seq,
}
if session_id is not None:
    params["agent_session_id"] = session_id

request = json.dumps({"id": f"herdr:vibe:{seq}", "method": "pane.report_agent", "params": params})
try:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(0.5)
        client.connect(os.environ["HERDR_SOCKET_PATH"])
        client.sendall((request + "\n").encode())
        client.recv(4096)
except Exception:
    pass
' "$action" 2>/dev/null || true
