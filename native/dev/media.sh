#!/usr/bin/env bash
# Makes the demo pictures and the demo video of the native app, against a throwaway Shepherd server with a
# small sample project (one task, one pattern run, some bus traffic) and stand-in agents, so nothing of yours
# is touched and no real agent ever runs. Everything temporary is removed on exit.
#   dev/media.sh <out-dir>          writes <out-dir>/*.png, shepherd-demo.mp4 and shepherd-demo.gif
#   SHOTS=0 dev/media.sh <out-dir>  only the video;  FILM=0 dev/media.sh <out-dir>  only the pictures
# Variables: SERVER (shepherd binary), APP (shepherd-native binary), SIZE (window, default 1920x1080). Needs python3 with Pillow, and ffmpeg.
set -u
OUT="${1:?usage: dev/media.sh <out-dir>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
SERVER="${SERVER:-$HOME/shepherd-worktrees/agent-skill/target/debug/shepherd}"
APP="${APP:-$HERE/target/release/shepherd-native}"
[ -x "$SERVER" ] || { echo "no server binary at $SERVER (set SERVER=...)"; exit 1; }
[ -x "$APP" ] || { echo "no app binary at $APP (run: cargo build --release -p shepherd-native)"; exit 1; }
mkdir -p "$OUT"; OUT="$(cd "$OUT" && pwd)"

T=/tmp/shepherd-demo-env
rm -rf "$T"; mkdir -p "$T"
mkdir -p "$T/bin" "$T/cfg/shepherd" "$T/cfg/shepherd-dev" "$T/state" "$T/home" "$T/webgo/.shepherd/patterns" "$T/notes"
printf '[shepherd]\nenabled = true\n\n[shepherd.limits]\nmax_concurrent = 16\n' >"$T/cfg/shepherd-dev/config.toml"
# every role runs the stand-in `codex`: a script that shows a prompt and swallows input
printf 'roles:\n  brain: {vendor: codex, worktree: false}\n  coder: {vendor: codex, worktree: false}\n  reviewer: {vendor: codex, worktree: false}\n  tester: {vendor: codex, worktree: false}\n' >"$T/cfg/shepherd/config.yaml"
printf '#!/bin/bash\nprintf "\\033[2J\\033[H› "\nwhile IFS= read -r _; do :; done\n' >"$T/bin/codex"
chmod +x "$T/bin/codex"
cat >"$T/webgo/.shepherd/patterns/api-team.yaml" <<'YAML'
name: api-team
kind: orchestrator-worker
description: an orchestrator splits the work and three coders build it
topics:
- {name: orders, partitions: 4, key: task_id}
- {name: results, partitions: 1}
agents:
- id: orchestrator
  role: brain
  prompt: Split the objective into independent subtasks and publish them to orders.
  publishes: [orders]
  consumes: [results]
  start: true
- id: worker
  role: coder
  prompt: Take an order, do it, and publish the result to results.
  count: 3
  group: worker-pool
  publishes: [results]
  consumes: [orders]
YAML
export HOME="$T/home" XDG_CONFIG_HOME="$T/cfg" XDG_STATE_HOME="$T/state" SHEPHERD_SOCKET_PATH="$T/s.sock"
unset SHEPHERD_ENV SHEPHERD_CLIENT_SOCKET_PATH SHEPHERD_PANE_ID
export PATH="$T/bin:$PATH"
[ "$(bash -ic 'command -v codex' 2>/dev/null)" = "$T/bin/codex" ] || { echo "the stand-in agent is not first on PATH; refusing to start (a real agent could run)"; rm -rf "$T"; exit 1; }
(cd "$T/webgo" && git init -q && git -c user.email=a@b -c user.name=t commit -q --allow-empty -m init)

SERVER_PID=""
cleanup() {
  "$SERVER" server stop >/dev/null 2>&1
  sleep 1
  [ -n "$SERVER_PID" ] && ps -p "$SERVER_PID" -o args= 2>/dev/null | grep -q "$T" && kill "$SERVER_PID" 2>/dev/null
  rm -rf "$T"
}
trap cleanup EXIT INT TERM

