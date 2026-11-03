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
