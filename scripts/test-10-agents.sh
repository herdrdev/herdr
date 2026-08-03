#!/usr/bin/env bash
# 10 concurrent agents across 3 workspaces resurrect test
# Uses isolated named herdr session + herdr-fix binary
set -euo pipefail

HERDR="${HERDR_FIX:-/home/bhd/.local/bin/herdr-fix}"
SESS="herdr-10agent-$$"
BASE="/tmp/herdr-10agent-$$"
LOG_DIR="/tmp/herdr-10agent-logs-$$"
mkdir -p "$LOG_DIR"

cleanup() {
    pkill -f "herdr-fix --session $SESS" 2>/dev/null || true
    sleep 1
    rm -rf "$BASE" "$LOG_DIR" 2>/dev/null || true
    rm -rf "$HOME/.config/herdr/sessions/$SESS" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

echo "=== 10-agent / 3-workspace resurrect test ===" >&2
echo "Binary: $HERDR" >&2
echo "Session: $SESS" >&2

# Setup 3 workspaces
mkdir -p "$BASE/ws1" "$BASE/ws2" "$BASE/ws3"
for i in 1 2 3; do
    cd "$BASE/ws$i"
    git init -q
    echo "ws$i" > README.md
    git add . && git commit -qm "init ws$i"
done
cd "$HOME"

# Start isolated herdr
"$HERDR" --session "$SESS" server >"$LOG_DIR/server-1.log" 2>&1 &
SRV=$!
sleep 5
if ! kill -0 "$SRV" 2>/dev/null; then
    echo "FAIL: server didn't start" >&2
    cat "$LOG_DIR/server-1.log" >&2
    exit 1
fi

# Create 3 workspaces
for i in 1 2 3; do
    "$HERDR" --session "$SESS" workspace create --cwd "$BASE/ws$i" --label "ws$i" --no-focus >"$LOG_DIR/ws$i-create.log" 2>&1
done

sleep 2

# Get workspace IDs
W1=$("$HERDR" --session "$SESS" workspace list 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print([w['workspace_id'] for w in d['result']['workspaces'] if w.get('label')=='ws1'][0])")
W2=$("$HERDR" --session "$SESS" workspace list 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print([w['workspace_id'] for w in d['result']['workspaces'] if w.get('label')=='ws2'][0])")
W3=$("$HERDR" --session "$SESS" workspace list 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print([w['workspace_id'] for w in d['result']['workspaces'] if w.get('label')=='ws3'][0])")
echo "Workspace IDs: $W1, $W2, $W3" >&2

# Split panes to reach 10 total: ws1=4, ws2=3, ws3=3
# Correct syntax: `herdr pane split <wid>:p1 --direction right`
echo "=== Split panes ===" >&2

# ws1: 3 splits (p1 → p2, p3, p4)
for _ in 1 2 3; do
    RESULT=$("$HERDR" --session "$SESS" pane split "$W1:p1" --direction right 2>&1)
    echo "  ws1 split: $(echo "$RESULT" | python3 -c "import json,sys;print(json.load(sys.stdin).get('result',{}).get('pane',{}).get('pane_id','?'))" 2>/dev/null || echo "?")" >&2
    sleep 1
done

# ws2: 2 splits (p1 → p2, p3)
for _ in 1 2; do
    RESULT=$("$HERDR" --session "$SESS" pane split "$W2:p1" --direction right 2>&1)
    echo "  ws2 split: $(echo "$RESULT" | python3 -c "import json,sys;print(json.load(sys.stdin).get('result',{}).get('pane',{}).get('pane_id','?'))" 2>/dev/null || echo "?")" >&2
    sleep 1
done

# ws3: 2 splits (p1 → p2, p3)
for _ in 1 2; do
    RESULT=$("$HERDR" --session "$SESS" pane split "$W3:p1" --direction right 2>&1)
    echo "  ws3 split: $(echo "$RESULT" | python3 -c "import json,sys;print(json.load(sys.stdin).get('result',{}).get('pane',{}).get('pane_id','?'))" 2>/dev/null || echo "?")" >&2
    sleep 1
done

sleep 2

# Collect all pane IDs from workspace list (pane_count tells us)
echo "=== Workspace pane counts ===" >&2
"$HERDR" --session "$SESS" workspace list 2>/dev/null | python3 -c "
import json,sys
d=json.load(sys.stdin)
for w in d['result']['workspaces']:
    if w.get('label','').startswith('ws'):
        print(f\"  {w['label']}: {w['pane_count']} panes\")
" >&2

# Get all pane IDs by parsing workspace list + workspace get
ALL_PANES=""
for WID in "$W1" "$W2" "$W3"; do
    # Use workspace get to enumerate panes
    WS_INFO=$("$HERDR" --session "$SESS" workspace get "$WID" 2>/dev/null)
    PANE_IDS=$(echo "$WS_INFO" | python3 -c "
import json,sys
d=json.load(sys.stdin)
w=d['result']['workspace']
wid=w['workspace_id']
# workspace get doesn't list panes directly; we need to enumerate from layout
# Use a different approach: iterate p1..pN based on pane_count
count=w.get('pane_count',1)
for i in range(1, count+1):
    print(f'{wid}:p{i}')
" 2>/dev/null || echo "")
    ALL_PANES="$ALL_PANES $PANE_IDS"
done
echo "All panes: $ALL_PANES" >&2

# Start agents (mix of pi and hermes) with UNIQUE names
# Pattern: 7 pi + 3 hermes
echo "=== Start agents ===" >&2
IDX=0
STARTED=0
for pane in $ALL_PANES; do
    IDX=$((IDX+1))
    if [ $IDX -le 7 ]; then
        KIND="pi"
        NAME="pi-$IDX"
    else
        KIND="hermes"
        NAME="hermes-$IDX"
    fi
    echo "  [$IDX] $KIND ($NAME) → $pane" >&2
    if "$HERDR" --session "$SESS" agent start "$NAME" --kind "$KIND" --pane "$pane" >"$LOG_DIR/start-$IDX.log" 2>&1; then
        STARTED=$((STARTED+1))
    else
        echo "  WARN: start failed for $pane (see $LOG_DIR/start-$IDX.log)" >&2
    fi
    sleep 2  # Give agent time to spawn
done
echo "Started: $STARTED / $IDX" >&2

# Send prompts
echo "=== Send prompts ===" >&2
for pane in $ALL_PANES; do
    "$HERDR" --session "$SESS" agent prompt "$pane" "echo hello-$pane" >/dev/null 2>&1 || true
done

# Wait for sessions to register
echo "=== Wait 30s for session refs ===" >&2
sleep 30

# Force session save by listing workspaces
"$HERDR" --session "$SESS" workspace list >/dev/null 2>&1
sleep 2

SESS_DIR="$HOME/.config/herdr/sessions/$SESS"

# Verify pre-restart session refs
REFS_BEFORE=$(python3 -c "
import json,os
path='$SESS_DIR/session.json'
if not os.path.exists(path):
    print(0)
    exit()
with open(path) as f:
    d=json.load(f)
n=0
for w in d.get('workspaces',[]):
    if w.get('custom_name','').startswith('ws'):
        for t in w.get('tabs',[]):
            for p in t.get('panes',{}).values():
                if p.get('agent_session'): n+=1
print(n)
")
echo "Session refs before kill: $REFS_BEFORE" >&2

# Kill server
echo "=== Kill server ===" >&2
kill "$SRV" 2>/dev/null || true
sleep 3
wait "$SRV" 2>/dev/null || true

# Restart server
echo "=== Restart server ===" >&2
RUST_LOG=info "$HERDR" --session "$SESS" server >"$LOG_DIR/server-2.log" 2>&1 &
SRV=$!
sleep 15

# Verify restoration
WS_AFTER=$("$HERDR" --session "$SESS" workspace list 2>/dev/null | python3 -c "
import json,sys
d=json.load(sys.stdin)
print(len([w for w in d['result']['workspaces'] if w.get('label','').startswith('ws')]))
")
echo "Workspaces after restart: $WS_AFTER" >&2

PANES_AFTER=$("$HERDR" --session "$SESS" workspace list 2>/dev/null | python3 -c "
import json,sys
d=json.load(sys.stdin)
n=0
for w in d['result']['workspaces']:
    if w.get('label','').startswith('ws'):
        n += w.get('pane_count',0)
print(n)
")
echo "Panes after restart: $PANES_AFTER" >&2

# Result
echo "" >&2
echo "=== RESULT ===" >&2
PASS=true
[ "$STARTED" -ge 10 ] || { echo "FAIL: STARTED=$STARTED / 10"; PASS=false; }
[ "$WS_AFTER" -ge 3 ] || { echo "FAIL: WS=$WS_AFTER < 3"; PASS=false; }
[ "$PANES_AFTER" -ge 10 ] || { echo "FAIL: PANES=$PANES_AFTER < 10"; PASS=false; }

if [ "$PASS" = "true" ]; then
    echo "PASS: $STARTED/10 agents started, 3 WS + $PANES_AFTER panes restored" >&2
    exit 0
else
    echo "FAIL (started=$STARTED, ws=$WS_AFTER, panes=$PANES_AFTER)" >&2
    exit 1
fi
