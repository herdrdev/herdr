# Unified fleet sessions — one client, every machine

Status: **design, approved direction, unbuilt.** Agreed with Matthias on 2026-10-15 in
chat; the companion dashboard with the sidebar mock and latency budget is
`herdr-unified-sidebar` on the tailnet. Measurements below were taken on ub1 the same day.

## Goal

One Herdr client shows every session on every machine in the local network as if it
were local. Three properties define "native":

1. **Status freshness.** A remote agent's state change (running, waiting, blocked, done)
   appears in the sidebar within the same window as a local one.
2. **Open latency.** Opening a remote pane shows no dial, probe or spinner.
3. **Host is an attribute, not a structure.** Nothing in the sidebar groups by host; the
   host appears as a badge and a filter.

Budget: a remote action completes within **2× the local figure**. Parity is the aim;
2× is the ceiling, and 2× already reads as slow.

Out of scope: spawn placement (exists, `spawn-on-host`), a separate travel client
(Matthias remote-logs into the Mac's or ub1/ub2's own Herdr client when travelling and
accepts WAN latency on that hop), process migration between servers.

## Measured basis (ub1 → ub2, 2026-10-15)

| Measure | Value | Consequence |
| --- | --- | --- |
| LAN ping, avg rtt | 0.08 ms | network is not the cost |
| Tailscale ping, direct peer, avg rtt | 0.45 ms | Tailscale addressing is free on the LAN |
| `ssh ub2 true`, no ControlMaster | 240 ms | the entire remote penalty is the handshake |
| `ssh ub2 true`, warm ControlMaster socket, ×3 | < 10 ms | one warm connection per host closes the gap |
| `[remote.fleet].refresh_interval_ms` default | 5 000 ms | status lag today is poll interval + handshake |
| Hetzner | not a tailnet peer; reached via the `he` ssh alias over WAN | WAN rtt, not relaying, is its cost |

## What exists

- `[[remote.fleet.hosts]]` with a Hosts dock surface and per-host sidebar groups
  (`src/fleet.rs`, `src/app/machines.rs`, `src/app/creation.rs`).
- Inventory refresh: `fleet.rs` routes each API request through a fresh `ssh` exec with
  a 5 s timeout (`route_api_request_with_ssh_program`, `run_ssh_program_with_timeout`),
  once per host per tick. Local hosts read the API socket directly.
- Pane open: direct terminal attach over SSH per click (`herdr agent attach` semantics);
  a second click refocuses an already attached pane (`focus_attached_fleet_host_pane`).
- Remote agent streams: alongside the poller, `supervise_remote_agent_streams`
  (`src/fleet.rs`) runs one long-lived `ssh` process per remote host that subscribes
  to `FleetAgentsChanged` and, when the server supports it, `SessionChanged`, seeding
  session inventory from the stream. The worker retries 3 s after a stream ends, 10 s
  after a failed capability probe, and 30 s when the host offers neither event. The
  poller keeps running at its configured interval (default 5 s) the whole time,
  including while a stream reconnects.
- `herdr --remote` keeps a private per-attach SSH control socket; saved fleet hosts do
  not.

The architecture is right. The data path is one cold `ssh` per poll request, a
separate `ssh` per event stream, and per-open dialling.

## Design

### 1. One warm channel per host

A per-host connection supervisor owns one multiplexed SSH connection (OpenSSH
`ControlMaster=auto`, `ControlPersist`, a private control socket under the Herdr state
directory, keepalive from the existing generated SSH config). Every remote operation for
that host — inventory, event subscription, pane attach, dispatch — goes through it.

- Reconnect with exponential backoff (250 ms → 10 s cap), silent in the UI below 3 s,
  then a host badge turns stale.
- Binary compatibility probe runs once per connection, not once per open.
- Hosts declare `target`; the recommended target is the Tailscale MagicDNS name so the
  same config works at home and when travelling. On the LAN the peer path is direct, so
  this costs under half a millisecond.
- Windows clients fall back to the current per-exec path (Windows OpenSSH has no
  control sockets); behaviour there is unchanged.

### 2. Event stream over the warm channel; poll as fallback

The per-host event stream already exists (see "What exists"). This step moves it onto
the warm channel instead of its own `ssh` process, aligns its reconnect with the
supervisor's backoff (250 ms → 10 s instead of today's fixed per-case delays), and demotes the
5 s poll to a slow fallback (60 s) that runs only while the stream is absent or reports
it fell behind. It also checks that the stream's subscriptions cover every state the
sidebar shows, adding pane lifecycle coverage only if a gap is found.

This is a client change only; the server API exists. Freshness stays at event latency
plus one warm round trip, roughly 5–20 ms, and stops depending on a cold reconnect.

### 3. Pane open path

- Attach streams are opened through the warm channel: no handshake, no probe.
- The last rendered snapshot of every remote pane that has ever been attached is cached
  client-side and painted immediately while the live stream catches up.
- Pre-warm (keep the attach stream open before the first click) is **off by default
  for LAN hosts**; the measured warm path lands inside the budget without it. It is **on
  for hosts flagged `wan = true`** (Hetzner), where one WAN rtt already approaches the
  ceiling on first open. Scope when on: panes in the active workspace plus the last ten
  focused. Flagged default; flip in config.

### 4. Sidebar

Grouping order becomes **state → project → agent**, across all hosts. Host becomes a
two-letter badge on the row (`u1`, `u2`, `mb`, `he`) that also acts as a filter chip;
the `Local ▾` selector in the Blockers header becomes `all hosts ▾`. Collapsed
per-device rows (`hetzner (2) · mbpro (25) · ub2 (5)`) go away. Sorting within a project
is by activity age regardless of host. Flagged default: badge rather than full host
name, to keep rows single-line at the current sidebar width.

### 5. Latency budget per action

| Action | Local today | Remote today | Remote after | Within 2× |
| --- | --- | --- | --- | --- |
| Status change visible | ~50 ms | stream event plus a cold-ssh stream; while a stream is down, the next poll (0–5 000 ms + 240 ms) | ~5–20 ms over the warm channel | yes |
| Open pane, first time | ~30 ms | ~400–700 ms | ~30–60 ms | yes |
| Open pane, already attached | ~30 ms | ~30 ms | ~30 ms | yes (already) |
| Keystroke echo | < 1 ms | ~1–2 ms | ~1–2 ms | yes (unchanged) |
| Hetzner, same actions | — | + WAN rtt | + WAN rtt, pre-warmed | status yes; first open borderline |
| Reconnect after network move | — | manual / retry | 1–3 s silent | n/a |

Local figures are estimates of in-process UI latency; remote-today figures compose the
measured handshake with the documented poll interval; remote-after figures assume the
warm-channel measurement.

## Build order

1. Connection supervisor + warm channel, used first by the existing poller (no behaviour
   change, handshake disappears). Measurable on its own: poll tick cost 240 ms → < 10 ms.
2. Existing event stream moved onto the channel with supervisor backoff; poll demoted to
   a 60 s fallback.
3. Attach over the channel; snapshot cache; `wan` flag and pre-warm.
4. Sidebar regrouping and host badge/filter.
5. Config migration note: recommend Tailscale names for `target`; no breaking change.

Each step ships separately and is verified by the per-action budget table above, measured
from mbpro against ub1 and ub2 with `herdr fleet status --watch` timestamps and a pane
open stopwatch.

## Open points (flagged defaults)

- Pre-warm default off for LAN hosts, on for `wan = true`.
- Two-letter badge rather than full host name.
- Whether Hetzner joins the tailnet; optional, the warm channel matters more there than
  anywhere.
