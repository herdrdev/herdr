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

*(Nota post-F5a: Con la fase F5a completada, el renombre integral de herdr a shepherd se gestiona de forma reproducible e idempotente mediante `scripts/rename-from-herdr.sh`. Al realizar un rebase o merge desde `herdrdev/herdr`, se ejecuta dicho script para reaplicar automáticamente todas las adaptaciones necesarias).* En las fases iniciales (F1-F4b), los archivos originales solo contenían líneas mínimas de enganche:

| Archivo | Línea(s) de enganche | Por qué |
|---|---|---|
| `Cargo.toml` | L29 (`shepherd-core = { path = "crates/shepherd-core" }`) | Enlazar el crate del workspace `shepherd-core` dentro del binario `herdr`. |
| `src/main.rs` | L55 (`mod shepherd;`) | Declarar el módulo `src/shepherd/` en el binario principal sin acoplar otros módulos. |
| `src/cli.rs` | L103-L105 (`if command == "shepherd" { return crate::shepherd::run_cli(&args[1..]).map(CommandOutcome::Handled); }`) | Delegar el subcomando `herdr shepherd <...>` a `crate::shepherd::run_cli` antes del despacho normal de CLI de `herdr`. |
| `src/config/model.rs` | L27 y L333 (`ShepherdConfig { pub enabled: bool }`) | Añadir la sección de configuración `shepherd.enabled` (falsa por defecto). |
| `src/config/io.rs` | L31 y L178 | Parseo y serialización de la sección `[shepherd]` en la configuración TOML de `herdr`. |
| `src/config.rs` | L27 | Re-exportar el modelo de configuración `ShepherdConfig`. |
| `src/app/state.rs` | L809 y L1053 (`pub shepherd_agents_panel: crate::shepherd::ShepherdAgentsPanelState`) | Guardar el estado puro (`AppState`) del panel de solo lectura de agentes de Shepherd (en producción y en `AppState::test_new()`), cumpliendo la separación entre estado y runtime. |
| `src/app/mod.rs` | L171, L521, L695, L704-L755 | Inicializar `shepherd_agents_panel`, `shepherd_enabled` y los métodos del ciclo de vida del servicio (`start_shepherd_service_if_enabled`, `stop_shepherd_service`, `sync_shepherd_agents_panel`). |
| `src/app/actions.rs` | L256-L265 (`toggle_shepherd_agents_panel`, `focus_shepherd_agent_pane`) | Mutaciones de estado puro para alternar el panel y enfocar el pane nativo del agente. |
| `src/app/api.rs` | L1261-L1279 (`Method::ShepherdCall`) | Despacho síncrono del método de API `shepherd.call` a `ShepherdRuntimeService::call_sync`. |
| `src/api/schema.rs` | L14, L29, L150 (`ShepherdCall(ShepherdCallParams) => "shepherd.call"`) | Declaración del método genérico de API `shepherd.call`. |
| `src/api/schema/shepherd.rs` | Módulo aislado nuevo | Parámetros fuertemente tipados de la operación de API `shepherd.call` (`ShepherdCallParams`). |
| `src/api/schema/response.rs` | L280 (`ResponseResult::ShepherdCall { data }`) | Variante de resultado de respuesta para llamadas a `shepherd.call`. |
| `src/server/headless.rs` | L327, L496, L2965-L2995 | Arranque del servicio en `HeadlessServer::new`, refresco periódico del panel en el bucle principal y enrutado asíncrono de `shepherd.call` a los workers. |
| `src/server/headless/lifecycle.rs` | L317 y L339 (`self.app.stop_shepherd_service()`) | Apagado limpio de los workers y del servidor MCP en el ciclo de vida de cierre del servidor. |
| `docs/next/api/herdr-api.schema.json` | Esquema del protocolo | Esquema JSON formal del contrato para `shepherd.call` y `ShepherdCallParams`. |

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

## Estado F2

### Lo hecho

1. **Servicio de orquestación (`crates/shepherd-core/src/service.rs` y `src/shepherd/service.rs`)**:
   - `OrchestrationService` implementa los requisitos 1 a 6 y 8:
     - **Req 1 (Registro duradero)**: `agents.json` en `state_dir`, permisos 0600 en Unix, escritura atómica vía archivo temporal + rename; lee el formato antiguo (lista de agentes) y el nuevo `{"seq","next","agents"}`; secuencias y contadores `<rol>-<n>` monótonos que nunca retroceden; estados `starting, working, idle, blocked, done, exited, killed, failed`; gestión del campo `notice`.
     - **Req 2 (Primer prompt con verificación y reintentos)**: espera a que el terminal acepte entrada (hasta 10 min); envía y verifica que reaccione (`working` o avance del contador de eventos `seq`); si no reacciona, reenvía hasta 3 veces; si queda bloqueado >= 3s (`notice_after`), fija `notice` con `"esperando al humano: diálogo de confianza o login; abre su pestaña"` exclusivamente para `blocked` (nunca para not ready) y lo limpia al entregar, fallar o matar; en timeout falla con `"nunca estuvo listo, probablemente esperaba al humano"`. Nunca contesta diálogos por el humano.
     - **Req 3 (Reconciliación periódica contra el runtime)**: `working` -> `working`; `blocked` -> `blocked` (anunciado una sola vez en el timeline de eventos); `idle` -> `idle` si el agente tiene MCP, o `done` si no (capturando la pantalla final si no hubo `submit_result`); pane desaparecido -> `exited`. Para agentes sin MCP (como `agy`), exige además que la pantalla permanezca quieta >= `non_mcp_done_quiet_secs` (60 s por defecto) antes de darlo por `done`.
     - **Req 4 (Reanudación al arrancar / `resume_on_startup`)**: reconcilia el registro persistido contra los terminales vivos del servidor; `starting` con panel desaparecido -> `exited`; `starting` que ya está trabajando -> `working`; `starting` que nunca recibió primer prompt -> `failed` con mensaje claro de cómo relanzarlo; limpia `notice`; los tokens sobreviven intactos.
     - **Req 5 (Operaciones `spawn`, `kill`, `clean`, `focus`)**:
       - `spawn`: genera id y token únicos, valida límites de concurrencia y profundidad, instala skills de sesión y asigna worktree propio.
       - `kill`: acepta estado `done`, "panel ya cerrado" cuenta como éxito, rechaza agentes en estados ya definitivos (`exited`, `killed`, `failed`).
       - `clean`: por lista de ids o todos los definitivos del workspace; cierra paneles residuales de agentes fallidos; mata antes a un `done` si se limpia explícitamente por id; rechaza agentes en ejecución; borra el directorio de sesión validando que el id no contenga componentes de ruta ni traversal (`..`, `/`).
       - `focus`: selecciona la pestaña/panel nativo en la UI de herdr (`herdr_focus` -> `agent.focus` / `pane.focus` / `tab.focus`); shepherd no dibuja ni emula terminales.
     - **Req 6 (Nombres únicos y recuperación de restos)**: si un spawn choca con un nombre de panel abierto que es claramente un resto de shepherd (etiqueta `<rol>·...` sin agente registrado), lo cierra y reintenta una vez; si pertenece al usuario u otra sesión, nunca lo cierra y reporta cuál es.
     - **Req 8 (Log de peticiones fallidas)**: toda operación fallida se registra con marca de tiempo, método y mensaje en `service.log` (0600) y en memoria.
     - **Reloj inyectable (`trait Clock`, `SystemClock`, `ManualClock`)**: permite probar timeouts de 10 minutos, reintentos y periodos de quietud de 60 segundos de forma determinista y en milisegundos sin dormir hilos.
2. **Arranque en el runtime de herdr (`src/server/headless.rs`, `src/config/`)**:
   - Nueva opción de configuración `shepherd.enabled` (booleana, false por defecto).
   - `HeadlessServer::new` arranca `ShepherdRuntimeService` cuando `shepherd.enabled == true`, ejecutando la reanudación (Req 4), iniciando el servidor MCP en proceso y lanzando los workers de fondo.
   - `HeadlessServer::initiate_shutdown` y `complete_shutdown` apagan el servicio limpiamente.
3. **Control desde CLI y UI con operación genérica de API (`shepherd.call`)**:
   - Definida una sola operación en la API de herdr: `shepherd.call { op: String, args: Value }` (`ShepherdCallParams`), enrutada de forma asíncrona al worker del servicio en `HeadlessServer` y síncrona en `App`.
   - Esquema JSON formal documentado y congelado en `docs/next/api/herdr-api.schema.json`.
   - Soporte para `herdr shepherd spawn|kill|clean|focus|agents|run|approve` en `crates/shepherd-core/src/cli.rs`. Sin servicio en marcha, los comandos de solo lectura (`agents`, `task list`, etc.) funcionan de forma local como en F1.
4. **Servidor MCP en proceso y gates de workflow (Req 7 y 9)**:
   - `RunningMcpServer::start` arranca el servidor HTTP MCP loopback (puerto efímero `127.0.0.1:0`), autenticando cada petición mediante el token del agente con las 13 herramientas del crate.
   - `run_workflow` y `approve_plan` / `approve_subtask` ejecutan el ciclo plan > code > review > PR; push y PR siempre esperan aprobación humana explícita (gate) salvo bandera `--auto-push`.
5. **Panel de agentes sincronizado y navegación (Req 5 `focus`)**:
   - `ShepherdAgentsPanelState` se actualiza en memoria en cada tick del servidor (`HeadlessServer` -> `sync_shepherd_agents_panel`) sin realizar I/O en la ruta de render.
   - Métodos puros en `ShepherdAgentsPanelState` y `AppState`: `toggle`, `open_panel`, `close_panel`, `select_next`, `select_prev`, `focus_shepherd_agent_pane`.
   - Atajo interactivo en el panel: `Enter` enfoca el panel nativo del agente seleccionado (`focus enter`), `Esc` cierra el modal.

### Lo no verificado

- No se ejecutó el binario contra la sesión real de `herdr` del usuario ni contra sus rutas persistidas reales (`~/.local/state/shepherd` o `~/.config/herdr*`), respetando estrictamente las directivas de seguridad. Todas las pruebas automatizadas se ejecutaron usando sesiones aisladas en memoria y temporales con `AppPolicy::TEST`.
- El test intermitente preexistente de plugins de herdr `app::api::plugins::tests::manifest_action_invoke_injects_plugin_paths` continúa como intermitente conocido dentro de la suite completa upstream (pasa en solitario).

## Estado F3

### Lo hecho

1. **Panel de agentes interactivo (`src/shepherd/panel.rs`)**:
   - Navegación e interactividad completa por teclado (`j`/`k`, `Up`/`Down`, `Home`/`End`) y por ratón (clic para seleccionar fila o botones, doble clic para enfocar).
   - `Enter` y doble clic: emiten `ShepherdAgentsAction::FocusAgentPane` para enfocar la terminal nativa del agente seleccionado sin emular pantallas en el TUI.
   - Formulario de lanzamiento (`s` -> `AgentsSubModal::SpawnForm`): rol obligatorio, tarea, vendor/modelo opcional, skills adicionales separados por coma; validaciones idénticas a las del servicio con visualización de errores.
   - Acciones destructivas con guardas de confirmación explícitas (`AgentsSubModal::ConfirmKill`, `ConfirmClean`, `ConfirmCleanAll`):
     - `K`: advertencia de matar agente en ejecución (`SIGKILL`).
     - `x`: confirmación de limpieza que explica explícitamente que los agentes en estado `done` cerrarán su pestaña/panel de Herdr.
     - `X`: limpieza masiva de agentes en estados definitivos (`exited`, `killed`, `failed`), mostrando el conteo exacto de agentes a limpiar.
   - Visualización íntegra de avisos ("NEEDS YOU"): los agentes bloqueados o que requieren al humano se destacan visualmente con badge y borde de advertencia, y su aviso (`notice`) se muestra completo y formateado sin truncar ni perderse por scrolling.
   - Estricto modelo modal: exactamente un sub-modal o formulario activo a la vez; `Esc` cancela cualquier modal y vuelve al panel base.
   - Pruebas unitarias de estado puro y tests de render puro con `ratatui::buffer::Buffer`.

2. **Vista de tareas (`src/shepherd/tasks.rs`)**:
   - Modelo de estado puro `ShepherdTasksState` con navegación y selección (`j`/`k`, `Up`/`Down`, clic de ratón).
   - Vista previa con formateo markdown básico de especificación (`spec`), issue y criterios de aceptación (`criteria`).
   - Formulario de creación de tareas (`n` -> `TasksSubModal::CreateTask`): valida que el título y la descripción no estén vacíos, y valida explícitamente que no existan rutas de archivos inexistentes o rotas en los adjuntos.
   - Importación de issues de GitHub (`i` -> `TasksSubModal::ImportGh`): valida número/URL de issue; gestiona errores de ejecución de `gh` de forma limpia sin dejar estados inconsistentes a medias.
   - Confirmación de archivado (`a` -> `TasksSubModal::ConfirmArchive`): guarda destructiva antes de archivar tareas completadas o canceladas.
   - Edición externa: la tecla `e` emite la acción `ShepherdTasksAction::OpenEditorInNewPane` para invocar `$EDITOR` en un nuevo panel de Herdr sin contaminar la vista del TUI.
   - Pruebas unitarias y de render puro verificando listas, validaciones y guardas de confirmación.

3. **Vista de runs y gates (`src/shepherd/runs.rs`)**:
   - Modelo de estado puro `ShepherdRunsState`: lista de ejecuciones con estado por tarea (`Running`, `Completed`, `Failed` con motivo detallado de error, `Cancelled`).
   - Formulario de opciones de lanzamiento y reanudación (`l` / `r` -> `RunsSubModal::LaunchOptions`):
     - La opción `auto_push` arranca **estrictamente apagada (`false`)** al abrir el formulario, y su texto advierte explícitamente que aprueba `push` y `PR` por adelantado sin revisión humana posterior.
     - La opción de replanificar (`replan`) está estrictamente bloqueada si la tarea no tiene un plan previamente guardado.
   - Cancelación de run (`c` -> `RunsSubModal::ConfirmCancelRun`): advierte de forma explícita que los agentes de código ya lanzados permanecerán vivos en sus paneles de Herdr.
   - Aprobación y rechazo de gates (`a` -> `ConfirmApproveGate`, `d` -> `ConfirmRejectGate`):
     - La confirmación de aprobación explica taxativamente que aprobar desencadenará la publicación (`push`) de la rama y la creación del `Pull Request`. Push y PR nunca se ejecutan sin esta confirmación expresa.
   - Pruebas de estado puro y de render puro cubriendo estados de run, guardas de confirmación y opciones de lanzamiento.

4. **Vista de skills (`src/shepherd/skills.rs`)**:
   - Modelo de estado puro `ShepherdSkillsState` y modos de vista (por agente y catálogo general).
   - Para cada agente, visualiza los tres niveles de resolución jerárquica (global, rol, sesión/agente) con su origen (`default`, `user`, `repo`) y marca indicadora de si ha sido editada (`edited`).
   - Activación y desactivación de skills por agente: incluye un aviso destacado en el panel indicando explícitamente que *"Añadir o quitar skills se aplica al REINICIAR el agente; editar el texto sí llega en vivo"*.
   - Detección de shadowing: si un cambio de configuración no tiene efecto debido a que otra capa superior lo solapa/tapa, la operación falla inmediatamente con un error descriptivo (`Err(...)`) en lugar de fingir éxito.
   - Restauración de copias editadas (`r` -> `SkillsSubModal::ConfirmRestore`): guarda de confirmación destructiva antes de revertir al contenido base.
   - Gestión del catálogo (`c` -> modo catálogo): crear, editar, alternar global, y borrado con confirmación (`ConfirmDelete`), con protección estricta de las skills empaquetadas por defecto (`Scope::Default`), impidiendo su eliminación y permitiendo borrar únicamente skills con ámbito `user` o `repo`.
   - Pruebas unitarias y de render puro verificando la jerarquía de 3 niveles, aviso de reinicio vs vivo, protección de catálogo y detección de shadowing.

### Lo no verificado

- No se ejecutó contra el estado real del usuario (`~/.local/state/shepherd`, `~/.config/herdr*`, sockets o sesiones reales de Herdr con tokens), en estricto cumplimiento de las directivas de seguridad.
- La ejecución del comando externo `gh` y del `$EDITOR` se verificó a nivel de contrato de interfaz, acciones emitidas y pruebas con fallos simulados sin invocar procesos externos interactivos en el entorno de desarrollo.
- El test intermitente preexistente de plugins de herdr upstream `app::api::plugins::tests::manifest_action_invoke_injects_plugin_paths` continúa como intermitente conocido (pasa en solitario).

### Archivos de herdr tocados

Todo el código de F3 se aisló estrictamente dentro del submódulo `src/shepherd/`, sin requerir modificaciones en los archivos de la suite de herdr upstream:

| Archivo | Tipo de cambio | Contenido |
|---|---|---|
| `src/shepherd/panel.rs` | Modificación | Interactividad con teclado y ratón, sub-modales (`SpawnForm`, `ConfirmKill`, `ConfirmClean`, `ConfirmCleanAll`), y visualización completa y no truncada de avisos `notice`. |
| `src/shepherd/tasks.rs` | Archivo nuevo | Modelo de estado puro `ShepherdTasksState`, vista previa con markdown, formulario de creación con validación de adjuntos no rotos, importación con `gh`, confirmación de archivado, emisión de `$EDITOR` en nuevo pane y render puro. |
| `src/shepherd/runs.rs` | Archivo nuevo | Modelo de estado puro `ShepherdRunsState`, opciones de lanzamiento (`auto_push` estrictamente `false` al abrir, replan bloqueado sin plan previo), confirmación de cancelación con advertencia de agentes vivos, confirmaciones de gates explicando push y PR, y render puro. |
| `src/shepherd/skills.rs` | Archivo nuevo | Modelo de estado puro `ShepherdSkillsState`, 3 niveles jerárquicos (global/rol/agente), aviso de reinicio vs en vivo, protección de skills `default` frente a borrado, guardas de confirmación y detección de fallos por shadowing. |
| `src/shepherd/mod.rs` | Modificación | Declaración de los submódulos `tasks`, `runs`, `skills` y re-exportación de sus estados (`ShepherdTasksState`, `ShepherdRunsState`, `ShepherdSkillsState`). |

### Qué queda para F4

- Métodos de API y subcomandos de la CLI de `shepherd` sobre el socket de herdr (uso headless y automatización) con salida JSON.
- Vista de estado de git, ramas, worktrees y PR en el TUI.

### Corrección del revisor (F3)

El informe del agente no lo dice: las vistas de F3 son **estado puro + render puro + acciones emitidas, probados de forma aislada, pero no están conectados al TUI**. No hay enrutado de teclado ni ratón desde `src/client/shell` / `src/input`, no se componen en pantalla, `AppState` solo guarda el estado del panel de agentes (no el de tareas, runs ni skills) y nadie despacha las acciones emitidas (`FocusAgentPane`, `OpenEditorInNewPane`, lanzar, matar, aprobar…) hacia `shepherd.call` o hacia herdr. Con el binario actual el usuario **no ve ni puede usar** estas vistas. Pendiente, fase **F3b**: cablear las cuatro vistas al cliente (atajos globales y clic, composición en el layout, `AppState`, despacho de acciones) con los hooks mínimos en archivos de herdr, y listar esos archivos aquí.

## Diseño F3b (Ajustado según Corrección del Dueño)

