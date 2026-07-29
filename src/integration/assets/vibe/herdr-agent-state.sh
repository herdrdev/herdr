#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=vibe
# HERDR_INTEGRATION_VERSION=1

# Mistral Vibe has no SessionStart hook. It exposes only post_agent,
# pre_tool, and post_tool (see vibe/core/hooks/models.py). The post_agent
# event fires after each agent turn and carries the session_id, so herdr
# learns the session identity from it. There is no action argument: each
# hook type invokes its own command, so this script is registered solely
# under the post_agent hook in ~/.vibe/hooks.toml.

set -eu

hook_input_file="$(mktemp "${TMPDIR:-/tmp}/herdr-vibe-hook.XXXXXX")" || exit 0
trap 'rm -f "$hook_input_file"' EXIT HUP INT TERM
cat >"$hook_input_file" 2>/dev/null || true

[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import random
import socket
import time

source = "herdr:vibe"
pane_id = os.environ.get("HERDR_PANE_ID")
socket_path = os.environ.get("HERDR_SOCKET_PATH")
hook_input_file = os.environ.get("HERDR_HOOK_INPUT_FILE")

if not pane_id or not socket_path:
    raise SystemExit(0)

hook_input = {}
if hook_input_file:
    try:
        with open(hook_input_file, encoding="utf-8") as handle:
            content = handle.read()
        if content.strip():
            hook_input = json.loads(content)
    except Exception:
        hook_input = {}


def first_text(*keys):
    for key in keys:
        value = hook_input.get(key)
        if isinstance(value, str) and value:
            return value
    return None


# Vibe's only session-scoped hook is post_agent (HookType.POST_AGENT,
# hook_event_name "post_agent"). A missing field is allowed for forward
# compatibility; any other event is not session identity, so skip it.
hook_event_name = first_text("hook_event_name", "hookEventName")
if hook_event_name not in (None, "", "post_agent"):
    raise SystemExit(0)

# Skip subagent/child sessions: parent_session_id being set means this is
# a forked or nested session, not the pane's primary session.
parent_session_id = hook_input.get("parent_session_id")
if parent_session_id:
    raise SystemExit(0)

session_id = first_text("session_id", "sessionId")
agent_session_id = session_id if isinstance(session_id, str) and session_id else None
if not agent_session_id:
    raise SystemExit(0)

transcript_path = first_text("transcript_path")

request_id = f"{source}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}"
report_seq = time.time_ns()
params = {
    "pane_id": pane_id,
    "source": source,
    "agent": "vibe",
    "seq": report_seq,
    "agent_session_id": agent_session_id,
}
if transcript_path:
    params["agent_session_path"] = transcript_path
request = {
    "id": request_id,
    "method": "pane.report_agent_session",
    "params": params,
}

try:
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(0.5)
    client.connect(socket_path)
    client.sendall((json.dumps(request) + "\n").encode())
    try:
        client.recv(4096)
    except Exception:
        pass
    client.close()
except Exception:
    pass
PY
