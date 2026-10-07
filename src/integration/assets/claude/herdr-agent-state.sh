#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=claude
# HERDR_INTEGRATION_VERSION=11

set -eu

action="${1:-}"
hook_input_file="$(mktemp "${TMPDIR:-/tmp}/herdr-claude-hook.XXXXXX")" || exit 0
trap 'rm -f "$hook_input_file"' EXIT HUP INT TERM
cat >"$hook_input_file" 2>/dev/null || true

case "$action" in
  session|title|session-name|notification) ;;
  *) exit 0 ;;
esac

[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
if [ "$action" = "title" ] || [ "$action" = "session-name" ]; then
  herdr_bin="${HERDR_BIN_PATH:-herdr}"
  if [ "$herdr_bin" = "herdr" ]; then
    command -v herdr >/dev/null 2>&1 || exit 0
  elif [ ! -x "$herdr_bin" ]; then
    exit 0
  fi
  if [ "$action" = "title" ]; then
    "$herdr_bin" agent turn-title --provider claude <"$hook_input_file" >/dev/null 2>&1 || true
  fi
  # Claude renames a session outside turn boundaries, so publish the current
  # name on every event this hook receives, not only at turn start.
  "$herdr_bin" agent session-name --provider claude <"$hook_input_file" >/dev/null 2>&1 || true
  exit 0
fi
command -v python3 >/dev/null 2>&1 || exit 0

HERDR_ACTION="$action" HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import random
import socket
import time
import tempfile

source = "herdr:claude"
action = os.environ.get("HERDR_ACTION", "")
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

if "CURSOR_VERSION" in os.environ or "cursor_version" in hook_input:
    raise SystemExit(0)
hook_event_name = str(hook_input.get("hook_event_name") or "")
if hook_event_name not in ("SessionStart", "Notification"):
    raise SystemExit(0)
is_subagent = bool(hook_input.get("agent_id"))
if is_subagent:
    raise SystemExit(0)
request_id = f"{source}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}"
report_seq = time.time_ns()
session_id = hook_input.get("session_id")
agent_session_id = session_id if isinstance(session_id, str) and session_id else None
transcript_path = hook_input.get("transcript_path")
agent_session_path = transcript_path if isinstance(transcript_path, str) and transcript_path else None
session_start_source = hook_input.get("source") if hook_event_name == "SessionStart" else None
if not isinstance(session_start_source, str) or not session_start_source:
    session_start_source = None
if agent_session_id:
    if hook_event_name == "SessionStart":
        # Pane-keyed mirrors must never retain a previous session's blocker.
        try:
            root = os.environ.get("XDG_STATE_HOME") or os.path.expanduser("~/.local/state")
            directory = os.path.join(root, "herdr", "agent-status")
            os.makedirs(directory, exist_ok=True)
            mirror = os.path.join(directory, pane_id.replace(":", "_") + ".json")
            try:
                with open(mirror, encoding="utf-8") as handle:
                    prior = json.load(handle)
            except (OSError, ValueError):
                prior = {}
            if prior.get("session_id") != agent_session_id:
                fd, staging = tempfile.mkstemp(dir=directory, prefix=".session-")
                with os.fdopen(fd, "w", encoding="utf-8") as handle:
                    json.dump({"v": 2, "agent": "claude", "session_id": agent_session_id,
                               "blocking": 0, "agents": 0, "gates": [], "items": [],
                               "seq": report_seq, "transcript_path": agent_session_path}, handle)
                os.replace(staging, mirror)
        except (OSError, ValueError):
            pass
    params = {
        "pane_id": pane_id,
        "source": source,
        "agent": "claude",
        "seq": report_seq,
        "agent_session_id": agent_session_id,
    }
    if agent_session_path:
        params["agent_session_path"] = agent_session_path
    if session_start_source:
        params["session_start_source"] = session_start_source
    if hook_event_name == "Notification":
        if hook_input.get("notification_type") not in ("permission_prompt", "idle_prompt"):
            raise SystemExit(0)
        params["state"] = "blocked"
        params["message"] = "permission_prompt" if hook_input.get("notification_type") == "permission_prompt" else "idle_prompt"
    request = {
        "id": request_id,
        "method": "pane.report_agent" if hook_event_name == "Notification" else "pane.report_agent_session",
        "params": params,
    }
else:
    raise SystemExit(0)

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