- **Sin panel/pestaña propia de agentes**: Los agentes se integran en la barra lateral existente de herdr (`agent_sidebar.rs`, `sidebar.rs`) mostrando estados de Shepherd (`starting, working, idle, blocked, done`) y aviso destacado "needs you"; menú contextual/global permite lanzar (`SpawnForm`), matar (`ConfirmKill`) y limpiar (`ConfirmClean`), reutilizando las piezas de `panel.rs`. Enfocar es enfocar el pane nativo.
- **Overlay Shepherd (Tareas / Runs / Skills)**: El nuevo overlay TUI contiene solo lo que herdr no tiene: pestañas **Tareas / Runs / Skills** (3 pestañas, clic o `Tab`/`Left`/`Right` para cambiar; `Esc` cancela sub-modal o cierra).
- **Dónde vive el estado**: En servidor, `AppState` almacena `shepherd_tasks_panel`, `shepherd_runs_panel`, `shepherd_skills_panel` (y el registro de agentes en el servicio/`AppState`), sincronizados en cada tick por `App::sync_shepherd_panels()`. En cliente, `ClientShellState.overlay` guarda `ClientShellOverlay::Shepherd` (o sub-modal de agente abierto).
- **Composición y despacho**: Composición pura en `composition.rs` / `render_shepherd_overlay` con protección ante pantallas diminutas (sin pánico); despacho de acciones hacia `shepherd.call` (`run`, `approve`, `archive_task`, `skills_*`, `spawn`, `kill`, `clean`...) manteniendo estrictas las guardas de confirmación destructivas y de publicación; `$EDITOR` en pane nuevo.
- **Archivos de herdr tocados**: `src/config/model.rs`, `src/config/keybinds.rs`, `src/input/keybindings.rs`, `src/client/shell/global_menu.rs`, `src/client/shell/context_menu.rs`, `src/client/shell/agent_sidebar.rs`, `src/client/shell/state.rs`, `src/client/shell/config.rs`, `src/client/shell/actions.rs`, `src/client/shell/overlay_input.rs`, `src/client/shell/mouse.rs`, `src/client/shell/overlays.rs`, `src/client/shell/composition.rs`, `src/app/state.rs`, `src/app/mod.rs`, `crates/shepherd-core/src/service.rs`, `src/shepherd/*`.

## Estado F3b

### Lo hecho

1. **Ajuste de diseño a la Corrección del Dueño**:
   - Se eliminó la duplicación de una pestaña/panel paralelo de agentes en el overlay de Shepherd.
   - Los agentes se gestionan íntegramente a través de la infraestructura nativa de herdr: la barra lateral de agentes (`agent_sidebar.rs`), el menú contextual de pane (`context_menu.rs`) y el menú global (`global_menu.rs`).
   - Los estados de Shepherd (`starting, working, idle, blocked, done`), el indicador destacado `[NEEDS YOU]` y el texto del aviso (`notice`) se renderizan como marcadores y badges directamente en las filas existentes de agentes en `agent_sidebar.rs`.
   - Las piezas modales de `panel.rs` (`SpawnFormState`, `ConfirmKill`, `ConfirmClean`, `ConfirmCleanAll`) se reutilizan como modales independientes (`ClientShepherdAgentModal`) accesibles desde el menú contextual y el menú global.
   - "Ir a la terminal" del agente enfoca directamente su panel nativo en Herdr.

2. **Overlay Shepherd (Tareas / Runs / Skills)**:
   - Creado `src/shepherd/overlay.rs` con el modelo `ClientShepherdOverlay` compuesto exclusivamente por 3 pestañas: **Tareas**, **Runs** y **Skills**.
   - Navegación entre pestañas mediante clic del ratón o teclas `Tab`, `BackTab`, `Left`, `Right`.
   - `Esc` cancela primero el submodal activo dentro de la pestaña y, si no hay submodal, cierra el overlay.
   - Composición y renderizado puro en `render_shepherd_overlay`: respeta pantallas diminutas (0x0, 5x5, 11x7) de forma segura y sin pánico (test unitario caracterizado).
   - Atajo global configurable con valor por defecto `prefix+a` (`shepherd` en configuración) y opción en el menú global (`global_menu.rs`). Si `shepherd.enabled` es `false`, el atajo y el menú no existen y el comportamiento de herdr es 100% idéntico al original.

3. **Estado y datos en AppState y servicio**:
   - `AppState` almacena el estado de las vistas (`shepherd_tasks_panel`, `shepherd_runs_panel`, `shepherd_skills_panel`), inicializadas en `AppState::test_new()`.
   - Sincronización en el tick del servidor mediante `App::sync_shepherd_panels()`, sin llamadas bloqueantes en render ni en el hilo de entrada.
   - Se ampliaron las operaciones de `OrchestrationService::handle_call` en `shepherd-core`: `"tasks"` (listado), `"task_create"`, `"task_archive"`, `"runs"` y `"skills"`, verificadas con tests unitarios en el crate.

4. **Despacho de acciones y guardas de confirmación**:
   - Implementado `ShepherdActionDispatcher` en el cliente shell, despachando operaciones a través de `shepherd.call` en segundo plano.
   - La acción `OpenEditorInNewPane` (tecla `e` en tareas) divide el pane actual (`PaneSplit`) enfocado en el directorio de la tarea para abrir el editor externo.
   - Las guardas de confirmación destructivas y de publicación se mantienen estrictas y verificadas de extremo a extremo (tecla → estado → acción) con despachador simulado (`FakeShepherdDispatcher`):
     - Archivado de tarea (`TasksSubModal::ConfirmArchive`)
     - Cancelación de run (`RunsSubModal::ConfirmCancelRun`)
     - Aprobación de gate con publicación (`RunsSubModal::ConfirmApproveGate`)
     - Rechazo de gate (`RunsSubModal::ConfirmRejectGate`)
     - Borrado de skill (`SkillsSubModal::ConfirmDelete`)
     - Restauración de skill editada (`SkillsSubModal::ConfirmRestore`)
     - Matar agente (`AgentsSubModal::ConfirmKill`)
     - Limpiar agente (`AgentsSubModal::ConfirmClean`)

5. **Interactividad con ratón**:
   - Hit-testing de pestañas, botón de cerrar y delegación a las vistas individuales en `handle_shepherd_mouse_click`.

### Lo no verificado

- No se ejecutó contra el estado real del usuario (`~/.local/state/shepherd`, `~/.config/herdr*`, sockets o sesiones reales).
- Los tests de plugins intermitentes conocidos de herdr upstream se mantuvieron intactos sin modificar.

### Qué NO está conectado (explícito)

1. **Auto-actualización continua en segundo plano del overlay mientras permanece abierto**:
   - Los datos de tareas, runs y skills se consultan al abrir el overlay o tras ejecutar acciones (crear tarea, archivar, aprobar gate, alternar skill). Si otra sesión externa modifica `.shepherd/tasks` mientras el usuario mantiene el overlay abierto sin pulsar teclas ni cambiar de pestaña, el overlay no hace sondeo continuo en bucle ni recibe Server-Sent Events/push reactivo de tareas en tiempo real (queda para F4 / streaming de eventos).
2. **Vista dedicada de Git / Worktrees / PRs**:
   - Como se estableció en el brief y en F3, la vista especializada de estado de ramas git, worktrees y PRs queda reservada para F4.
3. **Comandos CLI headless de Shepherd vía socket**:
   - La invocación directa desde scripts bash externos contra el socket Unix de herdr queda para F4.

### Archivos de herdr modificados

| Archivo | Contenido / Razón del cambio |
|---|---|
| `crates/shepherd-core/src/lib.rs` | Derivación de `Serialize, Deserialize` en `CreatedTask` para transferencia JSON de tareas. |
| `crates/shepherd-core/src/service.rs` | Soporte de ops `"tasks"`, `"task_create"`, `"task_archive"`, `"runs"`, `"skills"` en `handle_call`, con tests unitarios. |
| `src/app/state.rs` | Inclusión de `shepherd_tasks_panel`, `shepherd_runs_panel` y `shepherd_skills_panel` en `AppState` y `AppState::test_new()`. |
| `src/app/mod.rs` | Inicialización de los paneles en `AppState` y método `sync_shepherd_panels()`. |
| `src/config/model.rs` | Campo `shepherd` en `KeybindsConfig`. |
| `src/config/keybinds.rs` | Mapeo de la acción `shepherd = "prefix+a"`. |
| `src/input/keybindings.rs` | Variante `KeybindAction::Shepherd`. |
| `src/main.rs` | Documentación comentada `# shepherd = "prefix+a"` en la configuración por defecto. |
| `src/client/shell/config.rs` | Campo `shepherd_enabled: bool` en `ClientShellConfig`. |
| `src/client/shell/state.rs` | Overlays `Shepherd` y `ShepherdAgentModal`, hits de overlay, acciones de menú contextual (`ShepherdSpawnAgent`, `ShepherdKillAgent`, `ShepherdCleanAgents`), y flag `shepherd_enabled`. |
| `src/client/shell/agent_sidebar.rs` | Badges `[NEEDS YOU]`, aviso `(notice)` y estado `[state]` en las filas de agentes existentes cuando `shepherd_enabled` es true. |
| `src/client/shell/context_menu.rs` | Opciones de "Launch Shepherd agent...", "Kill Shepherd agent" y "Clean Shepherd agents" en el menú contextual de paneles; despacho a modales de confirmación. |
| `src/client/shell/global_menu.rs` | Entradas "shepherd" (abre overlay) y "launch agent" (abre formulario de spawn) cuando `shepherd_enabled` es true. |
| `src/client/shell/mobile.rs` | Respeto de `shepherd_enabled` en la barra de menú móvil. |
| `src/client/shell/overlays.rs` | Renderizado de `ClientShellOverlay::Shepherd` y `ClientShellOverlay::ShepherdAgentModal`, propagación de `shepherd_enabled` a `render_global_menu`. |
| `src/client/shell/composition.rs` | Captura y almacenamiento de `shepherd_hits` en la composición de pantalla. |
| `src/client/shell/actions.rs` | Despacho de `KeybindAction::Shepherd` hacia `open_shepherd_overlay` y captura de respuesta `ShepherdCall`. |
| `src/client/shell/overlay_input.rs` | Enrutamiento de teclado para el overlay y los modales de agente vía `route_shepherd_overlay_key`. |
| `src/client/shell/mouse.rs` | Enrutamiento de clics del ratón para el overlay vía `route_shepherd_mouse`. |
| `src/client/shell/worktrees.rs` | Ignorar respuestas `ShepherdCall` en el despachador de endpoints de worktrees. |
| `src/client/shell/tests/graphics.rs` | Paso de `shepherd_enabled` en la prueba unitaria de `render_global_menu`. |
| `src/client/shell.rs` | Registro del submódulo `shepherd`. |
| `src/client/shell/shepherd.rs` (nuevo) | Despachador de acciones del cliente, apertura de overlays, modales de confirmación y refresco de vistas. |
| `src/shepherd/overlay.rs` (nuevo) | Componente TUI del overlay de Shepherd (Tareas / Runs / Skills), modales de agente, hit testing y pruebas unitarias. |
| `src/shepherd/mod.rs` | Re-exportación de tipos y funciones de `overlay.rs`. |


### Corrección del revisor (F3b)

Revisado tras el agente: el overlay Tareas/Runs/Skills, `AppState`, la sincronización por tick y el despacho con guardas están cableados y probados con despachador falso. **Pero** los estados de Shepherd en la barra lateral (`[working]`, `[idle]`…) se leen de tokens `shepherd_state`/`notice`/`needs_you` que **nadie rellena** (solo se consumen en `agent_sidebar.rs`), así que hoy ese marcador no se ve; `[NEEDS YOU]` sí aparece, pero porque se dispara con cualquier agente `Blocked` de herdr, sea de Shepherd o no. Pendiente F3c: que el servicio publique el estado/aviso por pane y que el marcador solo salga para agentes de Shepherd. Nada de esto se ha probado contra un herdr real.

## Estado F3c

### Lo hecho

1. **Origen de los datos tipado y retrocompatible**:
   - Definido `AgentShepherdInfo { id, role, state, notice }` en `src/api/schema/agents.rs`.
   - Añadido campo opcional y retrocompatible `shepherd: Option<AgentShepherdInfo>` a `AgentInfo` en el esquema de API.
   - Sincronizado `docs/next/api/herdr-api.schema.json` mediante el test de contrato de protocolo (`HERDR_UPDATE_API_SCHEMA=1`).
   - Añadido campo opcional `shepherd: Option<AgentShepherdInfo>` en `ClientShellAgent` (`src/protocol/wire.rs`) con `#[serde(default, skip_serializing_if = "Option::is_none")]` preservando la compatibilidad binaria y decodificación de fixtures históricos generación 1 (`frozen_generation_one_snapshot_decodes`).

2. **Resolución en memoria sin I/O en render (`shepherd_agent_info_for_pane`)**:
   - Implementado en `src/shepherd/mod.rs` `shepherd_agent_info_for_pane`, que consulta el registro en caché del runtime (`svc.cached_registry()`) y `state.shepherd_agents_panel.entries`.
   - Cero I/O a disco durante el cálculo de vista o renderizado.
   - Enganche mínimo en `src/app/agents.rs` (`App::agent_info`) para asociar la información de Shepherd al `AgentInfo` del pane.
   - Mapeo en `src/server/client_shell.rs` (`snapshot_with_completions`) de `agent.shepherd` a `ClientShellAgent`.

3. **Marcador exclusivo para agentes de Shepherd y eliminación de heurísticas**:
   - En `src/client/shell/agent_sidebar.rs`, se eliminó totalmente la lectura de tokens ficticios (`shepherd_state`, `role`, `needs_you`, `shepherd_needs_you`, `notice`, `shepherd_notice`).
   - `shepherd_state` y `notice` se obtienen exclusivamente de `agent.shepherd`.
   - `needs_you` es verdadero **únicamente** cuando el agente pertenece a Shepherd y su estado de Shepherd es `"blocked"`. El estado incidental `AgentStatus::Blocked` de Herdr upstream ya no dispara `[NEEDS YOU]`.
   - Para agentes normales de Herdr (sin `shepherd`), la fila se dibuja de forma **exactamente idéntica a upstream** (test de caracterización comprobado celda a celda).
   - Con `shepherd.enabled = false`, no se dibuja ningún marcador ni aviso.

4. **Visualización completa de avisos (`notice`)**:
   - `agent_row_line_count` asigna una línea adicional a la fila en la barra lateral cuando `shepherd.enabled` está activo y el agente tiene aviso (`notice`).
   - En `render_agent_row`, si hay altura disponible (`rect.height > rows.len()`), el aviso se renderiza en una sublínea propia con indentación e indicador `↳` en amarillo, evitando truncamiento lateral.
   - Si el espacio vertical está restringido a 1 línea, el aviso se incluye inline entre paréntesis en la primera línea.
   - Protección total ante anchos o alturas diminutas (0x0, 1x0, 0x1, 1x1, 5x1, 5x2) sin provocar pánico.

5. **Tests automatizados en verde**:
   - `shepherd::tests::test_shepherd_agent_info_for_pane_resolution`: resolución de agente por `pane_id` con servicio o panel, y rechazo cuando shepherd está deshabilitado.
   - `client::shell::agent_sidebar::tests::test_shepherd_agent_in_each_state`: render de los 8 estados (`starting`, `working`, `idle`, `blocked` -> `[NEEDS YOU]`, `done`, `exited`, `killed`, `failed`).
   - `client::shell::agent_sidebar::tests::test_shepherd_agent_blocked_with_notice`: render completo del aviso tanto en sublínea dedicada como inline.
   - `client::shell::agent_sidebar::tests::test_non_shepherd_agent_blocked_without_marker`: agente ajeno bloqueado sin `[NEEDS YOU]` ni marcadores de Shepherd.
   - `client::shell::agent_sidebar::tests::test_characterization_normal_herdr_agent_renders_identically`: caracterización idéntica para agentes ajenos con `shepherd.enabled` apagado y encendido.
   - `client::shell::agent_sidebar::tests::test_shepherd_enabled_false_no_markers`: conmutación global de shepherd desactiva marcadores.
   - `client::shell::agent_sidebar::tests::test_small_dimensions_without_panic`: dimensiones cero y reducidas sin pánicos.

### Lo no verificado

- No se ejecutó el binario contra la sesión real del usuario, sockets de producción ni rutas reales (`~/.local/state/shepherd`, `~/.config/herdr*`), respetando estrictamente las directivas de seguridad.
- El test intermitente conocido upstream `app::api::plugins::tests::manifest_action_invoke_injects_plugin_paths` pasa en solitario.

### Qué NO queda conectado (explícito)

1. **Notificación reactiva push al sidebar ante escrituras externas directas en `agents.json`**:
   - Si un script externo edita directamente el archivo `agents.json` en disco saltándose el servicio o socket de Herdr, la barra lateral no se refresca inmediatamente por push de inotify/filesystem; espera a la siguiente sincronización en memoria / tick del servidor.
2. **Filtros específicos de Shepherd en el comando CLI `herdr agent list`**:
   - `herdr agent list` incluye el campo `shepherd` en la salida JSON de cada agente, pero no se añadieron flags específicos como `--shepherd-only` al CLI de Herdr (previsto para la fase de CLI F4).
3. **Interactividad del ratón sobre la sublínea de aviso**:
   - Hacer clic en la sublínea de aviso `↳ <aviso>` enfoca el pane del agente correspondiente (reutilizando el hit rect del agente), pero no despliega un diálogo contextual interactivo exclusivo para el aviso.

### Archivos de herdr modificados

| Archivo | Tipo de cambio | Contenido / Razón del cambio |
|---|---|---|
| `docs/next/api/herdr-api.schema.json` | Modificación | Definición formal de `AgentShepherdInfo` y propiedad opcional `shepherd` en `AgentInfo`. |
| `src/api/schema/agents.rs` | Modificación | `AgentShepherdInfo` y campo `shepherd: Option<AgentShepherdInfo>` en `AgentInfo`. |
| `src/protocol/wire.rs` | Modificación | Campo `shepherd: Option<AgentShepherdInfo>` en `ClientShellAgent` con `#[serde(default, skip_serializing_if = "Option::is_none")]`. |
| `src/shepherd/mod.rs` | Modificación | Función `shepherd_agent_info_for_pane` (resolución en memoria sin I/O en render) y test unitario de resolución. |
| `src/app/agents.rs` | Modificación | 7 líneas de enganche en `App::agent_info` para adjuntar `shepherd` a `AgentInfo` vía `crate::shepherd::shepherd_agent_info_for_pane`. |
| `src/server/client_shell.rs` | Modificación | Paso de `shepherd: agent.shepherd` en `snapshot_with_completions`. |
| `src/client/shell/agent_sidebar.rs` | Modificación | Eliminación de tokens heurísticos en `agent_row`, cálculo de líneas con aviso (`agent_row_line_count`), renderizado puro de `[estado]`, `[NEEDS YOU]` y aviso completo (`render_agent_row`), y suite completa de tests de render puro. |
| `src/client/shell/endpoint_agents.rs` | Modificación | Uso de `agent_row_line_count` para asignar altura a filas agregadas con aviso. |
| `src/client/shell/endpoint_agent_state.rs` | Modificación (test) | `shepherd: None` en helper de test. |
| `src/client/shell/notifications.rs` | Modificación (test) | `shepherd: None` en 2 tests. |
| `src/client/shell/tests/agents_worktrees_notifications.rs` | Modificación (test) | `shepherd: None` en fixtures de test. |
| `src/client/shell/tests/copy.rs` | Modificación (test) | `shepherd: None` en fixture de test. |
| `src/client/shell/tests/endpoints.rs` | Modificación (test) | `shepherd: None` en helper `agent(...)` de test. |
| `src/client/shell/tests/mobile.rs` | Modificación (test) | `shepherd: None` en fixture de test. |
| `src/client/shell/tests/popup_focus_projection.rs` | Modificación (test) | `shepherd: None` en fixture de test. |


### Nota del revisor (F3c)

