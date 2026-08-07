# Herdr

Herdr is a terminal-based agent runtime: a mouse-first TUI, backed by a persistent server, for
running and supervising coding agents in real terminal panes.

## Language

### Structure

**Session**:
A persistent Herdr server namespace. The default `herdr` command attaches to the default session;
`herdr session list|attach|stop` manage session lifecycle only. A session holds one or more
workspaces plus the server's live state (sockets, data dir).
_Avoid_: "instance", or conflating with an agent's own native conversation session (a separate,
agent-owned concept — see [Native agent session] below) or with a login/auth session.

**Workspace**:
The top-level project container — one per repo, task, or investigation. A workspace owns tabs and
panes, and its sidebar summary rolls up from the state of the agents inside it.
_Avoid_: "project", "space" alone (see [Space] below, which names workspace git identity, not the
container itself).

**Tab**:
A layout inside a workspace — a split-pane arrangement used to separate views such as `agents`,
`logs`, `server`, or `review`.
_Avoid_: "view", "window".

**Pane**:
A real terminal: Herdr renders its output, forwards input to the underlying process, and preserves
it across client detach. Internally this splits into `PaneState` (pure, testable identity/viewport
data — e.g. `seen`, the attached terminal id) and `PaneRuntime` (the live PTY, child process, and
detection task handles); state and runtime are deliberately kept separate so pane logic is testable
without a real terminal.
_Avoid_: using "pane" to mean only the runtime process, or only the state struct — be explicit
which layer you mean when it matters.

**Space**:
The git/worktree identity a workspace is derived from — repo root, checkout key, branch, and
whether it's a linked worktree checkout. The sidebar's top section (and its config, `[ui.sidebar.spaces]`)
is named after this concept, showing a per-workspace summary row (name, branch, git status).
_Avoid_: as a synonym for "workspace" — a Space is a workspace's derived git identity/metadata,
not the container itself.

**Sidebar**:
The TUI's persistent navigation panel, split into a spaces section (per-workspace summary rows)
and an agent panel section (per-pane/per-agent detail rows). Purely a client-presentation concept —
sidebar layout, sorting, and token placement are never shared server state.
_Avoid_: "nav", "panel" alone (ambiguous with "agent panel", one of its two sections).

### Agent detection

**Agent detection**:
The mechanism that classifies a pane's foreground agent process by reading a live snapshot of the
bottom of its terminal buffer (never the parser, never the user-scrollable viewport) and evaluating
it against a manifest. For some integrations, detection can instead be authoritative via a
lifecycle hook, bypassing manifest matching entirely.
_Avoid_: "screen scraping" or "polling" — those describe implementation, not the domain concept.

**Manifest**:
A per-agent TOML rule file (`src/detect/manifests/<agent>.toml`) describing how to recognize that
agent's idle, working, and blocked screen chrome from visible text and optional OSC sequences. A
manifest is sourced as bundled (shipped with Herdr), remote (fetched from herdr.dev), or a local
override.
_Avoid_: "rule file" alone once "manifest" is established — manifest is the canonical term.

**Gate**:
A boolean matching condition inside a manifest rule, composed from `all` (AND), `any` (OR), and
`not` (negation) of leaf matchers (`contains`, `regex`, `line_regex`). Gates nest recursively to
express "this text AND NOT that text" style detection logic.
_Avoid_: "condition", "predicate" — gate is the manifest vocabulary term.

**Agent state**:
The core 4-value classification a detection produces: `Idle`, `Working`, `Blocked`, or `Unknown`.
_Avoid_: conflating this with the 5-value set shown in user docs and the sidebar, which adds
`Done` — see next entry.

**Done**:
A derived, sidebar-facing label — not an `AgentState` variant — meaning the agent reached `Idle`
while the user was looking at a different workspace (`PaneState.seen == false`). Once the user
views the pane, the label reverts to plain `Idle`.
_Avoid_: treating "Done" as a fifth backend state; it's a presentation-layer derivation of Idle.

**Presentation**:
Confirmed live (not deprecated): agent-reported display metadata layered on top of `AgentState` —
a title, a display name, and per-state text labels an agent/integration can report about itself,
used when rendering pane and sidebar chrome.
_Avoid_: none — but note this is unrelated to the plain-English "presentation" used elsewhere in
config doc comments (e.g. "collapsed sidebar presentation").

**Native agent session**:
An agent-owned conversation/session identity (e.g. from an OpenCode or Claude session) that Herdr
tracks separately from its own `Session` server namespace, used to resume the agent's own
conversation across restarts when `session.resume_agents_on_restore` is enabled.
_Avoid_: "session" unqualified — always distinguish from a Herdr server [Session].

### Sockets and API

**Server socket**:
The JSON socket API (env `HERDR_SOCKET_PATH`) that `herdr api ...` commands, scripts, and agents
use to query and drive server state — session snapshots, workspace/tab/pane operations, events.
_Avoid_: "the API" unqualified when a client-socket contrast matters.

**Client socket**:
The private wire protocol socket (env `HERDR_CLIENT_SOCKET_PATH`) between a TUI client and the
server — keystrokes, render frames, and other client-only traffic. Not intended for external
scripting; the server socket is the supported integration surface.
_Avoid_: "the socket" unqualified — always distinguish from the [Server socket].

**Event**:
A dot-named domain notification of a server state change (e.g. `workspace.created`,
`pane.agent_status_changed`, `worktree.created`), delivered over socket subscriptions to notify
clients and integrations without polling.
_Avoid_: "message" alone — reserve for lower-level wire framing, not domain events.