(cd "$T/webgo" && setsid "$SERVER" server >"$T/server.log" 2>&1 & echo $! >"$T/pid")
SERVER_PID="$(cat "$T/pid")"
for _ in $(seq 1 50); do [ -S "$T/s-client.sock" ] && break; sleep 0.2; done
[ -S "$T/s-client.sock" ] || { echo "the server did not start; see $T/server.log"; cat "$T/server.log"; exit 1; }

json() { python3 -c "import sys,json; d=json.load(sys.stdin)['result']; print(eval(sys.argv[1]))" "$1"; }
A=$("$SERVER" workspace create --cwd "$T/webgo" --label webgo --focus | json "d['root_pane']['pane_id']")
# two panes with made-up agent transcripts (printed by plain scripts; no agent runs)
cat >"$T/bin/fake-brain" <<'TXT'
#!/bin/bash
g=$'\e[32m'; c=$'\e[36m'; d=$'\e[2m'; b=$'\e[1m'; o=$'\e[0m'
echo "${b}brain-1${o} ${d}claude opus · run api-team${o}"; echo
echo "${g}●${o} Read task #1: Create a Go server ${d}(6 acceptance criteria)${o}"
echo "${g}●${o} Plan 3 subtasks: ${c}scaffold${o} · ${c}handlers${o} · ${c}tests${o}"
echo "${g}●${o} shepherd.publish ${c}#orders${o}  subtask 1/3 ${d}→ p0${o}"
echo "${g}●${o} shepherd.publish ${c}#orders${o}  subtask 2/3 ${d}→ p1${o}"
echo "${g}●${o} shepherd.publish ${c}#orders${o}  subtask 3/3 ${d}→ p2${o}"
echo "${g}●${o} shepherd.poll ${c}#results${o}"
echo "  ${d}└ coder-1: scaffold done · go.mod, cmd/server/main.go${o}"
echo "  ${d}└ coder-2: handlers done · GET /, GET /health, 404${o}"
echo; echo "${g}✓${o} 2 of 3 results in; waiting for the tests"
TXT
cat >"$T/bin/fake-coder" <<'TXT'
#!/bin/bash
g=$'\e[32m'; c=$'\e[36m'; d=$'\e[2m'; b=$'\e[1m'; o=$'\e[0m'
echo "${b}coder-3${o} ${d}codex · worktree shep/3-tests${o}"; echo
echo "${g}●${o} Consume ${c}#orders${o} p2 ${d}· subtask 3/3 tests${o}"
echo "${g}●${o} Write ${b}cmd/server/main_test.go${o}  ${g}+61${o}"
echo "${g}●${o} Bash ${b}go vet ./...${o}"; echo "  ${d}└ ok${o}"
echo "${g}●${o} Bash ${b}go test ./...${o}"
echo "  ok  	webgo/cmd/server	0.012s"
echo "${g}●${o} Bash ${b}git commit -am 'test: cover /, /health and 404'${o}"
echo "  ${d}└ [shep/3-tests 4e1c9a2] 1 file changed, 61 insertions(+)${o}"
echo "${g}●${o} shepherd.publish ${c}#results${o}"
echo; echo "${g}✓${o} delivered; idle"
TXT
chmod +x "$T/bin/fake-brain" "$T/bin/fake-coder"
"$SERVER" pane run "$A" "PS1='\$ '; clear; fake-brain" >/dev/null
A2=$("$SERVER" pane split "$A" --direction right --cwd "$T/webgo" --no-focus | json "d['pane']['pane_id']")
"$SERVER" pane run "$A2" "PS1='\$ '; clear; fake-coder" >/dev/null
"$SERVER" tab create --workspace w1 --cwd "$T/webgo" --label logs --no-focus >/dev/null
B=$("$SERVER" workspace create --cwd "$T/notes" --label notes --no-focus | json "d['root_pane']['pane_id']")
"$SERVER" pane run "$B" "PS1='\$ '; clear; echo 'notes: scratch space'; seq 1 12" >/dev/null
(cd "$T/webgo" && "$SERVER" task new "Create a Go server" \
    --description "Build a small HTTP server in Go." \
    --criteria '`go build ./...` and `go vet ./...` pass' \
    --criteria '`GET /health` returns 200 with body `ok`' \
    --criteria '`GET /` returns 200 with body `hello`; unknown routes return 404' \
    --criteria 'It listens on `$PORT`, 8080 by default' \
    --criteria '`go test ./...` passes (httptest)' \
    --criteria 'Standard library only' >/dev/null 2>&1
  "$SERVER" pattern validate api-team 2>&1
  "$SERVER" task new "Add request logging" --description "Log method, path and status of every request." \
    --criteria 'Each request is logged on one line' --criteria 'Health checks are not logged' >/dev/null 2>&1
  "$SERVER" task new "Document the API" --description "Write the README section for the endpoints." \
    --criteria 'Every endpoint has an example' >/dev/null 2>&1
  "$SERVER" pattern run api-team --task "Task #1: Create a Go server" 2>&1
  "$SERVER" pattern run hierarchical --task "Task #2: Add request logging" 2>&1)