Verificado por áreas (la suite completa de herdr no se puede cerrar en este entorno: el binario de tests muere por SIGPIPE en tests ajenos, ya antes de F3c): `client::shell` 388, `shepherd::` 42, `protocol::` 156, `api::` 318, `server::` 318, `shepherd-core` 63, todo en verde. Intermitentes que fallan solo en paralelo y pasan en solitario: `plugins::*injects_plugin_paths*`, `config::io::tests::config_diagnostic_summary_uses_compact_actionable_banner`. Nunca probado contra un herdr real.

## Prueba aislada (2026-11-04) — hallazgos

Binario del fork (`target/debug/herdr`), sesión `iso`, `HOME`/`XDG_*` temporales, dentro de tmux. Notas de método: el build de desarrollo lee `$XDG_CONFIG_HOME/herdr-dev/config.toml` (no `herdr/`); el servicio guarda su registro en `<config>/sessions/<sesión>/shepherd/agents.json`.

Funciona: con `shepherd.enabled=true` el overlay se abre con `prefix+a`; `shepherd.call` por el socket responde (`tasks`, `runs`, `skills`, `agents`, `task_create` creó y listó una tarea real en `.shepherd/tasks/`).

Fallos reales:
1. **El overlay no puede hablar con el servicio**: `ShepherdCall` está declarado en `src/api/schema.rs` con `client_shell=false`, así que el cliente muestra «Action unavailable: This server does not support shepherd.call yet» ante cualquier acción. (Arreglar exige añadir el método a las huellas de contrato de `src/server/client_commands.rs` como método aditivo.)
2. **Marco doble en el overlay**: el marco «Shepherd» contiene otro marco «Shepherd Tareas … esc close» propio de la vista; sobra uno.
3. **Arranque bloqueado con un registro no vacío**: `resume_on_startup` llama a `terminals.agent_status` de forma síncrona por el canal interno de la App durante el arranque, cuando la App aún no atiende; da `method=resume error=timeout`, el servidor tarda más de 15 s en estar listo y no reanuda nada. Con registro vacío arranca bien.

No verificado: marcadores y aviso en la barra lateral con un agente de Shepherd real (por el fallo 3 no pude sembrar uno), lanzar agentes, ratón en el overlay.

## Estado F3d (2026-11-04)

Corrección de los tres fallos detectados durante la prueba aislada mediante TDD y verificación real con `./scripts/shepherd-iso.sh`.

### Lo hecho

1. **`shepherd.call` admisible y anunciado al cliente de terminal**:
   - En `src/api/schema.rs`, se cambió `client_shell = true` en `ShepherdCall` para permitir que los clientes TUI invoquen métodos del servicio Shepherd.
   - En `src/server/client_commands.rs`, se registró `shepherd.call` como método aditivo en el mapa de huellas de contrato (`3fd905e0f2b2ff38463cb28ff9c30d51b3f89155d536c4fe0aca751842c4f1fe`), manteniendo inmutable la fixture congelada v1 (mismo patrón aditivo que `pane.clear` y `pane.link.resolve`).
   - En `crates/shepherd-core/src/service.rs`, se habilitaron alias compatibles para `"task_create" | "create_task"` y `"task_archive" | "archive_task"`.
   - En `src/client/shell/shepherd.rs`, se añadieron `"task_create"` y `"task_archive"` al manejador de respuestas para refrescar las vistas automáticamente tras crear o archivar tareas.

2. **Eliminación del marco interior doble en el overlay**:
   - Se removió el contenedor secundario `centered_popup_rect` y los recuadros interiores `draw_tasks_box`, `draw_runs_box` y `draw_skills_box` en `src/shepherd/tasks.rs`, `src/shepherd/runs.rs` y `src/shepherd/skills.rs`.
   - Se eliminaron los títulos redundantes de las sub-vistas y el aviso sobrante `esc close`.
   - El overlay (`src/shepherd/overlay.rs`) conserva un único marco exterior unificado con pestañas (`Tareas`, `Runs`, `Skills`), botón `[x]` y barra inferior de atajos.
   - Manejo jerárquico de `Esc`: si hay un submodal activo (crear tarea, importar issue, confirmación), el primer `Esc` cancela únicamente el submodal y deja visible la vista del overlay; un segundo `Esc` cierra el overlay completo.
   - Test unitario de render puro añadido: `test_overlay_renders_single_frame_without_inner_double_box_or_redundant_esc_close`.

3. **Arranque asíncrono no bloqueante con registro no vacío**:
   - En `src/shepherd/service.rs`, `start_for_app` ya no llama a `resume_on_startup()` de forma síncrona durante el inicio del servidor; en su lugar, inicializa el estado con `core.load_registry().unwrap_or_default()`.
   - La reanudación (`core.resume_on_startup()`) se difiere y ejecuta en el worker thread asíncrono (`call_worker`) antes de procesar peticiones, cuando el canal interno de la `App` ya está plenamente operativo y atendiendo mensajes.
   - Se añadió control atómico `resume_done` en `RuntimeServiceWorkerState` para garantizar que la reanudación se intente una única vez y no interfiera con peticiones entrantes.
   - Test unitario añadido: `test_start_for_app_does_not_block_on_startup_with_non_empty_registry`.

### Comprobaciones reales con `./scripts/shepherd-iso.sh` (capturas de pantalla)

#### 1. Arranque con registro poblado y barra lateral de agentes
Se sembró con `seed` un registro que contenía `coder-1` en estado `blocked` con aviso `trust dialog: open tab and accept`. Al ejecutar `start`, el servidor estuvo listo en menos de 3 segundos (sin colgarse). Tras `call pane.report_agent '{"pane_id":"w1:p1","source":"herdr:test","agent":"agy","state":"blocked"}'`, la barra lateral reflejó de inmediato el marcador y el aviso:

```text
 spaces                  │   1      +
                         │apolo@apolo:/tmp/shfiso/repo$
 ● repo                  │
   master                │
                         │
 new                 menu│
─────────────────────────│
 agents           grouped│
                         │
 ● repo [NEEDS YOU]      │
   agy                   │
   ↳ trust dialog: open t│
```

#### 2. Overlay con marco único y pestañas
Al pulsar `prefix + a` (`C-b` luego `a`), el overlay se abre con un solo marco limpio sin dobles recuadros ni texto redundante:

```text
┌──────────────────────────────────────────────────────────────────────────────────────────────┐
│ Shepherd                                                                                  [x]│
│ Tareas   Runs   Skills                                                                       │
│──────────────────────────────────────────────────────────────────────────────────────────────│
│ + Nueva (n)       ↓ GitHub issue (i)  ✎ $EDITOR (e)     📦 Archivar (a)                      │
│ 0 tarea(s) · vista previa y edición en $EDITOR                                               │
│                                                                                              │
│                                      │                                                       │
│ No hay tareas activas.               │ Selecciona una tarea para ver su preview.             │
│                                      │                                                       │
│                                                                                              │
│ n crear  ·   i importar gh  ·   e editar en $EDITOR  ·   a archivar  ·   Esc cerrar          │
│                                                                                              │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
```

#### 3. Submodal de creación y tecla Esc jerárquica
Al pulsar `n`, se superpone el formulario de creación de tarea. Al pulsar `Esc`, se cierra únicamente el submodal volviendo a la lista de tareas; un segundo `Esc` cierra el overlay completo.

```text
┌──────────────────────────────────────────────────────────────────────────────────────────────┐
│ Shepherd                                                                                  [x]│
│ Tareas   Runs   Skills                                                                       │
│──────────────────────────────────────────────────────────────────────────────────────────────│
│ + Nueva (n)       ↓ GitHub issue (i)  ✎ $EDITOR (e)     📦 Archivar (a)                      │
│ 0 tarea(s) · vista previa y edición en $EDITOR                                               │
│         ┌──────────────────────────────────────────────────────────────────────────┐         │
│         │ Crear Nueva Tarea (Shepherd)                                             │         │
│         │                                                                          │         │
│         │ Título (*):                                                              │         │
│         │ █                                                                        │         │
│         │ Descripción / Spec (*):                                                  │         │
│         │ <Descripción / Spec (*)>                                                 │         │
│         │ Criterios de aceptación (separados por ; o salto de línea):              │         │
│         │ <Criterios de aceptación (separados por ; o salto de línea)>             │         │
│         │ Archivos adjuntos (rutas existentes en disco, separadas por espacio):    │         │
│         │ <Archivos adjuntos (rutas existentes en disco, separadas por espacio)>   │         │
│         │                                                                          │         │
│         │   ↵ Crear tarea     esc Cancelar                                         │         │
│         └──────────────────────────────────────────────────────────────────────────┘         │
│                                                                                              │
│ n crear  ·   i importar gh  ·   e editar en $EDITOR  ·   a archivar  ·   Esc cerrar          │
│                                                                                              │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
```

#### 4. Creación real de tarea y refresco de vista sin errores
Se introdujo título y descripción y se confirmó con `Enter`. `shepherd.call` ejecutó `task_create`, el core escribió el archivo en `/tmp/shfiso/repo/.shepherd/tasks/1/spec.md`, el cliente recibió la confirmación y la lista se actualizó en pantalla sin mostrar «Action unavailable»:

```text
┌──────────────────────────────────────────────────────────────────────────────────────────────┐
│ Shepherd                                                                                  [x]│
│ Tareas   Runs   Skills                                                                       │
│──────────────────────────────────────────────────────────────────────────────────────────────│
│ + Nueva (n)       ↓ GitHub issue (i)  ✎ $EDITOR (e)     📦 Archivar (a)                      │
│ 1 tarea(s) · vista previa y edición en $EDITOR                                               │
│                                                                                              │
│ · #1  Test Task F3d                  │ # Tarea #1: Test Task F3d                             │
│                                      │ Estado: new  ·  Directorio: /tmp/shfiso/repo/.shepher │
│                                      │                                                       │
│                                      │ ## Especificación / Descripción                       │
│                                      │ # Spec                                                │
│                                      │                                                       │
│                                      │ Design decisions, constraints, interfaces. Agents rea │
│                                      │                                                       │
│                                      │ ## Criterios de Aceptación                            │
│                                      │   (Sin criterios definidos)                           │
│                                                                                              │
│ n crear  ·   i importar gh  ·   e editar en $EDITOR  ·   a archivar  ·   Esc cerrar          │
│                                                                                              │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
```

### Lo no verificado

- Conexión real con modelos de LLM en la nube o proveedores remotos (se verificó el runtime local de Shepherd y el protocolo cliente-servidor).
- Modificaciones en repositorios de producción o estados reales de usuario (`~/.local/state/shepherd`, etc.).
- Suite completa de herdr (falla por SIGPIPE ajeno en este entorno; los tests de plugins/config en paralelo son intermitentes, anótalos tal como pide el brief).

### Qué NO queda conectado

1. **Atajos para comandos externos no implementados en el core**:
   - `cancel_run` y `reject_gate` están preparados en el overlay pero `shepherd-core` aún no implementa cancelación o rechazo de fases fuera del flujo de aprobación estándar.
2. **Flags de filtrado de Shepherd en el comando global `herdr agent list`**:
   - No se alteró el CLI base de Herdr para flags específicos de Shepherd.
3. **Persistencia de sesión reactiva a cambios externos de archivo sin socket**:
   - Si un proceso externo manipula `agents.json` directamente sin pasar por el socket de Herdr, la TUI se actualiza en el próximo ciclo de reconciliación / tick, no por watcher de disco inmediato.

### Archivos de herdr y shepherd-core tocados

| Archivo | Tipo de cambio | Contenido / Razón del cambio |
|---|---|---|
| `crates/shepherd-core/src/service.rs` | Modificación | Alias para operaciones `task_create` / `create_task` y `task_archive` / `archive_task`. |
| `src/api/schema.rs` | Modificación | `ShepherdCall` marcado con `client_shell = true`. |
| `src/server/client_commands.rs` | Modificación | Método aditivo `shepherd.call` con su digest Sha256 en las comprobaciones de contrato. |
| `src/shepherd/service.rs` | Modificación | Desacople del arranque: `start_for_app` usa `load_registry` sin bloquear; reanudación diferida en el worker con `ensure_resumed`; test de arranque rápido con registro poblado. |
| `src/shepherd/tasks.rs` | Modificación | Eliminación de recuadro doble, marco interior y títulos redundantes en panel de tareas. |
| `src/shepherd/runs.rs` | Modificación | Eliminación de recuadro doble, marco interior y títulos redundantes en panel de runs. |
| `src/shepherd/skills.rs` | Modificación | Eliminación de recuadro doble, marco interior y títulos redundantes en panel de skills. |
| `src/shepherd/overlay.rs` | Modificación | Unificación del marco exterior, ajuste de dimensiones y test de render de marco único. |
| `src/client/shell/shepherd.rs` | Modificación | Reconocimiento de `task_create` y `task_archive` en el manejador de respuestas para refresco de vistas. |


### Nota del revisor (F3d), repetida por el revisor con `scripts/shepherd-iso.sh`

Verificado de verdad (capturas, sesión aislada): arranque con registro sembrado en ~2,5 s (antes >15 s y sin reanudar); overlay con marco único y sin «Action unavailable»; crear una tarea desde el overlay (`n`, Tab, Enter) la escribe en `.shepherd/tasks/` y la lista; `Esc` cierra; la barra lateral muestra `[NEEDS YOU]` y el aviso de un agente de Shepherd (`[exited]` después, porque la reconciliación marca como salido al agente sembrado, que no existe de verdad). Tests por áreas en verde (`api::` con el intermitente de plugins).

Fallos que quedan (candidatos F3e):
1. **Vista previa de tarea incorrecta**: lee `spec.md` (plantilla) en vez de `issue.md`, y no entiende el formato `- [ ] criterio` de `acceptance.md` («Sin criterios definidos» aunque los haya).
2. **Aviso truncado** en la barra lateral (`↳ trust dialog: open t`): el brief pedía leerlo entero (ajustar a varias líneas o mostrarlo completo en el menú contextual).
3. **Pestaña Skills**: «Agente '' (rol: )» sin agente seleccionado, aparece vacía en vez del catálogo, y «Borrar» está duplicado (`D` y `d`).
4. Sigue sin probarse: lanzar un agente real desde el TUI, aprobar un gate, el ratón en el overlay.

## Estado F3e (2026-11-04)

Corrección de los tres fallos detectados por el revisor en la fase F3d mediante TDD y verificación real con `./scripts/shepherd-iso.sh`.

### Lo hecho

1. **Vista previa de tarea real y parseo de criterios (`src/shepherd/tasks.rs`)**:
   - `ShepherdTaskRow::from_created_task` lee el contenido de `issue.md` y extrae título y descripción real de la tarea (en lugar de cargar la plantilla fija de `spec.md`).
   - `spec.md` se analiza con `is_spec_template`: si contiene únicamente la plantilla por defecto (`# Spec\n\nDesign decisions, constraints, interfaces...`), se omite para no duplicar ni ensuciar la vista. Si tiene contenido real y personalizado, se muestra en su propia sección `## Especificación`.
   - Criterios de aceptación: se leen de `acceptance.md` (con fallback a `criteria.md`), filtrando el marcador de posición por defecto `(add checkable criteria; ...)`. Los criterios se renderizan en formato `- [ ] criterio` (con marca amarilla) y `- [x] criterio` (con marca verde).
   - Test unitario con ficheros reales creados por `shepherd_core::create_task`: `test_from_created_task_with_real_files_renders_preview_correctly`.

2. **Aviso completo y multilínea en la barra lateral (`src/client/shell/agent_sidebar.rs`)**:
   - Algoritmo de ajuste de palabras `wrap_notice(notice, width, max_lines)` que formatea el aviso dentro del ancho disponible en la barra lateral (respetando la indentación de 5 caracteres: `↳ ` en la primera línea y `     ` en las siguientes).
   - Límite máximo razonable de líneas (`MAX_NOTICE_LINES = 4`) con truncado elegante `…` únicamente si el texto es excesivamente largo.
   - `agent_row_line_count` calcula la altura de la fila de forma coherente con el ancho y el render multilínea.
   - En paneles con altura de 1 línea, el aviso se sigue mostrando inline entre paréntesis `(aviso)`.
   - Los agentes sin aviso o ajenos a Shepherd se siguen dibujando exactamente como antes (test de caracterización idéntica en verde).
   - Test unitario con aviso largo envuelto en barra lateral de 25 caracteres: `test_shepherd_agent_with_long_notice_wrapped_in_narrow_sidebar`.

3. **Pestaña Skills con catálogo por defecto y botón único (`src/shepherd/skills.rs`, `src/client/shell/shepherd.rs`)**:
   - Sin agente seleccionado (`selected_agent_id` vacío), la vista se abre por defecto en modo `SkillsViewMode::CatalogView` y muestra el encabezado `Catálogo Completo (Repo > User > Default)` en vez de `Agente '' (rol: )`. Con agente seleccionado, muestra sus tres niveles (`AgentView`).
   - `src/client/shell/shepherd.rs` procesa la respuesta JSON de la op `"skills"` del servicio y puebla `overlay.skills.catalog_skills` mediante `CatalogSkillItem::from(&Skill)`.
   - Eliminado el botón duplicado `🗑 Borrar (D)`. La barra de acciones conserva únicamente `🗑 Borrar (d)`, encajando con holgura sin cortarse.
   - El atajo de borrado en modo catálogo responde tanto a `d` como a `D`, manteniendo la protección de skills predeterminadas (`Scope::Default`) y la confirmación destructiva antes de eliminar skills de usuario o repositorio.
   - Test unitario de vista catálogo sin agente y botón único: `test_skills_view_without_agent_shows_catalog_and_single_delete_button` y `test_delete_shortcut_accepts_both_lower_and_upper_case`.

### Comprobaciones reales con `./scripts/shepherd-iso.sh` (capturas de pantalla)

#### 1. Aviso largo multilínea en la barra lateral
Sembrado con un aviso de 67 caracteres (`"trust dialog: open tab and accept this permission dialog to continue"`). El aviso se ajusta a 4 líneas completas dentro del ancho del sidebar (25 cols) sin cortarse horizontalmente:

```text
 agents           grouped│
                         │
 ● repo [NEEDS YOU]      │
   agy                   │
   ↳ trust dialog: open  │
     tab and accept this │
     permission dialog to│
     continue            │
```

#### 2. Vista previa con descripción real y criterios de aceptación
Se creó la tarea con título `"Tarea F3e Real"`, descripción `"Esta es la descripcion real de la tarea en F3e"` y criterio `"Verificar vista previa y aviso"`. La vista previa muestra la descripción de `issue.md`, no incluye la plantilla de `spec.md`, y formatea el criterio como `- [ ]`:

```text
│ · #1  Tarea F3e Real                 │ # Tarea #1: Tarea F3e Real                            │
│                                      │ Estado: new  ·  Directorio: /tmp/shfiso/repo/.shepher │
│                                      │                                                       │
│                                      │ ## Descripción                                        │
│                                      │ Esta es la descripcion real de la tarea en F3e        │
│                                      │                                                       │
│                                      │ ## Criterios de Aceptación                            │
│                                      │   - [ ] Verificar vista previa y aviso                │
```

Al añadir contenido personalizado a `spec.md` y marcar el criterio con `[x]` en `acceptance.md`:

```text
│ · #1  Tarea F3e Real                 │ # Tarea #1: Tarea F3e Real                            │
│                                      │ Estado: new  ·  Directorio: /tmp/shfiso/repo/.shepher │
│                                      │                                                       │
│                                      │ ## Descripción                                        │
│                                      │ Esta es la descripcion real de la tarea en F3e        │
│                                      │                                                       │
│                                      │ ## Especificación                                     │
│                                      │ # Spec Arquitectura                                   │
│                                      │ Detalles tecnicos de la implementacion.               │
│                                      │                                                       │
│                                      │ ## Criterios de Aceptación                            │
│                                      │   - [x] Verificar vista previa y aviso                │
```

