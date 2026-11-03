# Shepherd as a fork of herdr

Decision of the owner (2026-11-03): **Shepherd is a fork of herdr**, "a herdr with more features", not a separate program that calls herdr. This replaces the earlier idea of an external `shepherd-core` + adapter (kept as library code to be ported in).

Name (owner, 2026-11-03): **shepherd**, for the product and the binary. Credit to herdr stays in README and NOTICE. The Cargo package/binary rename is done in F5 (not earlier, to keep upstream rebases cheap); until then the binary is still built as `herdr`.

Base: `herdrdev/herdr` 0.9.5 (Apache-2.0), the terminal UI. The graphical `herdr-app` is not in that repository, so it is not part of this fork. Credit to herdr stays in the README and NOTICE; no upstream issues, PRs or pushes (the upstream `AGENTS.md` forbids them for forks). The `upstream` remote has its push URL disabled on purpose.

## Product

herdr's runtime (PTYs, sessions, workspaces, panes, agent detection, API server, mouse-first TUI) stays as is, **including watching and talking to an agent directly in its native terminal pane**, which the owner prefers over any view of our own. Shepherd adds, in-process:

- roles, vendors and routing (soft `must_differ_from`, single-vendor mode), limits, the agent tree;
- task context, plans, the plan > code > review > PR workflow with human gates (push and PR always wait for a person);
- an MCP server so agents talk to each other (13 tools);
- skills (global, role, per-session) injected per session, never written into the user's repo;
- a git tree/worktree/PR view and safe branch operations;
- panels for all of that inside the herdr TUI, and the same functions as `shepherd` CLI subcommands over herdr's API socket (headless use).

## Phases

| Phase | What | Needs |
|---|---|---|
| F0 | Baseline: build and test upstream unchanged in this clone | Rust 1.96.1 (installed: 1.93.1) and Zig (for `ghostty-vt`): **install needs the owner's OK** |
| F1 | Add `crates/shepherd-core` (the R1 library: tasks, plans, registry, bus, skills, MCP, workflow) as a workspace member; replace its `Terminals` adapters by an in-process one over herdr's `AppState`/`PaneRuntime` | F0 |
| F2 | Orchestration services in the app runtime: spawn into worktrees, reconcile, first-prompt delivery with verification, resume after restart, "needs you" notices, clean-up, MCP server | F1 |
| F3 | UI: agents tree, tasks, runs, skills and git panels in the TUI, following herdr's UI patterns (mouse-first, no god objects) | F2 |
| F4 | API methods and `shepherd` CLI subcommands; JSON output | F2 |
| F5 | Branding (the "herdr" name is not licensed by Apache-2.0), README credit, NOTICE, docs, an upstream rebase routine | owner decision on name |

## Rules inherited from upstream (they apply to forks)

State is separate from runtime (`AppState` is pure data); render is pure; no god objects; platform code isolated under `src/platform/`; render/layout loops are performance-critical (profile with `just bench-render-scale` when touching them). Read the upstream `AGENTS.md` before any change.

## Carried over from the Go and R1 work (requirements, not suggestions)

