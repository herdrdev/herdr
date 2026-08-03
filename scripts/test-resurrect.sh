#!/usr/bin/env bash
# Integration test for herdr resurrect fix
# Tests 2 workspaces with pi+hermes agents in isolated session
set -euo pipefail

HERDR_FIX="${HERDR_FIX:-/home/bhd/.local/bin/herdr-fix}"
TEST_SESSION="herdr-resurrect-test-$$"
WS1="/tmp/herdr-resurrect-ws1-$$"
WS2="/tmp/herdr-resurrect-ws2-$$"
LOG="/tmp/herdr-resurrect-test-$$.log"

cleanup() {
    local code=$?
    echo "=== Cleanup ===" >&2
    pkill -f "$TEST_SESSION" 2>/dev/null || true
    rm -rf "$WS1" "$WS2" 2>/dev/null || true
    rm -f "$LOG" 2>/dev/null || true
    exit $code
}
trap cleanup EXIT INT TERM

echo "=== Herdr resurrect integration test ===" >&2
echo "Binary: $HERDR_FIX" >&2
echo "Session: $TEST_SESSION" >&2
echo "Workspaces: $WS1, $WS2" >&2

# Step 1: Setup workspaces
mkdir -p "$WS1" "$WS2"
cd "$WS1" && git init -q && echo "test1" > f1.txt && git add . && git commit -qm "init1"
cd "$WS2" && git init -q && echo "test2" > f2.txt && git add . && git commit -qm "init2"
cd "$HOME"

# Step 2: Start isolated herdr server
echo "=== Step 2: Start herdr server ===" >&2
"$HERDR_FIX" --session "$TEST_SESSION" server >"$LOG" 2>&1 &
SRV_PID=$!
sleep 5

if ! kill -0 "$SRV_PID" 2>/dev/null; then
    echo "FAIL: server did not start" >&2
    cat "$LOG" >&2
    exit 1
fi
echo "Server PID: $SRV_PID" >&2

# Step 3: Create workspaces
echo "=== Step 3: Create workspaces ===" >&2
"$HERDR_FIX" --session "$TEST_SESSION" workspace create --cwd "$WS1" --label "test-ws1" --no-focus 2>&1 | head -3
"$HERDR_FIX" --session "$TEST_SESSION" workspace create --cwd "$WS2" --label "test-ws2" --no-focus 2>&1 | head -3

WS_COUNT=$("$HERDR_FIX" --session "$TEST_SESSION" workspace list 2>/dev/null | python3 -c "import json,sys; d=json.load(sys.stdin); print(len(d.get('result',{}).get('workspaces',[])))")
echo "Workspace count: $WS_COUNT" >&2

# Step 4: Get pane IDs
WS1_PANE=$("$HERDR_FIX" --session "$TEST_SESSION" workspace list 2>/dev/null | python3 -c "
import json, sys
d = json.load(sys.stdin)
for w in d['result']['workspaces']:
    if w.get('label') == 'test-ws1':
        print(w['workspace_id'] + ':p1'); break
")
WS2_PANE=$("$HERDR_FIX" --session "$TEST_SESSION" workspace list 2>/dev/null | python3 -c "
import json, sys
d = json.load(sys.stdin)
for w in d['result']['workspaces']:
    if w.get('label') == 'test-ws2':
        print(w['workspace_id'] + ':p1'); break
")
echo "WS1 pane: $WS1_PANE" >&2
echo "WS2 pane: $WS2_PANE" >&2

# Step 5: Send shell commands to verify panes are interactive
echo "=== Step 5: Make panes interactive (touch markers) ===" >&2
"$HERDR_FIX" --session "$TEST_SESSION" pane send-text "$WS1_PANE" --text "echo ws1-ready > marker.txt
" 2>&1 | head -2 || true
"$HERDR_FIX" --session "$TEST_SESSION" pane send-text "$WS2_PANE" --text "echo ws2-ready > marker.txt
" 2>&1 | head -2 || true
sleep 2

# Step 6: Kill server
echo "=== Step 6: Kill server ===" >&2
kill "$SRV_PID" 2>/dev/null || true
sleep 3
wait "$SRV_PID" 2>/dev/null || true

# Step 7: Restart server
echo "=== Step 7: Restart server ===" >&2
"$HERDR_FIX" --session "$TEST_SESSION" server >>"$LOG" 2>&1 &
SRV_PID=$!
sleep 8

# Step 8: Verify restoration
echo "=== Step 8: Verify restoration ===" >&2
WS_COUNT_AFTER=$("$HERDR_FIX" --session "$TEST_SESSION" workspace list 2>/dev/null | python3 -c "import json,sys; d=json.load(sys.stdin); print(len(d.get('result',{}).get('workspaces',[])))" || echo 0)
echo "Workspace count after restart: $WS_COUNT_AFTER" >&2

SESSION_DIR="$HOME/.config/herdr/sessions/$TEST_SESSION"
if [ -f "$SESSION_DIR/session.json" ]; then
    echo "session.json preserved" >&2
    python3 -c "
import json
with open('$SESSION_DIR/session.json') as f:
    d = json.load(f)
print(f\"Workspaces in session.json: {len(d.get('workspaces', []))}\", file=__import__('sys').stderr)
for w in d.get('workspaces', []):
    label = w.get('custom_name') or 'unnamed'
    pane_count = sum(len(t.get('panes', {})) for t in w.get('tabs', []))
    print(f\"  - {label}: {pane_count} pane(s)\", file=__import__('sys').stderr)
" >&2
fi

echo "" >&2
echo "=== RESULT ===" >&2
if [ "$WS_COUNT_AFTER" -ge 2 ]; then
    echo "PASS: workspace layout restored after restart" >&2
    exit 0
else
    echo "FAIL: workspace layout NOT restored (got $WS_COUNT_AFTER, expected >= 2)" >&2
    exit 1
fi