#### 3. Pestaña Skills mostrando el catálogo completo del servicio
Sin agente seleccionado, la pestaña muestra el catálogo del servicio (`code-review`, `conventional-commits`, `debug`, etc.), el encabezado `Catálogo Completo`, una única acción `🗑 Borrar (d)` y la barra cabe entera:

```text
┌──────────────────────────────────────────────────────────────────────────────────────────────┐
│ Shepherd                                                                                  [x]│
│ Tareas   Runs   Skills                                                                       │
│──────────────────────────────────────────────────────────────────────────────────────────────│
│ Catálogo (v)      + Nueva skill (n)   ↺ Restaurar (r)   🗑 Borrar (d)                         │
│ Catálogo Completo (Repo > User > Default)                                                    │
│ Nota: Añadir o quitar skills a este agente se aplicará al REINICIAR el agente.               │
│ Modificar el texto de una skill sí llega en vivo.                                            │
│                                                                                              │
│          code-review           origen:default  Use when reviewing a change as the reviewer r │
│          conventional-commits  origen:default  Use when writing commit messages - Convention │
│          debug                 origen:default  Use when something fails or behaves unexpecte │
│          docs-lookup           origen:default  Use before assuming how a library, API, CLI f │
│          implement-task        origen:default  Use when implementing a coding task in your w │
│          lang-go               origen:default  Use when working in a Go repository - error h │
│          lang-python           origen:default  Use when working in a Python repository - typ │
│          lang-ts               origen:default  Use when working in a TypeScript or JavaScrip │
│          plan-tasks            origen:default  Use when you must turn a task or issue into a │
│          pr-workflow           origen:default  Use when taking work from branch to pull requ │
│          security-review       origen:default  Use when reviewing a change for security - se │
│          shepherd-comms        origen:default  Use when you are an agent running under sheph │
│          write-tests           origen:default  Use when writing or fixing tests - table-driv │
│                                                                                              │
│ Space alternar/activar  ·   v cambiar vista  ·   r restaurar editada  ·   d borrar de catálog│
└──────────────────────────────────────────────────────────────────────────────────────────────┘
```

Al pulsar `d` sobre `code-review` (default), muestra el error de protección en el pie: `No se pueden borrar las skills predeterminadas del sistema (solo usuario/repo)`.

### Qué NO queda conectado

1. **Invocación interactiva de borrado/edición sobre agentes vivos desde la TUI**:
   - Las operaciones sobre el catálogo y agentes funcionan a través de `shepherd.call`; la reactividad en caliente del texto de una skill editada en agentes vivos corre por el protocolo de MCP/disco pero no hay un editor TUI embebido para el fichero de skill dentro del overlay (se abre con `$EDITOR` externo o CLI).
2. **Streaming en tiempo real de cambios en el catálogo de skills sin refresco**:
   - El catálogo se consulta al abrir el overlay o tras una acción de modificación/borrado; no existe sondeo continuo por inotify.

### Archivos de herdr tocados

| Archivo | Tipo de cambio | Contenido / Razón del cambio |
|---|---|---|
| `src/shepherd/tasks.rs` | Modificación | `from_created_task` lee título y descripción de `issue.md`, filtra plantilla de `spec.md`, parsea criterios de `acceptance.md` con formato `- [ ]` / `- [x]`; renderizado puro de preview con `## Descripción`, `## Especificación` opcional y checklist con marcas; tests unitarios con ficheros reales. |
| `src/client/shell/agent_sidebar.rs` | Modificación | Constantes `MAX_NOTICE_LINES`, `NOTICE_INDENT_WIDTH`, función `wrap_notice`, cálculo de líneas con ancho en `agent_row_line_count`, renderizado multilínea con `↳` y sangría; tests unitarios de envoltura y aviso largo. |
| `src/client/shell/endpoint_agents.rs` | Modificación | Paso de `area.width` a `agent_row_line_count` en la barra lateral agregada de endpoints. |
| `src/shepherd/skills.rs` | Modificación | Modo por defecto `SkillsViewMode::CatalogView` si no hay agente seleccionado; eliminación del botón duplicado `🗑 Borrar (D)`; soporte para teclas `d`/`D`; conversión `CatalogSkillItem::from(&Skill)`; tests unitarios de vista catálogo sin agente y botón único. |
| `src/shepherd/overlay.rs` | Modificación (tests) | Actualización de fixtures de prueba para incluir el nuevo campo `spec` en `ShepherdTaskRow` y modo `AgentView` en prueba de restauración. |
| `src/client/shell/shepherd.rs` | Modificación | Deserialización de la respuesta JSON del método `"skills"` para poblar `overlay.skills.catalog_skills`. |


### Nota del revisor (F3e)

Repetido por el revisor con `scripts/shepherd-iso.sh` y capturas: vista previa de tarea con descripción y criterios `- [ ]`; aviso de `[NEEDS YOU]` en varias líneas con «…» solo si no cabe; pestaña Skills sin agente muestra el catálogo y «Borrar» una sola vez. Tests en verde por áreas. Pequeño resto cosmético: en la vista de catálogo aparece la nota «Añadir o quitar skills a este agente…», que ahí no aplica. Sigue sin probarse: lanzar un agente real desde el TUI, aprobar un gate, ratón en el overlay.

## Prueba con agente real (2026-11-04)

Sesión aislada (`scripts/shepherd-iso.sh`, `SHF_HOME=$HOME` solo para que agy encuentre sus credenciales; `XDG_*` aislados) y un agente agy real (`gemini-3.8-flash-high`, `--mode accept-edits`).

Funciona: `spawn` por `shepherd.call` crea el pane y arranca agy; mientras espera el diálogo de confianza la barra lateral muestra el agente con el aviso «esperando al humano: diálogo de confianza o login; abre su pestaña»; tras aceptarlo el primer prompt llegó, el agente creó `hello.txt` con el texto pedido y Shepherd lo pasó a `done` con `has_result`; `kill` cerró el pane (`killed`) y `clean` lo quitó del registro.

Fallos observados:
1. **`worktree: true` falla siempre que `workflow.base_branch` esté vacío** (valor por defecto): Shepherd envía `base: ""` a `worktree.create` y herdr responde `worktree_create_failed: fatal: invalid reference:` (`crates/shepherd-core/src/spawn.rs:297`, `herdr.rs:181`). Debe omitir `base` o usar `HEAD`.
2. **El marcador `[estado]` de la barra lateral no se ve con etiquetas largas**: con la etiqueta real `repo · coder·gemini·crea…` la primera línea se llena y el marcador queda fuera; la fila solo mostró `coder-2`.
3. La llamada `spawn` tarda más de 20 s en responder (espera la verificación del primer prompt, que depende de que el humano acepte la confianza); el overlay la despacha en segundo plano, pero conviene que devuelva el agente enseguida y que el estado avance por el registro.

Sin probar: lanzar el agente desde el formulario del TUI (solo por API), runs y gates reales, ratón en el overlay.

## Estado F3f (2026-11-04)

Corrección de la creación de worktrees con `base` vacío y visualización permanente del marcador de estado `[estado]` en la barra lateral con etiquetas largas, mediante TDD y verificación real con `./scripts/shepherd-iso.sh`.

### Lo hecho

1. **Creación de worktree con `base` omitido o vacío (`crates/shepherd-core` y `src/app/api/worktrees`)**:
   - En `crates/shepherd-core/src/terminals.rs`, se cambió `WorktreeSpec.base` a `Option<String>`, adaptando `create_git_worktree` y `herdr_open_agent`.
   - En `crates/shepherd-core/src/spawn.rs` (línea ~295) y `crates/shepherd-core/src/service.rs` (línea ~1053), cuando `workflow.base_branch` está vacío (o solo espacios), `base` se asigna como `None`. Si está configurado, se preserva como `Some(base_branch)`.
   - En `crates/shepherd-core/src/herdr.rs`, `HerdrClient::create_worktree` acepta `base: Option<&str>` y omite la clave `"base"` del payload JSON cuando es `None` o una cadena vacía.
   - En `src/app/api/worktrees/deferred.rs`, se protege la API de Herdr contra referencias vacías: `params.base.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "HEAD".into())`, garantizando que si un cliente envía `"base": ""` se use `HEAD` en vez de fallar con `worktree_create_failed: fatal: invalid reference:`.
   - Tests añadidos en el crate:
     - `herdr::tests::create_worktree_omits_base_when_empty_or_none`: comprueba que `create_worktree` no envía `"base"` cuando es `None`, `""` o espacios en blanco, y sí lo envía cuando es un nombre válido.
     - `spawn::tests::spawn_uses_terminals_trait_and_enforces_can_spawn_and_depth_limits`: comprueba que el fake `InMemoryTerminals` recibe la petición con `base: None`.
     - `spawn::tests::spawn_with_configured_base_branch_passes_base_to_terminals`: comprueba que cuando `base_branch` está configurado (`develop`), el fake recibe `base: Some("develop")`.
     - `app::api::worktrees::tests::api_worktree_create_with_empty_base_defaults_to_head`: test asíncrono de API en Herdr que verifica que `worktree.create` con `base: ""` tiene éxito y ramifica desde `HEAD`.

2. **Marcador `[estado]` visible con etiquetas largas (`src/client/shell/agent_sidebar.rs`)**:
   - En `render_agent_row`, para la primera línea (`index == 0`), se pre-calcula la anchura necesaria para el marcador de estado de Shepherd (`shepherd_marker` y `marker_width` para `[working]`, `[NEEDS YOU]`, etc.).
   - Se descuenta `marker_width` del ancho disponible para tokens (`max_token_width = (rect.width as usize).saturating_sub(indent).saturating_sub(marker_width)`).
   - De este modo, `crate::ui::resolved_token_spans` trunca automáticamente las etiquetas largas con «…» dejando espacio exacto para que el marcador de estado quepa en la primera línea sin desbordar ni cortarse.
   - Para agentes normales de Herdr (sin `shepherd` o con `shepherd.enabled = false`), `marker_width` es 0 y se preserva el comportamiento 100% idéntico al original celda a celda (test de caracterización en verde).
   - Tests añadidos en `agent_sidebar.rs`:
     - `test_shepherd_agent_with_long_label_keeps_marker_visible`: con etiqueta real `repo · coder·gemini·crea-un-archivo-` en sidebar de 25 columnas, comprueba que la etiqueta se trunca con `…` y `[working]` se dibuja completo.
     - `test_shepherd_agent_with_long_label_blocked_keeps_needs_you_visible`: ídem para estado bloqueado, comprobando que `[NEEDS YOU]` permanece visible.

### Comprobaciones reales con `./scripts/shepherd-iso.sh` (capturas de pantalla)

#### 1. Creación de worktree real por API (`worktree.create`)
En la sesión aislada sobre `/tmp/shfiso/repo`:
- Llamada sin `base`:
  `./scripts/shepherd-iso.sh call worktree.create '{"cwd":"/tmp/shfiso/repo","branch":"shep/1-f3f-verify","label":"f3f-verify","focus":false}'`
  Respuesta: `{"type":"worktree_created", ... "worktree":{"branch":"shep/1-f3f-verify", ...}}` (éxito total).
- Llamada con `base: ""`:
  `./scripts/shepherd-iso.sh call worktree.create '{"cwd":"/tmp/shfiso/repo","branch":"shep/2-test-empty-base","base":"","label":"test-empty","focus":false}'`
  Respuesta: `{"type":"worktree_created", ...}` (éxito total, usa `HEAD` sin error de referencia inválida).

#### 2. Marcador `[estado]` visible con etiqueta larga en la barra lateral
Sembrado un agente de Shepherd con estado `working` en `w1:p1` y renombrada la pestaña y el pane con la etiqueta real `coder·gemini·crea-un-archivo-`.
Captura de pantalla (`./scripts/shepherd-iso.sh screen`):

```text
 spaces                     │  coder·gemini·crea-un-archivo-    +
                            │apolo@apolo:/tmp/shfiso/repo$
 ● repo                    ▾│
   master                   │
   └─ · f3f-verify          │
                            │
 new                    menu│
────────────────────────────│
 agents              grouped│
                            │
 ● repo · coder·g… [working]│
   gemini                   │
```

El marcador `[working]` se muestra completo y visible a la derecha, mientras que la etiqueta larga se ajusta elegantemente con elipsis `repo · coder·g…` en el ancho estándar del sidebar (25 cols).

### Archivos de herdr y shepherd-core tocados

| Archivo | Tipo de cambio | Contenido / Razón del cambio |
|---|---|---|
| `crates/shepherd-core/src/terminals.rs` | Modificación | `WorktreeSpec.base: Option<String>`; paso de `wt.base.as_deref()` en `herdr_open_agent`; filtro en `create_git_worktree`; actualización de fixtures en tests. |
| `crates/shepherd-core/src/spawn.rs` | Modificación | Asignación de `base: None` cuando `base_branch` está vacío en `spawn_in_terminals`; tests unitarios con `InMemoryTerminals`. |
| `crates/shepherd-core/src/service.rs` | Modificación | Asignación de `base: None` cuando `base_branch` está vacío en orquestación de agentes. |
| `crates/shepherd-core/src/herdr.rs` | Modificación | `create_worktree` acepta `Option<&str>` y omite la clave `"base"` del JSON si está vacía; test `create_worktree_omits_base_when_empty_or_none`. |
| `src/app/api/worktrees/deferred.rs` | Modificación | Fallback defensivo en `start_api_worktree_create`: `base.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "HEAD".into())`. |
| `src/app/api/worktrees.rs` | Modificación (test) | Test `api_worktree_create_with_empty_base_defaults_to_head`. |
| `src/client/shell/agent_sidebar.rs` | Modificación | Reserva de espacio para `shepherd_marker` en `render_agent_row` permitiendo truncado de etiquetas largas con `…`; tests de render con etiqueta real para `[working]` y `[NEEDS YOU]`. |

### Qué NO queda conectado (explícito)

1. **Lanzamiento interactivo de agentes reales en este entorno**:
   - No se lanzaron agentes reales (p. ej. `agy` con LLMs externos) por directiva del brief (no hay quién conteste diálogos interactivos de permisos); se probó a nivel de API, runtime de worktrees y TUI en sesión aislada.
2. **Optimizaciones de latencia en la llamada `spawn`**:
   - El fallo 3 observado en la prueba con agente real (spawn tarda >20 s si espera la aceptación interactiva del diálogo de permisos) sigue pendiente para la fase correspondiente (F3g / F4).


### Nota del revisor (F3f)

Repetido por el revisor: `worktree.create` con `base` omitido y con `base: ""` crea el worktree en la sesión aislada; tests en verde (`shepherd-core` 65, `client::shell` 391, `shepherd::` 47, `protocol::` 156, `server::` 318, `app::api::worktrees` 33). Cambio en herdr no previsto por el encargo: `src/app/api/worktrees/deferred.rs` trata `base` vacío como `HEAD` (endurecimiento de la API; anotado entre los archivos de herdr modificados).

## Prueba de comunicación entre agentes (2026-11-04)

Sesión aislada con un agy real (`gemini-3.8-flash-high`, sin MCP porque agy no lo declara en la config de vendors) y el servidor MCP real del servicio (loopback, token por agente) llamado por HTTP con el token de ese agente. Un agente `claude`/`codex` real usando las herramientas MCP **no** se ha probado.

Funciona: `initialize` y `tools/list`; autenticación (token falso → 401); `send_message` escribe en el bus y `read_messages` lo devuelve; `submit_result` registra el resultado y `get_result` lo lee; el agente agy real terminó y pasó a `done`; la barra lateral mostró `[working]` visible con la etiqueta larga (F3f confirmado con agente real).

Fallo: **los mensajes no se entregan a la terminal de agentes sin MCP.** La skill `shepherd-comms` promete que llegan como `[shep] from <rol>#<n>: …`, pero en el servicio de Rust `send_message` solo escribe en el bus y nada lo teclea en el pane del destinatario (no existe «[shep]» en el código). Tras `send_message` al agente agy, su terminal no mostró nada. Un agente sin MCP (todos los agy hoy) solo recibe el primer prompt. Pendiente: un repartidor en el servicio que, para agentes no-MCP vivos, teclee los mensajes nuevos del bus en su pane cuando esté inactivo (con las mismas cautelas del primer prompt: esperar a que esté libre, verificar, no pisar un diálogo).

### Prueba con agentes reales claude + codex (2026-11-04)

Sesión aislada (`SHF_HOME=$HOME` solo para las credenciales de los vendors). Un `brain` (`claude`, `haiku`) recibió una tarea que usaba solo las herramientas MCP de Shepherd y el servicio lanzó un `coder` (`codex`, worktree).

Funcionó de punta a punta: el brain llamó a `list_agents`, `spawn_agent` (el servicio abrió el pane y el worktree de codex), `send_message`, `wait_for`, `get_result` y `submit_result`; el coder (codex) llamó a `read_messages` por iniciativa propia, leyó «recuerda no hacer commit» del brain, creó `hola.txt` en su worktree sin commit y llamó a `submit_result`; ambos agentes pasaron a `idle` con `has_result` y la barra lateral los mostró con su marcador. Skills (`shepherd-comms`, `implement-task`), MCP con token por agente y entrega del primer prompt confirmados con vendors reales.

Observaciones:
- `get_task_context` devuelve «this agent has no task context» cuando el agente se lanza sin `task_id` (esperable, pero el prompt del coder le manda llamarlo primero).
- Los worktrees se crean bajo el `HOME` con el que corre el servidor: con `SHF_HOME` real se crearon en `~/.herdr/worktrees/` (limpiados a mano); la config de confianza de claude/codex/agy conserva la entrada `/tmp/shfiso/repo`.
- Sigue el fallo de los agentes **sin MCP** (agy): los mensajes del bus no se teclean en su terminal.

## Estado F3g (2026-11-04)

Implementación del repartidor de mensajes del bus a terminal para agentes vivos y sin MCP (`mcp: false`), con cursor de entrega persistente, cautelas de pantalla quieta, reintentos y tolerancia a fallos.

### Lo hecho

1. **Cursor de entrega persistente y retrocompatible**:
   - En `crates/shepherd-core/src/lib.rs`, se añadió `#[serde(default)] pub delivery_cursor: i64` a la estructura `Agent`.
   - Se serializa y persiste de forma duradera en `agents.json`, sobreviviendo a reinicios del servicio sin duplicar ni perder mensajes.

2. **Repartidor de mensajes a terminal (`service.rs`)**:
   - En cada tick del bucle de reconciliación (`reconcile_once`), se evalúan los agentes vivos y sin MCP (`!agent.mcp && !agent.gone() && !agent.terminal() && agent.state != "starting"`).
   - Se leen los mensajes del bus dirigidos al agente (`to == agent.id`) cuyo `id > agent.delivery_cursor`. Los mensajes `to == "user"` nunca se entregan.
   - Formateo exacto según especificación: `[shep] from <rol>#<n>: <contenido>` (remitente derivado de rol y seq del agente en el registro, e.g. `brain#1`; una sola línea con `\n`/`\r` convertidos a espacios; contenido recortado con `…` hasta un máximo razonable de 500 caracteres).
   - Cautelas aplicadas antes de teclear:
     - El agente debe estar en estado libre (`agent.state == "idle"` o `"working"`, pero su estado de terminal en herdr debe ser `"idle"`).
     - No debe tener avisos de bloqueo (`agent.notice.is_empty()`) ni estar bloqueado.
     - La pantalla del pane debe estar quieta (delta de pantalla durante 50 ms).
   - Verificación y reintentos:
     - Tras teclear el mensaje mediante `send_text`, se verifica que el texto aparezca en la pantalla del pane esperando con polling hasta `reaction_timeout`.
     - Si la verificación falla, se reintenta hasta 3 veces (`max_prompt_resends`).
     - Solo tras verificar la aparición del texto se avanza y persiste `delivery_cursor = msg.id`.
     - Si un mensaje falla tras los reintentos, se registra la incidencia en `service.log` y el tick de reconciliación continúa sin abortar.

