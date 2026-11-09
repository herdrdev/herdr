#!/usr/bin/env bash
# Runs the native app against a throwaway Shepherd server with a few spaces, tabs and panes, so it can be
# tried (or photographed) without touching the real server, sessions or agents. Everything is removed on exit.
#   dev/demo.sh                      open the window
#   SHOT=/tmp/x.png dev/demo.sh      save a picture of the window after a moment and exit
#   MODE=shepherd dev/demo.sh        start on the Shepherd panel instead of the terminal
#   TAB=agents|tasks|monitor, LAUNCH=1   start on that tab / with the pattern launcher open
#   SELECT=agent:researcher          start with that node selected in the pattern editor
#   PALETTE=text                     start with the command palette open and that text typed
# Variables: SERVER (shepherd binary), APP (shepherd-native binary).
set -u
HERE="$(cd "$(dirname "$0")/.." && pwd)"
SERVER="${SERVER:-$HOME/shepherd-worktrees/agent-skill/target/debug/shepherd}"
APP="${APP:-$HERE/target/release/shepherd-native}"
[ -x "$SERVER" ] || { echo "no server binary at $SERVER (set SERVER=...)"; exit 1; }
[ -x "$APP" ] || { echo "no app binary at $APP (run: cargo build --release -p shepherd-native)"; exit 1; }

T="$(mktemp -d /var/tmp/shep-native-demo.XXXXXX)"
mkdir -p "$T/bin" "$T/cfg/shepherd" "$T/cfg/shepherd-dev" "$T/state" "$T/home" "$T/alpha/.shepherd/patterns" "$T/beta"
# the Shepherd layer with stand-in agents, a sample pattern and a sample task
printf '[shepherd]\nenabled = true\n' >"$T/cfg/shepherd-dev/config.toml"
printf 'roles:\n  specialist:\n    vendor: codex\n    worktree: false\n' >"$T/cfg/shepherd/config.yaml"
# a stand-in for the `codex` agent: a script that shows a prompt and swallows input. It is first on PATH
# for the server, so no real agent is ever started (checked below).
printf '#!/bin/bash\nprintf "\\033[2J\\033[H› "\nwhile IFS= read -r _; do :; done\n' >"$T/bin/codex"
chmod +x "$T/bin/codex"
printf 'name: duo\nkind: blackboard\ndescription: two harmless agents\ntopics:\n- name: work\nagents:\n- id: a1\n  role: specialist\n  publishes: [work]\n  consumes: [work]\n- id: a2\n  role: specialist\n  publishes: [work]\n  consumes: [work]\n' >"$T/alpha/.shepherd/patterns/duo.yaml"
export HOME="$T/home" XDG_CONFIG_HOME="$T/cfg" XDG_STATE_HOME="$T/state" SHEPHERD_SOCKET_PATH="$T/s.sock"
unset SHEPHERD_ENV SHEPHERD_CLIENT_SOCKET_PATH SHEPHERD_PANE_ID
export PATH="$T/bin:$PATH"
[ "$(bash -ic 'command -v codex' 2>/dev/null)" = "$T/bin/codex" ] || { echo "the stand-in agent is not first on PATH; refusing to start (a real agent could run)"; rm -rf "$T"; exit 1; }

SERVER_PID=""
cleanup() {
  "$SERVER" server stop >/dev/null 2>&1
  sleep 1
  [ -n "$SERVER_PID" ] && ps -p "$SERVER_PID" -o args= 2>/dev/null | grep -q "$T" && kill "$SERVER_PID" 2>/dev/null
  rm -rf "$T"
}
trap cleanup EXIT INT TERM

(cd "$T/alpha" && setsid "$SERVER" server >"$T/server.log" 2>&1 & echo $! >"$T/pid")
SERVER_PID="$(cat "$T/pid")"
for _ in $(seq 1 50); do [ -S "$T/s-client.sock" ] && break; sleep 0.2; done
[ -S "$T/s-client.sock" ] || { echo "the server did not start; see $T/server.log"; cat "$T/server.log"; exit 1; }

json() { python3 -c "import sys,json; d=json.load(sys.stdin)['result']; print(eval(sys.argv[1]))" "$1"; }
A=$("$SERVER" workspace create --cwd "$T/alpha" --label alpha --no-focus | json "d['root_pane']['pane_id']")
"$SERVER" pane run "$A" "echo 'alpha: space with two tabs'; ls -la /usr/bin | head -8" >/dev/null
"$SERVER" tab create --workspace w1 --cwd "$T/alpha" --label logs --no-focus >/dev/null
B=$("$SERVER" workspace create --cwd "$T/beta" --label beta --focus | json "d['root_pane']['pane_id']")
"$SERVER" pane run "$B" "echo 'beta: a split pane'; seq 1 40" >/dev/null
"$SERVER" pane split "$B" --direction right --cwd "$T/beta" --no-focus >/dev/null
(cd "$T/alpha" && "$SERVER" task new "Sample task" --description "Something to try the panels with" --criteria "it works" >/dev/null 2>&1 && "$SERVER" pattern run duo --task "Task #1: Sample task" >/dev/null 2>&1)
"$SERVER" workspace focus w1 >/dev/null 2>&1
sleep 2
[ -n "${MODE:-}" ] && export SHEPHERD_NATIVE_MODE="$MODE"
[ -n "${TAB:-}" ] && export SHEPHERD_NATIVE_TAB="$TAB"
[ -n "${LAUNCH:-}" ] && export SHEPHERD_NATIVE_LAUNCH=1
[ -n "${SELECT:-}" ] && export SHEPHERD_NATIVE_SELECT="$SELECT"
[ -n "${PALETTE+x}" ] && export SHEPHERD_NATIVE_PALETTE="$PALETTE"
export SHEPHERD_BIN="$SERVER"   # the app asks it for the sessions (`session list`)

if [ -n "${SHOT:-}" ]; then
  SHEPHERD_NATIVE_SHOT="$T/shot.ppm" "$APP" >/dev/null 2>&1
  python3 -c "from PIL import Image; Image.open('$T/shot.ppm').save('$SHOT')" && echo "saved $SHOT"
else
  "$APP"
fi