sleep 2
for i in 1 2 3 4; do "$SERVER" topic publish orders "subtask $i/4 for the Go server" --from brain-1 >/dev/null 2>&1; done
"$SERVER" topic publish results "scaffold done: go.mod and cmd/server/main.go" --from coder-1 >/dev/null 2>&1
"$SERVER" topic publish results "handlers done: GET / and GET /health, with httptest" --from coder-2 >/dev/null 2>&1
"$SERVER" topic publish root-orders "milestone: request logging for every endpoint" --from brain-5 >/dev/null 2>&1
"$SERVER" topic publish team-orders "middleware: log method, path and status" --from reviewer-1 >/dev/null 2>&1
"$SERVER" topic publish team-orders "tests: assert health checks are not logged" --from reviewer-2 >/dev/null 2>&1
"$SERVER" topic publish team-results "middleware merged into main.go" --from coder-7 >/dev/null 2>&1
"$SERVER" workspace focus w1 >/dev/null 2>&1
sleep 2
export SHEPHERD_BIN="$SERVER" SHEPHERD_NATIVE_SIZE="${SIZE:-1920x1080}"

if [ "${SHOTS:-1}" != 0 ]; then
  shot() { # name mode tab [window size]
    SHEPHERD_NATIVE_SIZE="${4:-$SHEPHERD_NATIVE_SIZE}" SHEPHERD_NATIVE_MODE="$2" SHEPHERD_NATIVE_TAB="$3" SHEPHERD_NATIVE_SHOT="$T/shot.ppm" "$APP" >/dev/null 2>&1
    python3 -c "from PIL import Image; Image.open('$T/shot.ppm').save('$OUT/$1.png', optimize=True)" && echo "saved $1.png"
  }
  shot terminal terminal overview 1920x760   # shorter, so the made-up transcripts fill the panes
  shot overview shepherd overview
  shot runs shepherd runs
  shot agents shepherd agents
  shot tasks shepherd tasks
  shot patterns shepherd patterns
  shot activity shepherd monitor
  shot skills shepherd skills
  shot events shepherd events
  shot models shepherd models
fi

if [ "${FILM:-1}" != 0 ]; then
  mkdir -p "$T/frames"
  SHEPHERD_NATIVE_FILM="$T/frames" "$APP" >/dev/null 2>&1
  n=$(ls "$T/frames" | wc -l)
  echo "recorded $n frames"
  if [ "$n" -gt 0 ]; then
    ffmpeg -v error -y -framerate 12 -i "$T/frames/frame-%05d.ppm" -c:v libx264 -pix_fmt yuv420p -crf 20 -movflags +faststart "$OUT/shepherd-demo.mp4" && echo "saved shepherd-demo.mp4"
    ffmpeg -v error -y -framerate 12 -i "$T/frames/frame-%05d.ppm" \
      -vf "fps=8,scale=960:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=128[p];[b][p]paletteuse=dither=bayer:bayer_scale=4" \
      "$OUT/shepherd-demo.gif" && echo "saved shepherd-demo.gif"
  fi
fi