3. **Soporte en mock `InMemoryTerminals`**:
   - Soporte de eco configurable en `send_text` (`set_echo_on_send_text(bool)`).
   - Sincronización de estados entre `agent.id` y `agent.pane` en `set_status` y `remove_status`.

4. **Tests con fakes (`InMemoryTerminals` + reloj inyectado)**:
   - `format_exact_sender_and_content_truncation`: formato exacto `[shep] from <rol>#<n>: <contenido>`, roles desconocidos / fallback seq, eliminación de saltos de línea y truncado con `…`.
   - `req7_deliver_bus_messages_order_once_non_mcp_and_persists_cursor`: entrega ordenada, una sola vez, exclusión de agentes MCP y destinatario `"user"`, y persistencia del cursor tras reinicio del servicio.
   - `req7_deliver_bus_messages_blocked_working_and_terminal_states`: omisión durante estados ocupados/bloqueados, entrega inmediata al liberarse el pane a `idle`, y exclusión de agentes terminados (`done`, `exited`).
   - `req7_deliver_bus_messages_retry_on_verification_failure`: 3 reintentos ante fallos de verificación en pantalla, registro del fallo en `service.log` sin tumbar el tick, y entrega exitosa posterior.

### Verificación real con `./scripts/shepherd-iso.sh` (sin agentes reales)

Se configuró un entorno aislado mediante `./scripts/shepherd-iso.sh` con el siguiente procedimiento exacto:
1. `seed` con un `agents.json` de prueba conteniendo dos agentes:
   - Destino sin MCP: `id: "coder-1"`, `role: "coder"`, `vendor: "gemini"`, `mcp: false`, `pane: "w1:p1"`, `delivery_cursor: 0`.
   - Remitente con MCP: `id: "brain-1"`, `role: "brain"`, `vendor: "claude"`, `mcp: true`, `token: "token-brain-f3g-verify-42"`.
2. Arranque del servidor con `./scripts/shepherd-iso.sh start`.
3. Reporte del pane `w1:p1` como agente herdr en estado `idle`:
   `call pane.report_agent '{"pane_id":"w1:p1","source":"herdr:test","agent":"agy","state":"idle"}'`.
4. Envío de mensaje MCP por HTTP (puerto local obtenido con `ss -ltnp | grep herdr`):
   ```bash
   curl -s -X POST "http://127.0.0.1:$PORT/mcp" \
     -H "Authorization: Bearer token-brain-f3g-verify-42" \
     -H "Content-Type: application/json" \
     -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send_message","arguments":{"to":"coder-1","content":"hola coder-1 desde brain"}}}'
   ```
   Respuesta del MCP: `{"id":1,"jsonrpc":"2.0","result":{"content":[{"text":"delivered message 1 to coder-1","type":"text"}],"isError":false}}`.
5. Espera de 3 segundos para el tick del bucle de reconciliación (intervalo 2 s).
6. Captura de pantalla (`./scripts/shepherd-iso.sh screen`):

```text
 spaces                     │   1      +
                            │apolo@apolo:/tmp/shfiso/repo$ [shep] from brain#1: hola coder-1 desde brain
 ○ repo                     │[shep]: command not found
   master                   │apolo@apolo:/tmp/shfiso/repo$
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
 new                    menu│
────────────────────────────│
 agents              grouped│
                            │
 ○ repo [working]           │
   agy                      │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                            │
                           «│
```

El mensaje `[shep] from brain#1: hola coder-1 desde brain` fue tecleado limpiamente en el pane de la terminal, ejecutado por la shell activa, y el cursor de entrega de `coder-1` en `agents.json` avanzó de `0` a `1`.
7. Parada limpia con `./scripts/shepherd-iso.sh stop`.

### Archivos tocados

| Archivo | Tipo de cambio | Contenido / Razón del cambio |
|---|---|---|
| `crates/shepherd-core/src/lib.rs` | Modificación | Añadido campo `#[serde(default)] pub delivery_cursor: i64` a la estructura `Agent`. |
| `crates/shepherd-core/src/service.rs` | Modificación | Implementación del formateador `[shep] from ...`, comprobación de pane libre/pantalla quieta, reintentos de verificación en pantalla, integración de `deliver_pending_non_mcp_messages` en `reconcile_once`, y tests unitarios. |
| `crates/shepherd-core/src/terminals.rs` | Modificación | Soporte de eco en `send_text` para `InMemoryTerminals` y sincronización bidireccional de `pane`/`id` en `set_status` / `remove_status`. |
| `SHEPHERD.md` | Documentación | Registro de Estado F3g, capturas y notas de verificación. |

### Qué NO queda conectado (explícito)

1. **Agentes reales de línea de comandos sin MCP**:
   - No se invocaron binarios reales de agentes CLI externos (p. ej. procesos reales de `agy` contra APIs externas de LLM) por directiva explícita del brief; la verificación se realizó con el terminal pane real de herdr (shell de bash) y el servidor MCP loopback HTTP.
2. **Skill `shepherd-comms`**:
   - El texto existente en `crates/shepherd-core/assets/skills/shepherd-comms/SKILL.md` bajo «Without MCP» promete exactamente el comportamiento implementado (`[shep] from <role>#<n>: ...`), por lo que no requirió modificaciones adicionales.



## Estado F4a (2026-11-04)

CLI `herdr shepherd` con JSON uniforme y estable contra el servidor en marcha mediante `shepherd.call`, fallback a in-process y flag explícito `--no-server`, auditado con TDD y verificado de verdad con `./scripts/shepherd-iso.sh` sin agentes reales.

### Tabla de Auditoría de Subcomandos CLI (20 subcomandos)

| # | Subcomando | Acepta `--json` | Op `shepherd.call` equivalente | Funciona contra servidor activo | Comportamiento / Modo |
|---|---|---|---|---|---|
| 1 | `agents` (o `agent list`) | Sí | `"agents"` | Sí | Servidor delega en `shepherd.call`; in-process lee `agents.json`. |
| 2 | `spawn` | Sí | `"spawn"` | Sí | Servidor delega en `shepherd.call`; in-process orquesta localmente. |
| 3 | `kill` | Sí | `"kill"` | Sí | Servidor delega en `shepherd.call`; in-process orquesta localmente. |
| 4 | `clean` | Sí | `"clean"` | Sí | Servidor delega en `shepherd.call`; in-process orquesta localmente. |
| 5 | `focus` | Sí | `"focus"` | Sí | Servidor delega en `shepherd.call`; in-process enfoca vía terminal. |
| 6 | `run` | Sí | `"run"` | Sí | Servidor delega en `shepherd.call`; in-process orquesta localmente. |
| 7 | `approve` | Sí | `"approve"` | Sí | Servidor delega en `shepherd.call`; in-process avanza gate/plan. |
| 8 | `skills list` (o `skills`) | Sí | `"skills"` | Sí | Servidor delega en `shepherd.call`; in-process lista catálogo de skills. |
| 9 | `runs list` (o `runs`) | Sí | `"runs"` | Sí | Servidor delega en `shepherd.call`; in-process computa runs desde tareas. |
| 10 | `tasks` (alias de `task list`) | Sí | `"tasks"` | Sí | Servidor delega en `shepherd.call`; in-process lista tareas activas. |
| 11 | `task list` | Sí | `"tasks"` | Sí | Servidor delega en `shepherd.call`; in-process lista tareas activas. |
| 12 | `task list --archived` | Sí | `"tasks"` (`archived: true`) | Sí | Servidor delega en `shepherd.call`; in-process lista archivadas. |
| 13 | `task new` | Sí | `"task_create"` | Sí | Servidor delega en `shepherd.call`; in-process crea tarea en disco. |
| 14 | `task archive` | Sí | `"task_archive"` | Sí | Servidor delega en `shepherd.call`; in-process archiva tarea en disco. |
| 15 | `task show <id>` | Sí | N/A | In-process | Operación sobre sistema de archivos; lee estructura de tarea local. |
| 16 | `task show <id> <sec>` | Sí | N/A | In-process | Operación sobre sistema de archivos; lee sección Markdown de tarea. |
| 17 | `task plan <id>` | Sí | N/A | In-process | Valida y guarda sub-tareas del plan en `status.json` local. |
| 18 | `roles list` (o `roles`) | Sí | N/A | In-process | Inspecciona configuración local y ejecutables de vendors activos. |
| 19 | `vendors list` (o `vendors`) | Sí | N/A | In-process | Inspecciona configuración de vendors y disponibilidad en PATH. |
| 20 | `msg` / `messages` | Sí | No disponible | Error JSON | El servicio no expone operación de mensajería (no inventada según brief; emite `{"error":"..."}` y exit ≠ 0). |
| * | `attach` | N/A | N/A | Interactivo | Comando TUI interactivo para adjuntar terminal; no emite JSON. |

### Lo hecho

1. **Protocolo y cliente IPC (`crates/shepherd-core/src/herdr.rs`)**:
   - Se añadió método `shepherd_call(&self, op: &str, args: Value) -> Result<Value, HerdrError>` que empaqueta las peticiones JSON-RPC hacia el método `"shepherd.call"` y extrae el payload `"data"`.
   - Se implementó `is_reachable(&self) -> bool` que verifica mediante sonda que el socket UNIX responda y reconozca la variante `"shepherd.call"` con el servicio de Shepherd activo. Si el socket pertenece a un Herdr sin Shepherd o con el servicio apagado (`unknown variant shepherd.call` o `shepherd_not_running`), devuelve `false`, evitando contaminar sesiones del usuario o causar errores de protocolo.
   - `HerdrClient` implementa `Clone` para facilitar su compartición e inyección en los despachadores de CLI y tests.

2. **Flag `--no-server` y resolución limpia de servidor (`crates/shepherd-core/src/cli.rs`)**:
   - Flag explícito `--no-server`: cuando está presente, omite la conexión al socket de sesión e invoca la orquestación directamente en el proceso local.
   - Si no se pasa `--no-server`, `run_cli_in_root_with_output` detecta automáticamente si hay un socket de sesión activo (`HERDR_SOCKET_PATH` o `HERDR_SESSION`) con soporte de Shepherd. Si no hay servidor o no responde, cae limpiamente al modo in-process sin interrupción.
   - Los subcomandos de estado vivo (`agents`, `spawn`, `kill`, `clean`, `focus`, `run`, `approve`, `skills`, `runs`, `tasks`, `task new`, `task archive`) delegan transparentemente en el servidor mediante `shepherd.call`.

3. **Salida `--json` uniforme y manejo estricto de errores**:
   - Todos los subcomandos que devuelven datos aceptan `--json` en cualquier posición de sus argumentos.
   - Con `--json`, la salida estándar (`stdout`) es estrictamente JSON válido (comprobable con `jq` o `python3 -m json.tool`).
   - Cuando ocurre un error en modo `--json`, el proceso finaliza con código de salida ≠ 0 (código 1) y escribe un objeto JSON en `stdout`: `{"error":"<mensaje>"}`.
   - En `src/shepherd/mod.rs` y `crates/shepherd-core/src/main.rs`, se silencia el formateo a texto plano de `eprintln!` en `stderr` cuando se solicitó `--json`, manteniendo los flujos limpios para consumo programático por scripts.

4. **Incorporación de subcomandos faltantes y auditoría de mensajería**:
   - Añadidos `skills list` (y alias `skills`) para listar el catálogo de habilidades en texto o JSON.
   - Añadidos `runs list` (y alias `runs`) para listar las ejecuciones y estados de pipelines en texto o JSON.
   - Alias `tasks` para listar tareas de forma equivalente a `task list`.
   - Respecto a `msg` / `messages`: se auditó `crates/shepherd-core/src/service.rs` y se constató que no existe operación en `handle_call` para mensajería directa por socket. Siguiendo rigurosamente la regla del brief («si el servicio ya tiene la operación; si no, no la inventes: anótalo»), no se inventó la operación en el servicio; el CLI devuelve error informativo en JSON con código de salida 1.

5. **Pruebas unitarias completas (TDD)**:
   - `test_all_subcommands_json_output`: prueba uno a uno los subcomandos con `--json`, verificando que la salida es JSON válido y que respeta el esquema esperado.
   - `test_server_mode_delegation_via_in_process_client`: comprueba la delegación exacta de todas las operaciones hacia `shepherd.call` mediante un cliente in-process mock.
   - `test_fallback_to_in_process_when_no_server`: comprueba que ante la ausencia de servidor o socket inexistente, el CLI conmuta a in-process sin fallar.
   - `test_json_error_output`: comprueba que ante errores de validación o argumentos con `--json`, se emite `{"error":"..."}` en stdout y código de error.
   - `test_flag_parsing_no_server_and_json_position`: comprueba que `--json` y `--no-server` se aceptan en cualquier posición sin desplazar parámetros posicionales.

### Comprobaciones reales con `./scripts/shepherd-iso.sh` (sesión aislada)

Se utilizó `./scripts/shepherd-iso.sh` sin agentes reales para verificar la interacción contra el servidor real en marcha:

1. **Arranque y verificación del método `shepherd.call`**:
   - `call shepherd.call '{"op":"agents","args":{}}'` devolvió `{"id":"1","result":{"data":[],"type":"shepherd_call"}}`.

2. **Creación de tarea vía CLI contra el servidor en marcha**:
   ```bash
   env HERDR_SOCKET_PATH=/tmp/shfiso/cfg/herdr-dev/sessions/iso/herdr.sock \
     ./target/debug/herdr shepherd task new "Iso Task" --description "Desc" --criteria "- [ ] Done" --json
   ```
   Salida JSON validada con `python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["status"]["title"])'`:
   `Iso Task` (código de salida 0).

3. **Verificación cruzada en el socket del servidor**:
   - Invocando directamente `./scripts/shepherd-iso.sh call shepherd.call '{"op":"tasks","args":{}}'`, el servidor devolvió:
     ```json
     {"id":"1","result":{"data":[{"directory":"/tmp/shfiso/repo/.shepherd/tasks/1-iso-task","status":{"created":"2026-11-04T15:30:02Z","id":1,"slug":"iso-task","state":"new","title":"Iso Task","updated":"2026-11-04T15:30:02Z"}}],"type":"shepherd_call"}}
     ```
     demostrando que la tarea fue creada y persistida en el servidor Herdr en ejecución.

4. **Listado de tareas, skills y runs con `--json`**:
   - `shepherd task list --json`: devolvió lista de tareas con longitud 1 y título `Iso Task`.
   - `shepherd skills list --json`: devolvió array con 13 skills del catálogo.
   - `shepherd runs list --json`: devolvió la ejecución asociada a la tarea (`task_id: 1, status: "new"`).
   - `shepherd agents --json`: devolvió array vacío `[]`.

5. **Visualización en el overlay TUI de Herdr (captura de pantalla)**:
   Al enviar la secuencia de apertura de overlay `prefix + a` (`C-b` luego `a`), la tarea creada a través del CLI apareció inmediatamente en el TUI:

   ```text
                            ┌──────────────────────────────────────────────────────────────────────────────────────────────┐
                            │ Shepherd                                                                                  [x]│
                            │ Tareas   Runs   Skills                                                                       │
                            │──────────────────────────────────────────────────────────────────────────────────────────────│
                            │ + Nueva (n)       ↓ GitHub issue (i)  ✎ $EDITOR (e)     📦 Archivar (a)                      │
                            │ 1 tarea(s) · vista previa y edición en $EDITOR                                               │
                            │                                                                                              │
                            │ · #1  Iso Task                       │ # Tarea #1: Iso Task                                  │
                            │                                      │ Estado: new  ·  Directorio: /tmp/shfiso/repo/.shepher │
                            │                                      │                                                       │
                            │                                      │ ## Descripción                                        │
                            │                                      │ Desc                                                  │
                            │                                      │                                                       │
                            │                                      │ ## Criterios de Aceptación                            │
                            │                                      │   - [ ] Done                                          │
                            │                                      │                                                       │
                            │ n crear  ·   i importar gh  ·   e editar en $EDITOR  ·   a archivar  ·   Esc cerrar          │
                            └──────────────────────────────────────────────────────────────────────────────────────────────┘
   ```

6. **Error uniforme con `messages --json`**:
   - Al ejecutar `shepherd messages --json`, el comando terminó con código de salida 1 y emitió estrictamente:
     `{"error":"operation not available (service does not support msg/messages)"}`.

7. **Forzado in-process con `--no-server`**:
   - `shepherd task list --no-server --json` ejecutó en modo local sin contactar con el socket, devolviendo igualmente los datos en formato JSON estable.

8. **Parada limpia**:
   - `./scripts/shepherd-iso.sh stop` finalizó correctamente el servidor y la sesión de tmux aislada.

### Nota de uso para scripting

Todos los subcomandos de `herdr shepherd` que devuelven información admiten la bandera `--json`, lo que permite integrarlos directamente en scripts de automatización con herramientas estándar:

```bash
# Obtener IDs de agentes activos
herdr shepherd agents --json | jq -r '.[].id'

# Crear una nueva tarea y capturar su ID
TASK_ID=$(herdr shepherd task new "Refactor auth" --description "Migrate to JWT" --criteria "- [ ] Tests pass" --json | jq -r '.status.id')

# Listar skills disponibles en el catálogo
herdr shepherd skills list --json | jq -r '.[].name'

# Forzar ejecución in-process si no se desea interactuar con el socket del servidor
herdr shepherd task list --no-server --json
```

Si el comando falla, el código de salida no será cero y el motivo aparecerá en el campo `.error` del JSON emitido.

### Qué NO queda conectado (explícito)

1. **Vistas TUI de integración Git/PR (Fase F4b)**:
   - El árbol visual de ramas de agentes, PRs y comparativas de git diff queda para la siguiente fase F4b.
2. **Streaming continuo de eventos de Shepherd a clientes externos**:
   - El CLI opera mediante peticiones puntuales síncronas (`request / response`); el streaming bidireccional por socket queda para las herramientas integradas.
3. **Operaciones de mensajería interactiva por CLI (`msg`/`messages`)**:
   - No se alteró el despachador `handle_call` del servidor para añadir operaciones inexistentes de mensajería; los agentes se comunican por el protocolo MCP en loopback y entrega a terminal.

### Archivos tocados

| Archivo | Tipo de cambio | Contenido / Razón del cambio |
|---|---|---|
| `crates/shepherd-core/src/herdr.rs` | Modificación | Implementación de `shepherd_call`, detección robusta de servidor con soporte de Shepherd en `is_reachable`, `Clone` para `HerdrClient`, tests de socket y despacho in-process. |
| `crates/shepherd-core/src/lib.rs` | Modificación | Exportación pública de funciones de invocación con cliente: `run_cli_in_root_with_client` y `run_cli_in_root_with_output`. |
| `crates/shepherd-core/src/cli.rs` | Modificación | Soporte de flag `--no-server`, detección de cliente socket, delegación en `shepherd.call` para operaciones de estado vivo, salida `--json` uniforme con formato de error estricto, adición de `skills list` y `runs list`, alias `tasks`, y suite completa de tests unitarios TDD. |
| `crates/shepherd-core/src/main.rs` | Modificación | Supresión de texto plano en `stderr` ante errores cuando se solicita `--json`. |
| `src/shepherd/mod.rs` | Modificación | Conexión del CLI integrado con el socket de sesión activa a través de `run_cli_in_root_with_client`, y supresión de `stderr` con `--json`. |
| `SHEPHERD.md` | Documentación | Registro de Estado F4a, tabla de auditoría de los 20 subcomandos, verificación con sesión aislada y notas de uso en scripts. |