- A prompt accepted by a terminal can be lost while a CLI starts: verify the agent reacted, resend up to 3 times.
- A CLI sitting on a trust/login dialog never accepts its first prompt: mark it "needs the human" and **never answer the dialog for them**.
- Agent ids and the spawn counter never go backwards; names are unique among open panes (reclaim only shepherd's own leftovers).
- `done` = finished its turn, pane alive; only `exited`, `killed`, `failed` are final.
- Editing a skill's text reaches a running agent; adding or removing one needs a restart.
- Push and PR always wait for an explicit human approval unless `--auto-push` for that run.

## Archivos de herdr modificados

Para mantener los rebases desde `herdrdev/herdr` baratos, todo el código nuevo vive en `crates/shepherd-core/` y en el módulo aislado `src/shepherd/`. Los archivos originales de `herdr` solo contienen líneas mínimas de enganche:

| Archivo | Línea(s) de enganche | Por qué |
|---|---|---|
| `Cargo.toml` | L29 (`shepherd-core = { path = "crates/shepherd-core" }`) | Enlazar el crate del workspace `shepherd-core` dentro del binario `herdr`. |
| `src/main.rs` | L55 (`mod shepherd;`) | Declarar el módulo `src/shepherd/` en el binario principal sin acoplar otros módulos. |
| `src/cli.rs` | L103-L105 (`if command == "shepherd" { return crate::shepherd::run_cli(&args[1..]).map(CommandOutcome::Handled); }`) | Delegar el subcomando `herdr shepherd <...>` a `crate::shepherd::run_cli` antes del despacho normal de CLI de `herdr`. |
| `src/app/state.rs` | L809 y L1053 (`pub shepherd_agents_panel: crate::shepherd::ShepherdAgentsPanelState`) | Guardar el estado puro (`AppState`) del panel de solo lectura de agentes de Shepherd (en producción y en `AppState::test_new()`), cumpliendo la separación entre estado y runtime. |
| `src/app/mod.rs` | L521 (`shepherd_agents_panel: crate::shepherd::ShepherdAgentsPanelState::default()`) | Inicializar `shepherd_agents_panel` al construir `AppState` en `App::try_new`. |

## Estado F1

### Lo hecho

1. **Adaptador en proceso para `trait Terminals` (`HerdrInProcess`)**:
   - En `crates/shepherd-core/src/herdr.rs`: `HerdrClient::in_process(handler)` permite despachar cualquier método de la API de `herdr` mediante un cierre en memoria sin abrir ningún socket Unix (`is_in_process() == true`).
   - En `crates/shepherd-core/src/terminals.rs`: `HerdrInProcess` implementa las 10 operaciones de `trait Terminals` (`open_agent`, `prompt`, `send_text`, `send_keys`, `read_screen`, `wait`, `agent_status`, `label`, `close`, `attach`), junto con `BackendKind::InProcess` (`backend = "in-process"`) y el registro `set_in_process_terminals_factory`.
   - En `src/shepherd/mod.rs`:
     - `in_process_from_app_channel(api_tx: ApiRequestSender, event_hub: EventHub, session)`: conecta `HerdrInProcess` directamente al canal interno de peticiones del `App` (`ApiRequestMessage` + `EventHub` para `agent.wait` sobre `EventKind::PaneAgentStatusChanged`) y `dispatch_api_message_on_app` enruta `Method::AgentPrompt`, `Method::WorktreeCreate` y el resto de métodos igual que `HeadlessServer`.
     - `in_process_from_api_client(client: ApiClient, session)` y `try_connect_local_in_process(session)`: conectan `HerdrInProcess` al `src/api/client.rs::ApiClient` tipado de `herdr` cuando `herdr shepherd ...` se ejecuta desde un proceso CLI separado contra un servidor local en marcha.
   - **Nota arquitectónica sobre `App` vs `src/api/wait.rs` en `herdr`**:
     - En `herdr`, `App::handle_api_request_after_internal_events_drained` atiende de forma síncrona la mayoría de métodos (`session.snapshot`, `workspace.create`, `pane.split`, `pane.send_text`, `pane.send_keys`, `pane.read`, `pane.report_metadata`, `pane.rename`, `pane.close`, `agent.start`, `agent.get`, `agent.list`, etc.).
     - Sin embargo, tres operaciones bloqueantes o diferidas (`Method::AgentPrompt` con espera, `Method::AgentWait` y `Method::EventsWait`/`PaneWaitForOutput`) son coordinadas fuera del bucle principal de `App` por `src/api/wait.rs` + `EventHub` y los métodos diferidos `App::handle_deferred_agent_api_request` / `App::handle_deferred_worktree_api_request`. Por ello, `in_process_from_app_channel` resuelve `Method::AgentWait` suscribiéndose en memoria a `EventHub` (sin tocar sockets) y `dispatch_api_message_on_app` respeta las rutas diferidas de `App`. Unificar `src/api/wait.rs` dentro del propio `App` requeriría refactorizar el bucle de eventos de `src/server.rs`, lo cual se evitó en F1 para mantener cambios mínimos en `herdr`.
2. **Subcomando integrado (`herdr shepherd ...`)**:
   - Extraído `crates/shepherd-core/src/cli.rs` con `run_cli`, `run_cli_with_terminals` y `run_cli_in_root` (reexportados en `crates/shepherd-core/src/lib.rs` y usados por `crates/shepherd-core/src/main.rs`).
   - Añadido en `crates/shepherd-core/src/mcp.rs` `serve_with_terminals` para que `herdr shepherd mcp-serve` inyecte el `Arc<dyn Terminals>` en proceso si hay un servidor alcanzable.
   - El binario `herdr` expone `herdr shepherd agents --json`, `herdr shepherd task list --json`, `herdr shepherd roles list --json`, etc., delegando en `shepherd_core` e inyectando `HerdrInProcess` automáticamente cuando detecta un servidor local de la sesión activa. El binario conserva el nombre `herdr` (el renombrado a `shepherd` corresponde a F5).
3. **Panel de agentes de solo lectura en el TUI (`src/shepherd/panel.rs`)**:
   - `ShepherdAgentsPanelState` y `ShepherdAgentRow` almacenan datos puros en `AppState` (id, rol, vendor/modelo, estado, pane nativo de `herdr` y aviso `"NEEDS YOU"` cuando el agente está bloqueado o tiene `notice`).
   - `render_shepherd_agents_from_app` y `render_shepherd_agents_panel` son funciones de render puro (`&AppState` -> `ratatui::buffer::Buffer`) que reutilizan los helpers de modales de `src/ui/` (`centered_popup_rect`, `modal_stack_areas`, `release_notes_close_button_rect`, `release_notes_scrollbar_rect`) sin emular terminales ni tocar la vista nativa de cada pane.

### Lo no verificado

- No se ejecutó el binario compilado contra la sesión real de `herdr` del usuario ni contra sus sockets/terminales (prohibido expresamente por las reglas de F1). Toda la verificación de `HerdrInProcess`, `dispatch_api_message_on_app`, `EventHub`, `herdr shepherd ... --json` y el renderizado del panel TUI se realizó mediante tests unitarios y de integración en memoria sin lanzar procesos de agentes reales.
- El test preexistente `app::api::plugins::tests::manifest_action_invoke_injects_plugin_paths` sigue documentado como intermitente conocido cuando corre dentro de la suite completa (pasa en solitario).

### Qué queda para F2

- Conectar los servicios de orquestación en el bucle de ejecución del runtime (`spawn` en worktrees, reconciliación periódica de estado `AppState`/`AgentRegistryFile`, entrega del primer prompt con verificación y hasta 3 reintentos, detección de diálogos de trust/login para marcar `"needs-human"` sin responderlos nunca, reanudación tras reinicio, limpieza y arranque integrado del servidor MCP dentro del proceso del servidor).
- Conectar un atajo/acción de entrada en `src/app/input.rs` y el bucle de renderizado de cliente en F3 para abrir/cerrar interactivamente `shepherd_agents_panel` y refrescarlo automáticamente desde los eventos del runtime.