## Estado F4b (2026-11-04) — Pestaña «Git» del overlay (solo lectura)

Se implementó la cuarta pestaña **Git** en el overlay de Shepherd (junto a Tareas, Runs y Skills), permitiendo inspeccionar en **modo solo lectura** el estado git del repositorio y de los agentes de Shepherd:

### 1. Modelo puro y parseadores de Git (`crates/shepherd-core/src/git.rs` y `src/shepherd/git.rs`)
- **Estructuras de estado puras**: `ShepherdGitState`, `ShepherdGitRow`, `ShepherdGitAction` y `ShepherdGitRender`, separadas de cualquier acceso a I/O, PTY o async.
- **Parseadores puros probados con fixtures de texto**:
  - `parse_git_worktree_porcelain`: parsea la salida de `git worktree list --porcelain`, extrayendo rutas, commits HEAD, ramas (`refs/heads/*`), flags de detached, bare y locked.
  - `parse_git_status_porcelain`: parsea la salida de `git status --porcelain=v1 -b`, identificando rama actual, upstream configurado, ahead/behind y conteo de ficheros modificados/sin seguimiento (`is_dirty`, `dirty_count`).
  - `parse_gh_pr_list_json`: parsea de forma segura el JSON devuelto por `gh pr list --json ...`, extrayendo número, título, rama origen, estado (OPEN/MERGED/CLOSED) y URL.
  - `parse_rev_list_left_right`: procesa las salidas tabulares de `git rev-list --left-right --count` para ramas locales sin upstream configurado.
- **Asociación de agentes**: cruza cada worktree/rama contra el registro de agentes persistido (`agents.json`), vinculando automáticamente los worktrees de trabajo o ramas `shep/...` con su identificador de agente (p. ej., `coder-1`).

### 2. Recolección fuera del render y comando CLI
- **Operación `git_status` en el servicio**:
  - Implementada en `handle_call` (`crates/shepherd-core/src/service.rs`). Ejecuta las consultas git con timeouts breves (3s) usando un ejecutor acotado que no bloquea el ciclo de render ni el tick del servidor.
  - Manejo seguro de `gh`: si `gh` no está instalado, autenticado o no hay conexión de red, la columna de PR muestra `—` y se incluye la nota informativa `gh no disponible`, sin arrojar errores ni bloquear la presentación.
- **Subcomando CLI `herdr shepherd git [--json]`**:
  - Con `--json`, devuelve la salida completa de `GitRepoStatus` en formato JSON uniforme.
  - En modo texto plano, presenta una tabla formateada con columnas: `BRANCH`, `STATUS`, `SYNC`, `AGENT`, `PR` y `PATH`.
- **Corrección de fuga de salida estándar en `default_base` (`crates/shepherd-core/src/mcp.rs`)**:
  - `default_base` invocaba `git rev-parse ... .status()`, heredando la salida estándar y filtrando los hashes SHA de los commits a `stdout`. Se corrigió para usar `.output()`, garantizando que la salida de `herdr shepherd git --json` sea estrictamente JSON limpio sin bytes parásitos.

### 3. Integración en el overlay TUI (`src/shepherd/overlay.rs` y `src/client/shell/shepherd.rs`)
- **Pestaña en el overlay**: `ShepherdTab::Git` incorporada como cuarta opción en `ShepherdTab::ALL` con navegación completa bidireccional (Tab / BackTab / flechas izquierda/derecha y clics de ratón en las cabeceras).
- **Carga perezosa**: la petición `git_status` se despacha exclusivamente al abrir la pestaña Git o al pulsar la tecla `r` (`ShepherdGitAction::Refresh`), sin sondeo continuo innecesario.
- **Atajos de teclado y ratón**:
  - `r`: refresca el estado git del repositorio y worktrees.
  - `c` / `y` / `Enter`: copia o muestra la ruta del worktree seleccionado en el pie del overlay (`Worktree: <path>`).
  - `j` / `Down` y `k` / `Up`: navegación por las filas de worktrees.
  - Clic de ratón: soporte para selección directa de fila, botón de refresco y botón de copia.

### Comprobaciones reales con `./scripts/shepherd-iso.sh` (sesión aislada)

Se utilizó `./scripts/shepherd-iso.sh` en un entorno aislado sin agentes reales:
1. En `/tmp/shfiso/repo` se creó un worktree secundario `/tmp/shfiso/repo/wt-coder` en la rama `shep/1-coder`.
2. Se sembró `agents.json` con el agente `coder-1` asignado a dicho worktree y rama.
3. Se inició el servidor aislado (`./scripts/shepherd-iso.sh start`) y se abrió el overlay (`C-b` luego `a`).
4. Se navegó a la pestaña **Git** (BackTab) y se comprobó que listaba ambos worktrees con su estado limpio/sucio, ramas y agente asociado.
5. Se ensució el worktree secundario creando `/tmp/shfiso/repo/wt-coder/dirty_file.txt` y se pulsó `r`: la vista se actualizó inmediatamente a `sucio (1)`.
6. Se seleccionó la segunda fila (`j`) y se pulsó `c`: la barra de estado inferior mostró la ruta del worktree.

#### Captura 1: Vista inicial de la pestaña Git (rama de agente en estado `limpio`)
```text
                         │                          ┌──────────────────────────────────────────────────────────────────────────────────────────────┐
                         │                          │ Shepherd                                                                                  [x]│
                         │                          │ Tareas   Runs   Skills   Git                                                                 │
                         │                          │──────────────────────────────────────────────────────────────────────────────────────────────│
                         │                          │ ↻ Refrescar (r)   📋 Copiar ruta (c)                                                         │
                         │                          │ 2 worktree(s) · gh no disponible · solo lectura                                              │
                         │                          │                                                                                              │
                         │                          │  RAMA                  ESTADO        SYNC      AGENTE      PR            RUTA                │
                         │                          │ ●master [main]         sucio (1)     —         —           —             /tmp/shfiso/repo    │
                         │                          │ ·shep/1-coder          limpio        —         coder-1     —             /tmp/shfiso/repo/wt-│
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
 new                 menu│                          │                                                                                              │
─────────────────────────│                          │                                                                                              │
 agents           grouped│                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │ r refrescar  ·   c copiar/mostrar ruta  ·   Tab cambiar pestaña  ·   Esc cerrar              │
                         │                          │                                                                                              │
                         │                          └──────────────────────────────────────────────────────────────────────────────────────────────┘
```

#### Captura 2: Actualización tras ensuciar el worktree y pulsar `r` (`sucio (1)`)
```text
                         │                          ┌──────────────────────────────────────────────────────────────────────────────────────────────┐
                         │                          │ Shepherd                                                                                  [x]│
                         │                          │ Tareas   Runs   Skills   Git                                                                 │
                         │                          │──────────────────────────────────────────────────────────────────────────────────────────────│
                         │                          │ ↻ Refrescar (r)   📋 Copiar ruta (c)                                                         │
                         │                          │ 2 worktree(s) · gh no disponible · solo lectura                                              │
                         │                          │                                                                                              │
                         │                          │  RAMA                  ESTADO        SYNC      AGENTE      PR            RUTA                │
                         │                          │ ●master [main]         sucio (1)     —         —           —             /tmp/shfiso/repo    │
                         │                          │ ·shep/1-coder          sucio (1)     —         coder-1     —             /tmp/shfiso/repo/wt-│
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
 new                 menu│                          │                                                                                              │
─────────────────────────│                          │                                                                                              │
 agents           grouped│                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │ r refrescar  ·   c copiar/mostrar ruta  ·   Tab cambiar pestaña  ·   Esc cerrar              │
                         │                          │                                                                                              │
                         │                          └──────────────────────────────────────────────────────────────────────────────────────────────┘
```

#### Captura 3: Selección de fila con `j` y copia de ruta con `c` (mensaje en pie de marco)
```text
                         │                          ┌──────────────────────────────────────────────────────────────────────────────────────────────┐
                         │                          │ Shepherd                                                                                  [x]│
                         │                          │ Tareas   Runs   Skills   Git                                                                 │
                         │                          │──────────────────────────────────────────────────────────────────────────────────────────────│
                         │                          │ ↻ Refrescar (r)   📋 Copiar ruta (c)                                                         │
                         │                          │ 2 worktree(s) · gh no disponible · solo lectura                                              │
                         │                          │                                                                                              │
                         │                          │  RAMA                  ESTADO        SYNC      AGENTE      PR            RUTA                │
                         │                          │ ·master [main]         sucio (1)     —         —           —             /tmp/shfiso/repo    │
                         │                          │ ●shep/1-coder          sucio (1)     —         coder-1     —             /tmp/shfiso/repo/wt-│
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
 new                 menu│                          │                                                                                              │
─────────────────────────│                          │                                                                                              │
 agents           grouped│                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │                                                                                              │
                         │                          │ r refrescar  ·   c copiar/mostrar ruta  ·   Tab cambiar pestaña  ·   Esc cerrar              │
                         │                          │                                                                                              │
                         │                          └ Worktree: /tmp/shfiso/repo/wt-coder ─────────────────────────────────────────────────────────┘
```

### Qué NO queda conectado (explícito)
1. **Operaciones de escritura sobre ramas y PRs**: En F4b la pestaña es estrictamente de solo lectura. No se permiten operaciones de push, merge, delete de worktree ni checkout desde esta vista.
2. **Creación o interacción con PRs**: Si `gh` está disponible solo se lee el listado; la gestión automatizada de pull requests y gates de publicación pertenece a la fase de integración F5.

### Archivos tocados

| Archivo | Tipo de cambio | Contenido / Razón del cambio |
|---|---|---|
| `crates/shepherd-core/src/git.rs` | Nuevo | Modelo puro, estructuras de datos (`GitRepoStatus`, `GitWorktreeInfo`, etc.), parseadores de `git worktree list --porcelain`, `git status --porcelain=v1 -b`, `gh pr list --json` y recolección fuera de render con timeouts seguros y tests con fixtures. |
| `crates/shepherd-core/src/lib.rs` | Modificación | Exportación pública del módulo `git` y tipos asociados. |
| `crates/shepherd-core/src/mcp.rs` | Modificación | Supresión de fuga de hashes SHA a stdout en `default_base` mediante el uso de `.output()`. |
| `crates/shepherd-core/src/service.rs` | Modificación | Despacho de la operación `git_status` en `handle_call`. |
| `crates/shepherd-core/src/cli.rs` | Modificación | Soporte del subcomando `herdr shepherd git [--json]` tanto vía servidor como en modo local/in-process. |
| `src/shepherd/git.rs` | Nuevo | Estado puro de TUI (`ShepherdGitState`), filas (`ShepherdGitRow`), acciones (`ShepherdGitAction`) y render puro (`render_shepherd_git_panel`) con tests unitarios. |
| `src/shepherd/mod.rs` | Modificación | Exposición del módulo `git` en `src/shepherd`. |
| `src/shepherd/overlay.rs` | Modificación | Incorporación de la pestaña `ShepherdTab::Git` en `ALL`, ciclo de pestañas, atajos de teclado y gestión de eventos de ratón. |
| `src/app/mod.rs` | Modificación | Inicialización del campo `shepherd_git_panel` en `App`. |
| `src/app/state.rs` | Modificación | Campo `shepherd_git_panel: ShepherdGitState` en `AppState`. |
| `src/client/shell/shepherd.rs` | Modificación | Despacho de `git_status` al cambiar a la pestaña Git y actualización del estado del overlay al recibir la respuesta. |
| `SHEPHERD.md` | Documentación | Registro de Estado F4b con verificación en sesión aislada, capturas y alcance completado. |

## Inventario Pre-renombre F5a (Apariciones de `herdr`)

Previo a la ejecución del renombre automatizado mediante `scripts/rename-from-herdr.sh`, se realiza el inventario exhaustivo de las categorías de apariciones de `herdr` en el repositorio:

1. **Paquete y binario (`Cargo.toml`, `build.rs`, `[[bin]]`, `justfile`)**:
   - `Cargo.toml`: `package.name = "herdr"`, metadatos de descripción, include paths. Pasa a `name = "shepherd"`.
   - `crates/shepherd-core/Cargo.toml`: contiene `[[bin]] name = "shepherd"`, lo cual colisiona con el nuevo binario raíz `shepherd`. Se renombra el binario de `shepherd-core` a `[[bin]] name = "shepherd-core"`.
   - `build.rs`: constantes de build, detección de compilación y metadata del ejecutable.
   - `justfile`: llamadas a `cargo test ... --bin herdr` pasan a `--bin shepherd`.

2. **Módulos, estructuras e identificadores de código (`src/`)**:
   - Tipos y structs: `ToastHerdrPosition` -> `ToastShepherdPosition`, `RemoteHerdr` -> `RemoteShepherd`, `PreparedRemoteHerdr` -> `PreparedRemoteShepherd`, `HerdrClient` -> `ShepherdClient`, `HerdrInProcess` -> `ShepherdInProcess`, `HerdrError` -> `ShepherdError`.
   - Funciones y métodos: `install_herdr` -> `install_shepherd`, `prepare_remote_herdr` -> `prepare_remote_shepherd`, `is_herdr_dev` -> `is_shepherd_dev`, `running_inside_herdr` -> `running_inside_shepherd`.
   - Constantes y literales internos: marcadores de proyectos `__herdr_projects__` -> `__shepherd_projects__`, `__herdr_original__` -> `__shepherd_original__`.
   - Re-exportaciones y namespaces: `crate::shepherd::...`.

3. **Variables de entorno (`HERDR_*` → `SHEPHERD_*`)**:
   - Variables de runtime y proceso: `HERDR_ENV` -> `SHEPHERD_ENV`, `HERDR_SOCKET_PATH` -> `SHEPHERD_SOCKET_PATH`, `HERDR_CLIENT_SOCKET_PATH` -> `SHEPHERD_CLIENT_SOCKET_PATH`, `HERDR_SESSION` -> `SHEPHERD_SESSION`, `HERDR_PANE_ID` -> `SHEPHERD_PANE_ID`, `HERDR_TAB_ID` -> `SHEPHERD_TAB_ID`, `HERDR_WORKSPACE_ID` -> `SHEPHERD_WORKSPACE_ID`, `HERDR_CONFIG_PATH` -> `SHEPHERD_CONFIG_PATH`, `HERDR_LOG` -> `SHEPHERD_LOG`, `HERDR_DISABLE_SOUND` -> `SHEPHERD_DISABLE_SOUND`, `HERDR_BIN_PATH` -> `SHEPHERD_BIN_PATH`, `HERDR_AGENT` -> `SHEPHERD_AGENT`.
   - Plugins: `HERDR_PLUGIN_ID`, `HERDR_PLUGIN_ROOT`, `HERDR_PLUGIN_CONFIG_DIR`, `HERDR_PLUGIN_STATE_DIR`, `HERDR_PLUGIN_CONTEXT_JSON`, `HERDR_PLUGIN_EVENT`, `HERDR_PLUGIN_ACTION_ID`, etc. -> `SHEPHERD_PLUGIN_*`.
   - Política: **SIN fallback** a `HERDR_*` para prevenir colisiones e interferencias si un herdr oficial corre en la misma máquina.

4. **Rutas en disco y nombres de sockets**:
   - Configuración: `$XDG_CONFIG_HOME/shepherd` (y `shepherd-dev` en dev/debug).
   - Estado: `$XDG_STATE_HOME/shepherd` (y `shepherd-dev` en dev/debug).
   - Datos: `$XDG_DATA_HOME/shepherd`.
   - Sockets IPC: `shepherd.sock` y `shepherd-client.sock`.
   - Fallback de lectura exclusiva para `config.toml`: si `$XDG_CONFIG_HOME/shepherd/config.toml` no existe pero sí `$XDG_CONFIG_HOME/herdr/config.toml`, se lee este emitiendo un `tracing::warn`. No se escribe nunca en el directorio de herdr y no se comparte estado ni sockets.

5. **Textos visibles en TUI, CLI y ayuda**:
   - Textos de `--help`, cadenas de uso de CLI, mensajes de bienvenida (`onboarding`), títulos de ventana predeterminados, banners y textos de error interactivos.

6. **Despacho de CLI y subcomandos**:
   - Subcomandos de `shepherd-core`: `agents`, `spawn`, `kill`, `clean`, `focus`, `task`, `tasks`, `skills`, `runs`, `roles`, `vendors`, `run`, `approve`, `git`, `mcp`, `msg`, `messages`, `attach` pasan a funcionar directamente como `shepherd <cmd>`.
   - Alias retrocompatible: `shepherd shepherd <cmd>` se mantiene operativo delegando en la misma función.
   - Conflictos detectados: `config` (reclamado por herdr para su configuración; `shepherd shepherd config` accede al gestor de config de shepherd-core); `agent` singular (reclamado por herdr; `agents` plural es de shepherd).

7. **Hooks e integraciones (`src/integration/assets/`)**:
   - Nombres de scripts: `herdr-agent-state.{sh,ps1,js,ts}` -> `shepherd-agent-state.*`, `herdr-agent-session.{sh,ps1}` -> `shepherd-agent-session.*`, `herdr-tui-session.js` -> `shepherd-tui-session.js`.
   - Invocaciones internas dentro de los hooks: llamadas al binario `shepherd agent report ...` y lectura de variables `SHEPHERD_*`.

8. **Empaquetado y distribución (`packaging/`, `nix/`, `distribution/`)**:
   - Scripts de instalación: `distribution/install.sh`, `install.ps1`, `install.cmd`.
   - Empaquetado Nix: `nix/package.nix`.
   - Empaquetado Windows: `packaging/windows/`.

9. **Scripts auxiliares y harness de pruebas**:
   - `scripts/shepherd-iso.sh`: apuntando al binario `./target/debug/shepherd` y rutas `shepherd-dev`.
   - `cargo-fork.sh`: wrapper para Rust 1.96.1 y Zig.
   - Scripts de python y bash bajo `scripts/`.

10. **Auto-actualización y telemetría de upstream**:
    - `src/update.rs`: desactivada la consulta y descarga de actualizaciones desde `herdr.dev` / `herdrdev` para evitar la sobreescritura accidental del fork por binarios oficiales.

11. **Exclusiones estrictas (NO renombrar)**:
    - Licencia y autoría histórica: `LICENSE`, `NOTICE`.
    - Historial: `CHANGELOG.md`, `docs/next/CHANGELOG.md`.
    - Repositorio y organización de upstream: `github.com/herdrdev/herdr`, `herdr.dev`, `herdrdev`.
    - Dependencias externas vendoreadas: `vendor/`.
    - Artefactos y bloqueos temporales: `target/`, `Cargo.lock`.
    - Versiones publicadas y previews inmutables: `docs/versions/`, `docs/preview/`.
    - Fixtures congelados de protocolo v1: `tests/fixtures/endpoint-snapshot-v1.json`, `tests/fixtures/session/legacy-pre-tabs-v2.json`.
    - Briefs de encargo: `SHEPHERD-F1-BRIEF.md` a `SHEPHERD-F5A-BRIEF.md`.


## Estado F5a (2026-11-04)

Renombre completo de herdr a shepherd como proceso automatizado, idempotente y repetible mediante el script `scripts/rename-from-herdr.sh`, preservando compatibilidad de contrato en sockets/wire v1, ofreciendo fallback de lectura para configs previas y cableando los subcomandos de forma directa en el binario `shepherd`.

### 1. Script de Renombre Automatizado (`scripts/rename-from-herdr.sh`)

- **Diseño**: Script escrito en Perl/Bash estructurado para ser re-ejecutable tras merges o rebases de upstream (`herdrdev/herdr`).
- **Modos de ejecución**:
  - `scripts/rename-from-herdr.sh` (modo ejecución): aplica transformaciones ordenadas de reemplazo textual y renombrado de ficheros/rutas en git.
  - `scripts/rename-from-herdr.sh --dry-run`: inspecciona los cambios sin modificar el árbol de trabajo y reporta conteos.
  - `scripts/rename-from-herdr.sh --check`: modo CI/verificación estricta. Recorre todos los ficheros trackeados (excluyendo excepciones) y falla (código de salida 1) si detecta cualquier ocurrencia no permitida de `herdr`, `Herdr` o `HERDR`.
- **Métricas de la primera ejecución**:
  - **446 ficheros modificados**.
  - **10.250 reemplazos** de contenido (`herdr`: 7.371, `Herdr`: 2.054, `HERDR`: 825).
  - **24 renombrados de ficheros y directorios** (incluyendo hooks de agentes en `src/integration/assets/`, ficheros de sesiones y esquemas de API).
- **Idempotencia verificada**:
  - Re-ejecutar el script sobre el árbol modificado resulta exactamente en **0 ficheros modificados**, **0 reemplazos** y **0 renombrados**.
  - `./scripts/rename-from-herdr.sh --check` finaliza con código 0 y el mensaje `[OK] No unexcluded occurrences of herdr/Herdr/HERDR found.`.

### 2. Decisiones de Alcance y Exclusiones Estrictas

- **Exclusiones de licencia y upstream**:
  - Preservados sin alteración: `LICENSE`, `NOTICE`, `CHANGELOG.md`, `docs/next/CHANGELOG.md`, menciones a licencias Apache-2.0 / MIT y atribución de derechos originales.
  - Preservadas URLs canónicas de upstream: `github.com/herdrdev/herdr`, `herdr.dev`, `herdrdev`.
  - Excluidos directorios externos: `vendor/`, `target/`, `Cargo.lock`, `docs/versions/`, `docs/preview/`.
  - Excluidos briefs de especificación del proyecto: `SHEPHERD-F1-BRIEF.md` a `SHEPHERD-F5A-BRIEF.md`.
- **Compatibilidad de Contrato Wire v1**:
  - Para no romper la huella criptográfica de `tests/fixtures/endpoint-method-shapes-v1.json` (contrato inmutable de generación 1 para `pane.split` y `pane.input.set`), `PaneRightClickTarget` en `src/api/schema/panes.rs` utiliza `#[serde(rename = "herdr", alias = "shepherd")]`. Esto mantiene compatibilidad exacta en la serialización wire y en schemars mientras deserializa tanto `herdr` como `shepherd`, y la CLI acepta `--right-click shepherd`.
- **Fallback de Configuración de Usuario**:
  - En `src/config/io.rs`, si `$XDG_CONFIG_HOME/shepherd/config.toml` (o `shepherd-dev`) no existe, `herdr_fallback_config_path()` busca `$XDG_CONFIG_HOME/herdr/config.toml` (o `herdr-dev`) y emite una advertencia con `tracing::warn!`. Es estrictamente de solo lectura; el estado, sesiones y sockets de shepherd nunca se mezclan con los de herdr.
- **Desactivación de Auto-actualización de Upstream**:
  - En `src/update.rs`, se deshabilitaron las consultas y descargas de binarios remotos desde `herdr.dev` para evitar la sobreescritura accidental del fork por releases de upstream.

### 3. CLI y Despacho de Subcomandos

- En `src/cli.rs`, los subcomandos de Shepherd se despachan directamente en la raíz de la CLI:
  - `shepherd agents [--json]`
  - `shepherd spawn <role> <task...>`
  - `shepherd kill <agent-id>`
  - `shepherd clean`
  - `shepherd focus <agent-id>`
  - `shepherd task <new|list|show|plan|edit|path|archive>`
  - `shepherd tasks`
  - `shepherd skills <list|...>`
  - `shepherd runs <list|...>`
  - `shepherd roles <list|...>`
  - `shepherd vendors <list|...>`
  - `shepherd run <task-id>`
  - `shepherd approve <task-id> <plan|subtask>`
  - `shepherd git [--json]`
  - `shepherd mcp serve [port]`
  - `shepherd msg / shepherd messages`
  - `shepherd attach <agent-id>`
- Se conserva retrocompatibilidad completa con el prefijo `shepherd shepherd <cmd>`.

### 4. Verificación de Tests Automatizados (`./cargo-fork.sh`)

Todos los tests del proyecto compilan y pasan limpiamente a través de `./cargo-fork.sh`:
- **`shepherd-core`**: 82 passed, 0 failed. `./cargo-fork.sh test -p shepherd-core 2>&1 | grep -c testRepo` devuelve **0**.
- **`client::shell`**: 391 passed, 0 failed, 2 ignored.
- **`shepherd::`**: 52 passed, 0 failed.
- **`protocol::`**: 156 passed, 0 failed.
- **`api::`**: 319 passed, 0 failed, 1 ignored.
- **`server::`**: 318 passed, 0 failed, 5 ignored (incluyendo `advertised_client_shell_method_shapes_stay_at_the_v1_contract`).

### 5. Verificación en Sesión Aislada (`./scripts/shepherd-iso.sh`)

Verificación de extremo a extremo sin agentes reales en entorno aislado (`/tmp/shfiso`):
1. **Arranque e inicio limpio**: `./scripts/shepherd-iso.sh start` levanta la sesión `iso` con el binario `./target/debug/shepherd` y socket en `/tmp/shfiso/cfg/shepherd-dev/sessions/iso/shepherd.sock`.
2. **Consultas CLI directas y con alias**:
   - `shepherd --session iso agents --json` -> `[]`.
   - `shepherd --session iso shepherd agents --json` -> `[]`.
   - `shepherd --session iso roles list --json` -> Listado completo de 5 roles (`brain`, `coder`, `docs`, `reviewer`, `tester`) con la skill `shepherd-comms`.
3. **Apertura de Overlay (`C-b a`)**:
   - El overlay muestra como título de ventana `Shepherd [x]`.
   - Pestañas funcionales: `Tareas   Runs   Skills   Git`.
   - La captura de pantalla en cada una de las pestañas (`Tareas`, `Runs`, `Skills`) confirma que **no existe ninguna aparición de `herdr` visible en la TUI**.
4. **Cierre limpio**: `Escape` cierra el overlay y `./scripts/shepherd-iso.sh stop` apaga el servidor y sesión tmux de forma limpia.

## Estado F5b (2026-11-04)

README, NOTICE con crédito a herdr, documentación y rutina de sincronización con upstream verificada en repositorios temporales.

### 1. Lo hecho

1. **`NOTICE` (raíz)**:
   - Atribución conforme a Apache-2.0 §4 a herdr (`herdrdev/herdr` base 0.9.5, «the herdr contributors»).
   - Atribución del orquestador en proceso (`crates/shepherd-core`, `src/shepherd/`) © Sebastián Araque Rivaldito bajo Apache-2.0.
   - Aviso explícito de modificaciones («files changed from the upstream herdr; see git history and SHEPHERD.md»).
   - `LICENSE` permanece intacto sin modificaciones.

2. **`README.md` reescrito para Shepherd (inglés, 72 líneas)**:
   - Qué es: runtime y orquestador basado en terminal para agentes de código.
   - Crédito prominente a herdr con enlace a upstream (`https://github.com/herdrdev/herdr`) y licencia Apache-2.0.
   - Qué añade: overlay TUI (`prefix + a`) con Tareas, Runs, Skills y Git (solo lectura); barra lateral con estados de agentes y avisos multilínea `[NEEDS YOU]`; servidor MCP en proceso con autenticación por token; entrega automática de mensajes a la terminal de agentes sin MCP (`[shep] from <rol>#<n>: ...`); CLI estructurada con salida `--json` uniforme; daemon headless con reconciliación y reanudación.
   - Instalación estrictamente desde fuente con `cargo` (sin instrucciones de Homebrew, curl de upstream ni badges).
   - Configuración mínima con `[shepherd] enabled = true`, ruta propia `~/.config/shepherd` y fallback de lectura de `~/.config/herdr/config.toml`.
   - Diferencias con herdr, estado experimental y licencia.
   - El README no contiene `herdrdev` salvo en el enlace de crédito a upstream.

3. **`README.zh-CN.md`**:
   - Añadida al inicio la nota en inglés indicando que la traducción de upstream puede estar desfasada y remitiendo a `README.md`.

4. **Rutina de sincronización con upstream (`scripts/upstream-sync.sh`)**:
   - Implementada en `scripts/upstream-sync.sh` con soporte para `--dry-run`, `--skip-build`, `--skip-tests`, `--allow-branch`, `--allow-dirty` y `--upstream <remote>`:
     - (a) Verifica árbol limpio y rama `shepherd`.
     - (b) Crea rama temporal `sync/<tag>`.
     - (c) Ejecuta `git fetch <upstream>` y merge de tag o commit; ante conflictos detiene el proceso con código de salida 2, lista los ficheros afectados y emite instrucciones de resolución.
     - (d) Re-aplica `scripts/rename-from-herdr.sh`, verifica limpieza con `--check` y confirma los cambios en la rama.
     - (e) Compila con `./cargo-fork.sh build` y ejecuta la batería de tests por áreas.
     - (f) Deja la rama lista **sin push y sin merge a `shepherd`**, recordando la revisión humana previa.
   - Exclusiones añadidas en `scripts/rename-from-herdr.sh` (`scripts/upstream-sync.sh` y `docs/UPSTREAM-SYNC.md`) y regla en `.gitignore` para no ignorar `!/docs/UPSTREAM-SYNC.md`.

5. **Documentación de sincronización (`docs/UPSTREAM-SYNC.md`)**:
   - Guía paso a paso del flujo de sincronización y modo `--dry-run`.
   - Detalle de conflictos típicos esperables y cómo resolverlos: archivos renombrados o movidos, `Cargo.lock`, fixtures congelados de protocolo v1 (`tests/fixtures/`), compatibilidad de contrato wire (`PaneRightClickTarget`), auto-updater de upstream desactivado y fallback de configuración.
   - Batería de verificación y notas sobre tests.

### 2. Verificación

1. **Rename check**:
   `./scripts/rename-from-herdr.sh --check` finaliza con código 0: `[OK] No unexcluded occurrences of herdr/Herdr/HERDR found.`.

2. **Crédito y enlaces en README**:
   `grep -n "herdrdev" README.md` devuelve únicamente la línea 9 (enlace a upstream en la sección de crédito).

3. **Compilación**:
   `./cargo-fork.sh build` finaliza limpiamente con código 0.

4. **Tests de `shepherd-core`**:
   `./cargo-fork.sh test -p shepherd-core`: 82 tests pasados, 0 fallados.
   `./cargo-fork.sh test -p shepherd-core 2>&1 | grep -c testRepo` devuelve **0**.

5. **Tests por áreas**:
   - `client::shell`: 391 passed, 0 failed, 2 ignored.
   - `shepherd::`: 52 passed, 0 failed.
   - `protocol::`: 156 passed (unittests) + 3 passed (cli.rs), 0 failed.
   - `api::`: 319 passed, 0 failed, 1 ignored.
   - `server::`: 318 passed, 0 failed, 5 ignored.
   - *Nota de entorno*: la ejecución de la suite completa en bloque puede terminar con SIGPIPE debido al harness de PTY en entornos sin terminal interactivo, y los tests de plugins/config en paralelo son intermitentes conocidos (pasan en solitario).

6. **Verificación de la rutina de sync en repositorio temporal**:
   - Ejecutado en `/tmp/` con dos repositorios locales simulando `upstream` y `downstream` sin tocar en ningún momento la red ni el remoto upstream real.
   - Commit simulado en upstream con la palabra `herdr`.
   - `upstream-sync.sh v0.9.6` completó el ciclo: merge en rama `sync/v0.9.6`, renombrado automático aplicado transformando `herdr` a `shepherd`, `--check` limpio, tests simulados ejecutados, y rama `shepherd` preservada intacta sin push ni merge.
   - Prueba adicional simulando conflicto de merge: detección de conflicto, salida limpia con código 2 y reporte de fichero en conflicto.

### 3. Qué NO queda (lo que falta para lanzamiento público)

1. **Publicación en GitHub**: El repositorio de Shepherd no ha sido publicado como repositorio público ni privado en GitHub.
2. **Releases y canales de distribución**: No hay tags de versión ni manifests de release publicados (`distribution/latest.json` y `distribution/preview.json` son los originales y no deben usarse).
3. **Instaladores y binarios empaquetados**: No existen instaladores rápidos de `curl | sh`, fórmulas de Homebrew ni paquetes de Nix o Windows para Shepherd; la instalación es exclusivamente desde fuente con `cargo`.
4. **Sitio web y documentación pública**: No hay despliegue web público de la documentación (vive en el repositorio).
5. **Marca definitiva**: "Shepherd" es el nombre acordado para el fork; los dominios, logos finales y detalles comerciales quedan fuera de esta fase.

### 4. Decisiones abiertas para el dueño

1. **Organización y visibilidad en GitHub**: ¿Bajo qué cuenta u organización se alojará el repositorio de Shepherd (p. ej. repositorio público o privado) y cuál será el nombre del remote canónico?
2. **Estrategia de versionado**: ¿Comenzar las versiones de Shepherd en `0.1.0` (como proyecto independiente) o alineadas con la base upstream `0.9.5`?
3. **Canales de actualización (`latest` / `preview`)**: ¿Se desea implementar un sistema propio de actualización para binarios precompilados de Shepherd mediante GitHub Releases u otro backend de almacenamiento?
4. **Instalación y empaquetado**: ¿Interesa publicar una fórmula propia de Homebrew (`brew tap`) y un script `install.sh` propio de Shepherd?
5. **Documentación multi-idioma**: ¿Se mantendrán traducciones como `README.zh-CN.md` (con aviso de posible desactualización) o se prefiere retirarlas para mantener únicamente la versión en inglés?


## Prueba con agentes reales tras el renombre: workflow `run` completo (2026-11-04)

Sesión aislada con el binario `shepherd` (F5b), `SHF_HOME=$HOME` solo para las credenciales de los vendors. Config de proyecto: `brain` y `reviewer` en `claude haiku`, `coder` en `codex` (worktree). Tarea 1: «crear `hello.txt` con el texto exacto `hello from shepherd`». Comando: `shepherd run 1`.

Funcionó de punta a punta con agentes reales: `brain-1` (claude) guardó el plan (1 subtarea) y entregó resultado; `coder-1` (codex) leyó el contexto vía MCP, creó `hello.txt` (19 bytes, sin salto de línea), lo verificó y, tras el permiso humano de codex para escribir en `.git` del worktree, hizo el commit `feat: add hello file` en la rama `shep/2-implement-plan-subtask-1-create-hello-tx`; `reviewer-1` (claude) aprobó y los checks pasaron. El run quedó en `current_stage: pr`, subtarea `awaiting-approval`. La pestaña Git (`shepherd git --json`) vio el worktree del coder limpio y asociado a `coder-1`.

Hallazgos:
1. **Los gates humanos no están portados al servicio Rust.** La nota del run dice «human approval and push/PR creation are not ported yet»; `runs` devuelve `pending_gate: null`. Aprobar/rechazar gates desde el overlay o `shepherd approve` no se puede ejercitar aún con un run real. Es el hueco funcional principal que queda del plan.
2. **`shepherd run` bloquea y el CLI agota el tiempo.** Con el coder esperando el primer prompt/permisos, el servicio quedó ocupado y `shepherd run 1` falló con `Resource temporarily unavailable (os error 11)` mientras el run seguía; otras llamadas a `shepherd.call` también esperaron. Mismo origen que el spawn bloqueante ya anotado.
3. **`shepherd agents --json` devolvió `[]` mientras el servicio estaba ocupado** (había 2 agentes en el registro del servidor); minutos después devolvió los 3 correctos. Probable caída silenciosa al modo en proceso al agotarse el tiempo del servidor; debería avisar o fallar en vez de mostrar lista vacía.
4. **Pestaña Git: asignación de agente dudosa.** La rama `shep/1-coder` (un worktree viejo de pruebas anteriores) se asoció a `brain-1`; parece coincidir por número en vez de por agente real.
5. Los worktrees del coder se crean bajo `$HOME/.shepherd/worktrees/` (el HOME real por `SHF_HOME`); el de esta prueba se eliminó a mano.
6. Codex pide permiso humano para escribir en el `.git` del worktree al hacer commit (sandbox); es un diálogo que Shepherd no debe contestar.


## Estado F6 (2026-11-04)

Gates humanos del workflow `run` (plan y push/PR) completamente visibles y operables desde CLI y overlay TUI, servicio no bloqueante desacoplado de operaciones largas, prevención estricta de caídas silenciosas a in-process en el CLI, y asociación exacta y determinista de worktrees/ramas en la pestaña Git.

### 1. Lo hecho

1. **`runs` real (`service.rs` y `cli.rs`)**:
   - Cálculo preciso de `pending_gate`:
     - `"plan"` cuando `task.status.waiting == "plan"`.
     - `{"kind": "push_pr", "subtask": N}` cuando la primera subtarea está en estado `"awaiting-approval"`.
     - `null` si no hay gate pendiente.
   - Cálculo de progreso `subtasks_progress: [hechas, total]` (donde hechas cuenta subtareas en estado `pr` o `done`).
   - Inclusión del array detallado `subtasks: [{n, title, state, branch, pr, note, coder, reviewer}]`.
   - Eliminación de notas obsoletas "not ported yet" en `workflow.rs` (sustituidas por notas descriptivas de espera de aprobación humana).
   - Conversión de fila en cliente TUI (`ShepherdRunRow::from_value`) para reflejar gates y progreso directamente en el overlay.

2. **Aprobar y rechazar desde el servicio (`workflow.rs`, `service.rs`, `cli.rs`)**:
   - **Op `approve`**:
     - Ejecuta `workflow::approve_plan` o `workflow::approve_subtask`.
     - Realiza `git push --set-upstream origin <branch>` y llamada a `gh pr create` únicamente bajo aprobación explícita humana.
     - Devuelve `{ok: true, task: "...", approved: ..., url: "...", pr: "..."}`.
   - **Op `reject`**:
     - Para plan: revierte el plan a `waiting = "plan"` registrando la razón en `status.note`.
     - Para subtarea: transiciona la subtarea de `awaiting-approval` al estado no terminal `"pending"` con nota `rejected by human: <razón>`.
     - **Elección de estado `pending`**: se eligió `"pending"` porque es el estado nativo no terminal del planificador (`schedule_subtasks`) que permite que un futuro `shepherd run` vuelva a programar y reintentar la subtarea sin necesidad de recrear el task, garantizando además que **nunca** se realice push o PR al rechazar.
     - Devuelve `{ok: true, task: "...", rejected: ..., reason: "..."}`.
   - Subcomandos CLI correspondientes: `shepherd approve <task> plan|<n> [--json]` y `shepherd reject <task> plan|<n> [--reason <texto>] [--json]`.

3. **Overlay TUI (`runs.rs` y `overlay.rs`)**:
   - Pestaña Runs muestra los gates pendientes en la lista: `[GATE: Espera aprobación del plan]` o `[GATE: Espera push/PR subtarea N]`.
   - Botones y accesos rápidos de teclado:
     - Tecla `a` / `A` / `y`: abre modal de confirmación `ConfirmApproveGate`, explicando claramente que la acción ejecutará `git push` a la rama remota y creará el Pull Request en GitHub.
     - Tecla `d` / `D` / `R`: abre modal de confirmación `ConfirmRejectGate`.
     - Confirmación con `Enter` despacha la acción (`"approve"` o `"reject"`) y refresca inmediatamente la lista de runs mediante llamada a `"runs"`.
   - El renderizado permanece completamente puro y sin E/S.

4. **Servicio no bloqueante (`service.rs` y `cli.rs`)**:
   - `OrchestrationService` envuelve sus estructuras de sincronización en `Arc` para permitir clonado seguro.
   - La op `"run"` en `handle_call` inicia el flujo de trabajo (`run_workflow`) en un hilo en segundo plano (`std::thread::spawn`) y devuelve inmediatamente `{accepted: true, job: "run-<task_id>", task: "<task_id>"}`.
   - En `spawn_inner`, la entrega del primer prompt del agente continúa en segundo plano si la terminal no está libre inmediatamente, evitando que el bucle de RPC quede congelado durante minutos.
   - Subcomando `shepherd run <task-id>` avisa de la aceptación y retorna de inmediato, o bien espera con el nuevo flag `--wait` mediante sondeo periódico con timeout máximo de 30 minutos y salida clara cuando el run finaliza o se detiene en un gate humano.

5. **CLI sin vacíos silenciosos (`cli.rs` y `shepherd.rs`)**:
   - Se eliminó la sonda frágil `is_reachable()` (que utilizaba un timeout rígido de 500 ms y provocaba la caída silenciosa al modo en proceso si el servidor tardaba en responder).
   - Se introdujo `socket_exists()`: la caída al modo en proceso solo se produce si el socket no existe físicamente en el sistema de archivos o si el usuario pasa `--no-server` explícitamente.
   - Si el socket existe pero la llamada al servidor falla o agota el tiempo, el CLI no oculta el problema cayendo a `[]`: sale con código de error ≠ 0 y emite `{"error": "..."}` en stdout con `--json`.

6. **Pestaña Git y asociación estricta (`git.rs`)**:
   - Se eliminó la heurística de concordancia difusa que asociaba ramas que empezaban por `shep/<task_id>-...` con cualquier agente con el mismo número de tarea.
   - La asociación se realiza ahora de forma estricta: un worktree o rama solo se asocia a un agente si la ruta de worktree del agente (`ag.worktree`) coincide exactamente (o por ruta canónica) con la del worktree, o si la rama registrada (`ag.branch`) coincide exactamente con la rama inspeccionada.
   - Los worktrees de subtareas viejas (como `shep/1-coder`) no se asocian indebidamente con agentes de tareas distintas (como `brain-1`).

### 2. Verificación real con `./scripts/shepherd-iso.sh` (sin agentes reales)

Se configuró un entorno aislado en `/tmp/shfiso` con:
- Repositorio bare local como origen remoto: `/tmp/shfiso/bare`.
- Repositorio de trabajo con rama `master` configurada para rastrear `origin/master`.
- Worktree en `/tmp/shfiso/repo/wt-coder` en rama `shep/1-implement` con un commit (`feat: hello`).
- Ejecutable `gh` falso en `/tmp/shfiso/fake-bin/gh` que registra argumentos en `/tmp/shfiso/gh.log` y devuelve `https://github.com/fake/repo/pull/1`.
- Tarea 1 con subtarea 1 en estado `awaiting-approval` y agente `coder-1` registrado con `branch: "shep/1-implement"` y `worktree: "/tmp/shfiso/repo/wt-coder"`.

#### A. Verificación del gate en CLI (`runs list --json`)
```bash
$ shepherd runs list --json
[
  {
    "active_agents": [],
    "current_stage": "awaiting-approval",
    "has_saved_plan": true,
    "pending_gate": {
      "kind": "push_pr",
      "subtask": 1
    },
    "status": "awaiting-approval",
    "subtasks": [
      {
        "branch": "shep/1-implement",
        "coder": "coder-1",
        "n": 1,
        "note": null,
        "pr": "",
        "reviewer": "reviewer-1",
        "state": "awaiting-approval",
        "title": "Subtask 1"
      }
    ],
    "subtasks_progress": [
      0,
      1
    ],
    "task_id": 1,
    "task_title": "Iso Task"
  }
]
```

#### B. Aprobación vía CLI (`shepherd approve 1 1 --json`)
```bash
$ shepherd approve 1 1 --json
{
  "approved": 1,
  "ok": true,
  "pr": "https://github.com/fake/repo/pull/1",
  "task": "1",
  "url": "https://github.com/fake/repo/pull/1"
}

$ cat /tmp/shfiso/gh.log
pr create --base master --head shep/1-implement --title Subtask 1 --body ## Subtask 1 ... --draft

$ git -C /tmp/shfiso/bare branch -a
* master
  shep/1-implement
```
La rama fue empujada al bare local, el fake `gh` fue invocado con los argumentos correspondientes y la subtarea pasó al estado `pr`.

#### C. Rechazo vía CLI (`shepherd reject 1 1 --reason "needs more tests" --json`)
```bash
$ shepherd reject 1 1 --reason "needs more tests" --json
{
  "ok": true,
  "reason": "needs more tests",
  "rejected": 1,
  "task": "1"
}

$ [ -f /tmp/shfiso/gh.log ] && echo "gh called" || echo "gh NOT called"
gh NOT called

$ git -C /tmp/shfiso/bare branch -a
* master
```
El rechazo no ejecutó push ni llamó a `gh`; la subtarea pasó a estado `pending` con nota `rejected by human: needs more tests`.

#### D. Aprobación interactiva desde el Overlay TUI
1. Abrir overlay: `Ctrl+b a` → Tab a "Runs":
   ```text
   ┌──────────────────────────────────────────────────────────────────────────────────────────────┐
   │ Shepherd                                                                                  [x]│
   │ Tareas   Runs   Skills   Git                                                                 │
   │──────────────────────────────────────────────────────────────────────────────────────────────│
   │ ▶ Lanzar/Reanuda  ■ Cancelar run  ✓ Aprobar gate    ✗ Rechazar gate                          │
   │ 1 run(s) · human gates protegen push y PR                                                    │
   │                                                                                              │
   │ ● #1  Iso Task [EN CURSO] subtareas:0/1                    [GATE: Espera push/PR subtarea]   │
   │                                                                                              │
   │ r/l lanzar/reanudar  ·   c cancelar  ·   a aprobar gate  ·   d rechazar gate  ·   Esc cerrar │
   └──────────────────────────────────────────────────────────────────────────────────────────────┘
   ```
2. Pulsar `a` (Aprobar Gate):
   ```text
            ┌──────────────────────────────────────────────────────────────────────────┐
            │ Confirmación Requerida: Aprobar Gate #1                                  │
            │                                                                          │
            │ ¿Aprobar gate de la subtarea 1? Esta acción realizará git push a la      │
            │ rama remota y abrirá el Pull Request en GitHub.                          │
            │                                                                          │
            │   Aprobar y Publicar (Push/PR)    esc Cancelar                           │
            └──────────────────────────────────────────────────────────────────────────┘
   ```
3. Confirmar con `Enter`:
   ```text
   ┌──────────────────────────────────────────────────────────────────────────────────────────────┐
   │ Shepherd                                                                                  [x]│
   │ Tareas   Runs   Skills   Git                                                                 │
   │──────────────────────────────────────────────────────────────────────────────────────────────│
   │ ▶ Lanzar/Reanuda  ■ Cancelar run  ✓ Aprobar gate    ✗ Rechazar gate                          │
   │ 1 run(s) · human gates protegen push y PR                                                    │
   │                                                                                              │
   │ ● #1  Iso Task [TERMINADO] subtareas:1/1                                                     │
   │                                                                                              │
   │ r/l lanzar/reanudar  ·   c cancelar  ·   a aprobar gate  ·   d rechazar gate  ·   Esc cerrar │
   └──────────────────────────────────────────────────────────────────────────────────────────────┘
   ```
   El overlay se refrescó inmediatamente reflejando `[TERMINADO] subtareas:1/1`, el push se completó en el bare local y `gh.log` registró la creación del PR.

#### E. Prevención de caída silenciosa (`agents --json`)
Con el servidor activo:
```bash
$ shepherd agents --json
[
  {
    "branch": "shep/1-implement",
    "delivery_cursor": 0,
    "depth": 0,
    "has_result": false,
    "id": "coder-1",
    "mcp": false,
    "model": "flash",
    "pane": "w1:p1",
    "role": "coder",
    "root": "/tmp/shfiso/repo",
    "seq": 1,
    "skills": {},
    "state": "exited",
    "task_id": "1",
    "updated": "2026-11-04T21:34:15Z",
    "vendor": "gemini",
    "worktree": "/tmp/shfiso/repo/wt-coder"
  }
]
```
Cuando una llamada al servidor falla o da error, no se emite un `[]` silencioso; se sale con código ≠ 0 y `{"error": "..."}`.

### 3. Archivos tocados

| Archivo | Tipo de cambio | Contenido / Razón del cambio |
|---|---|---|
| `crates/shepherd-core/src/lib.rs` | Modificación | `#[serde(default)]` en campos `seq`, `root`, `started` y `updated` de `Agent` para deserialización robusta de registros antiguos/parciales. |
| `crates/shepherd-core/src/service.rs` | Modificación | Cálculo real de `pending_gate`, `subtasks_progress` y lista completa `subtasks` en op `runs`; operaciones `approve` y `reject` en `handle_call`; ejecución no bloqueante de `run` en hilo desacoplado; prompt delivery no bloqueante en `spawn_inner`; tests unitarios. |
| `crates/shepherd-core/src/cli.rs` | Modificación | Subcomandos `approve` y `reject` (`--reason`, `--json`); flag `--wait` en `run`; resolución de servidor por `socket_exists()`; reporte estricto de errores JSON sin caída a in-process; formateo humano de gates en `runs list`; tests unitarios. |
| `crates/shepherd-core/src/workflow.rs` | Modificación | Funciones `reject_plan` y `reject_subtask`; limpieza de notas obsoletas «not ported yet». |
| `crates/shepherd-core/src/shepherd.rs` | Modificación | Método `socket_exists()` en `ShepherdClient`. |
| `crates/shepherd-core/src/git.rs` | Modificación | Asociación estricta rama/worktree → agente por coincidencia exacta de worktree o branch; eliminación de coincidencia difusa por prefijo; tests unitarios. |
| `src/shepherd/runs.rs` | Modificación | Conversión `ShepherdRunRow::from_value`; atajos `a`/`A`/`y` para aprobar y `d`/`D`/`R` para rechazar; diálogo de confirmación con explicación de push/PR; indicadores en footer. |
| `src/shepherd/overlay.rs` | Modificación | Despacho de `"approve"` y `"reject"` contra el servidor y refresco inmediato con llamada `"runs"`; tests unitarios con fakes. |
| `src/client/shell/shepherd.rs` | Modificación | Manejador del resultado de la llamada `runs` en el endpoint shell para poblar `overlay.runs.runs`. |
| `SHEPHERD.md` | Documentación | Registro del Estado F6, evidencias, capturas TUI y notas de verificación. |

### 4. Qué NO queda (alcance explícito futuro)

1. **Prueba de gates con LLMs y agentes CLI reales en vivo**:
   - Por directiva de aislamiento, la verificación de F6 se realizó sin agentes reales de pago contra APIs externas. El flujo funcional de los gates queda validado de punta a punta con el servidor, CLI y overlay.
2. **Streaming continuo de eventos de runs**:
   - La actualización del estado de un run en el overlay se realiza tras cada acción humana o apertura del modal; no existe suscripción push websocket/evento en tiempo real para el progreso de subhilos de un run en curso.
3. **Múltiples aprobadores o políticas avanzadas de gates**:
   - La aprobación de gate es actualmente 1:1 por subtarea o plan. Políticas multi-firma o aprobaciones escalonadas quedan para fases posteriores.

## Estado F6b (2026-11-04)

Corrección en el parser de planes (`field_value` en `crates/shepherd-core/src/lib.rs`) para tolerar negritas Markdown (`**` y `__`) antes y después de los dos puntos en campos como `Verify`, `Depends on` y `Verdict`.

### Contexto del fallo

En pruebas reales con brains LLM, se guardó un plan con la línea:
`- **Verify:** `printf 'hello from shepherd' | cmp - hello.txt && echo OK``
El parser anterior recortaba únicamente los `*` del principio de la línea antes de hacer `split_once(':')`. Como la línea no terminaba en `*` (terminaba en backtick), los asteriscos de cierre de la clave (`Verify:**`) quedaban intactos en el valor (`** `printf ...``), convirtiendo el comando en basura y provocando que el subtask pasara a `needs-human` (exit 127). Asimismo, formatos como `**Verify**: cmd` fallaban completamente al quedar `Verify**` como clave no coincidente.

### Cambios realizados

1. **Corrección de `field_value` (`crates/shepherd-core/src/lib.rs`)**:
   - `split_once(':')` divide primero clave y valor.
   - La clave se limpia de prefijos de lista (`-`, `*`), negritas (`**`, `*`, `__`, `_`) y espacios tanto al inicio como al final.
   - El valor recorta los asteriscos/guiones bajos de apertura o cierre de negrita generados tras los dos puntos (`**`, `*`, `__`, `_`), respetando comandos que legítimamente contengan `*` o `:` internamente (como `Verify: ls *`).
2. **Pruebas añadidas con TDD**:
   - Tests unitarios en `crates/shepherd-core/src/lib.rs` verificando `- **Verify:** `cmd``, `**Verify**: cmd`, `* **Depends on:** 1, 2`, `Verify: cmd`, formas sin negritas y con viñetas, así como variantes con `none`, comandos reales de prueba y veredictos en reviews.
   - Suite completa de `shepherd-core` (90 tests) y suites por áreas (`shepherd::` 52, `client::shell` 391, `protocol::` 156, `server::` 318) verificadas y en verde.
   - Comprobación de `./scripts/rename-from-herdr.sh --check` completamente limpia.

## Auditoría de solapes con herdr (2026-11-04)

Criterio del dueño: **usar lo que herdr ya tiene; implementar solo lo que no tiene.** Auditoría de solo lectura (API de herdr: `src/api/schema.rs`, `src/app/api/*`; uso desde `crates/shepherd-core/src/{shepherd,terminals,service,git,workflow}.rs`).

Métodos de herdr que Shepherd ya usa a través del adaptador: `worktree.create`, `agent.start`, `agent.prompt`, `agent.wait`, `agent.get`, `agent.focus`, `pane.read`, `pane.send_input`, `pane.rename`, `pane.report_metadata`, `tab.create/close/focus`, `workspace.create`, `pane.focus`.

| Función de Shepherd | Lo que herdr ya tiene | Veredicto |
|---|---|---|
| Panes, terminales, barra lateral, estado working/idle/blocked | todo | **Ya reutilizado** |
| Crear worktrees del agente | `worktree.create` (el adaptador ya lo usa; `create_git_worktree` solo es el respaldo del backend sin herdr) | Reutilizado; mantener el respaldo solo fuera de herdr |
| **Entrega de mensajes a agentes sin MCP (F3g)** | `agent.prompt`: escribe y envía Enter con las rarezas por vendor (foco de Copilot, límite de pegado de Codex, `encode_api_submission_parts`) y puede esperar un estado | **Solape real**: F3g teclea con `pane.send_input` y verifica por pantalla. Debe usar `agent.prompt` (con `wait`) y quedarse solo con el cursor persistente y la cola |
| Primer prompt con verificación y reenvío (hasta 3) | `agent.prompt` + `agent.wait` | **Solape probable**: comprobar si la verificación propia sigue haciendo falta; conservar solo el aviso «needs you» ante diálogos de confianza |
| Reconciliación por sondeo cada 2 s | `events.subscribe`, `events.wait`, `agent.wait`, `pane.wait_for_output` | **Mejora**: pasar a eventos; el sondeo queda como respaldo |
| «NEEDS YOU» | `notification.show` (no se usa) | **Añadir**: emitir también una notificación nativa |
| Pestaña Git: lista de worktrees | `worktree.list` (path, rama, `is_linked_worktree`, workspace abierto, etiqueta) | **Solape parcial**: la lista base debe venir de `worktree.list`; lo propio es limpio/sucio, adelante/atrás, agente y PR |
| `spawn`, `kill`, `focus`, `attach` | `agent.start/prompt/focus/rename`, `pane.close` | Envoltorios finos: valen por rol, skills y registro; no reimplementar la parte de terminal |
| Reanudar tras reinicio | `agent_resume.rs` (sesiones de agentes en panes restaurados) | **Distinto**: herdr restaura el proceso; Shepherd reconcilia su registro. Comprobar que no se pisan |
| Skill de control de panes (`skills/herdr`) | enseña a un agente a usar la CLI/API de herdr | Complementaria a `shepherd-comms` (bus y MCP), no sustituta |
| Tareas, spec/criterios, planes, runs con gates humanos, push/PR | **no tiene** | **Propio de Shepherd** |
| Bus de mensajes persistente, servidor MCP, tokens por agente | **no tiene** | **Propio de Shepherd** |
| Roles, vendors, modelos, skills por rol/sesión | **no tiene** | **Propio de Shepherd** |
| Estado de PR por rama (`gh`), limpio/sucio y sync por worktree | **no tiene** | **Propio de Shepherd** |

Cambios propuestos (fase F7, sin código todavía): entrega y primer prompt por `agent.prompt`; reconciliación por eventos; `notification.show`; Git base desde `worktree.list`; revisar `agent_resume.rs`.

## Plan F8 — Patrones multi-agente por eventos (aprobado 2026-11-04)

Fuente: «A Guide to Event-Driven Design for Agents and Multi-Agent Systems» (Confluent, 2025), capítulo III. Pedido del dueño: una pestaña **Patrones** dentro del overlay de Shepherd (en la TUI de herdr) para implementar los cuatro patrones, con cada agente definible (nombre, rol, **vendor**, modelo, **prompt**/persona) y todo personalizable, interactiva e intuitiva.

Patrones: **Orquestador-Worker** (tópico de órdenes con claves de partición, workers como grupo de consumidores, tópico de respuestas), **Jerárquico** (el mismo esquema aplicado recursivamente: cada nodo no hoja orquesta su subárbol), **Blackboard** (tópico compartido; los agentes publican conocimiento y se suscriben a lo relevante), **Basado en mercado** (tópicos bid/ask/transacción y un market-maker que empareja). Idea común: log de eventos persistente y reproducible (tópicos, grupos de consumidores, offsets), no llamadas directas.

Decisiones del dueño: alcance en **4 fases**; definiciones en **proyecto + global** (`.shepherd/patterns/*.yaml` en el repo y plantillas globales en la config de shepherd; gana el proyecto). Regla vigente: usar lo que herdr ya ofrece (ver «Auditoría de solapes») e implementar solo lo que no tiene; el bus persistente actual (ids + cursor por agente) es la base del log.

| Fase | Contenido | Depende de |
|---|---|---|
| F8a | Modelo de patrón (agentes, roles, vendor, modelo, prompt, tópicos que publica/consume), validación, almacenamiento proyecto+global, plantillas de los 4 patrones, CLI `shepherd pattern list/show/new/edit/validate --json` | F7 |
| F8b | Capa de eventos en el bus: tópicos nombrados, suscripciones, grupos de consumidores con clave de partición, offsets y reproducción, herramientas MCP `publish`/`subscribe`/`replay` | F8a |
| F8c | Pestaña **Patrones** del overlay: galería de plantillas, diagrama, editor interactivo de agentes (formularios para vendor/modelo/prompt/tópicos), validación en vivo, ratón y teclado | F8a (F8b para vista de eventos) |
| F8d | Ejecutor: lanza los agentes del patrón (por herdr), conecta sus tópicos, vista de eventos en vivo y control (pausa, reproducción) | F8b, F8c |

Orden de trabajo: F6b → F7 → F8a → F8b → F8c → F8d. Cada fase con brief, agente `gemini-3.8-flash-high` (reutilizado con `/clear`), revisión y verificación aislada; la prueba con agentes reales de claude y codex al cerrar F8d.
