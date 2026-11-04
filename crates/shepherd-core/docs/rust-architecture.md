# Shepherd en Rust: arquitectura recomendada

Estado a 2026-11-03. Complementa `docs/rust-migration.md` (orden de portabilidad y criterios para retirar Go).

## Qué cambia respecto a la idea del fork

Verificado en esta máquina:

- Hay **dos productos de shepherd** instalados, con APIs distintas:
  - `shepherd` 0.9.5: TUI y servidor con API de socket NDJSON (el que usa hoy el código Go y `shepherd-core`).
  - `shepherd-app` + `shepherd-core` (`hc`): la **versión gráfica** (GPUI). Sus terminales y agentes corren en `hc`; la app solo los muestra.
- `shepherd-app` **se extiende sin fork**: la interfaz entera son archivos en `~/.config/shepherd-app` (layouts, teclas) y *mods* en JavaScript (gpui-kit) que pueden ejecutar `hc` y programas externos (`shepherd.exec`, `shepherd.hc`, ver `shepherd.d.ts`) y lanzar comandos de la app (`tab.new {argv, task}`, `terminal.open`, ...).
- Por eso **no hace falta un fork** para integrar Shepherd en la app gráfica. Un fork solo se justifica si se necesita algo que ni `hc` ni los mods exponen. Hoy no se ha encontrado ninguna carencia.

## Forma recomendada: núcleo + puerto + dos frontends

```
            ┌──────────────── shepherd-core (lib) ────────────────┐
            │ tareas · planes · workflow · roles · skills · MCP   │
            │ bus · registro · reconciliación · límites · gates   │
            └───────────────┬─────────────────────────────────────┘
                            │ trait Terminals  (el único punto de contacto con shepherd)
             ┌──────────────┴───────────────┐
        ShepherdSocket (shepherd 0.9.5)       HcCli (shepherd-core `hc` + `shepherd-app do`)
                            │
      ┌─────────────────────┴──────────────────────┐
  Frontend 1: CLI + TUI de terminal           Frontend 2: mod de shepherd-app
  (`shepherd`, ratatui más adelante)          (JS; ejecuta `shepherd … --json`)
```

1. **`trait Terminals`**: abrir un agente en una terminal/pestaña (con worktree), mandarle un prompt o teclas, leer su pantalla, esperar a que pare, ver su estado, etiquetarlo, cerrarlo, adjuntarse. `ShepherdSocket` es lo que ya existe; `HcCli` ejecuta `hc` (`run --bg`, `send`, `keys`, `read`, `wait`, `ls --json`, `label`, `kill`). El núcleo nunca importa un adaptador concreto, así que los tests usan un falso.
2. **Salida `--json` estable** en todos los comandos de lectura (`agents`, `task list/show`, `roles list`, `runs`, `skills`). Es el contrato que consumen la TUI y el mod. Documentado en `docs/rust-json-schema.md`.
3. **Frontend 2 (mod de shepherd-app)**: un panel que lista agentes, tareas y runs con sus acciones (abrir la terminal del agente, matar, aprobar un gate), llamando al binario `shepherd` con `--json`. Sin lógica de negocio en JavaScript. Se valida con `shepherd-app check <carpeta>`.
4. **Frontend 1**: CLI ya existente y, después, una TUI en ratatui sobre el mismo contrato (árbol, sub-pestañas y formularios como en las M11 a M14 de Go).
5. **Daemon (`shepherd serve`)**: proceso largo con el registro, el bus, el servidor MCP y la reconciliación contra el adaptador activo. La CLI habla con él por socket unix, igual que `shep`/`shepd`.

## Etapas

| Etapa | Contenido | Quién |
|---|---|---|
| **R1** | `trait Terminals` + `ShepherdSocket` refactorizado + `HcCli` con falsos; `--json`; las 7 herramientas MCP que faltan; skills (catálogo, niveles global/rol/sesión, instalación por vendor) | agente `agy` (`docs/rust-agent-brief.md`) |
| R2 | `shepherd serve`: registro persistente con contadores que nunca retroceden, reconciliación, reanudación tras reinicio, verificación y reenvío del primer prompt, aviso "needs you", recuperación de nombres ocupados | agente |
| R3 | Mod de `shepherd-app` (panel de agentes/tareas/runs) y vista de git/ramas/PR | agente |
| R4 | TUI de terminal (ratatui) | agente |
| R5 | Importar issues, `doctor`, `init`, configuración editable, instalador; retirar Go según `rust-migration.md` | agente |

Cada etapa termina con `cargo test --workspace` en verde, sin tocar el comportamiento seguro (push y PR siempre esperan a un humano).

## Lo aprendido que el puerto debe respetar (viene de las pruebas reales con Go)

Están en `docs/vendors.md` y `docs/decisions.md`; los imprescindibles:

- Un prompt aceptado por shepherd puede perderse si la CLI aún arranca: verificar que el agente reacciona y reenviar hasta 3 veces.
- Una CLI parada en un diálogo de confianza o login nunca acepta su primer prompt: marcarlo como "necesita al humano" y **nunca contestar el diálogo por el humano**.
- Los nombres de agente son únicos entre paneles abiertos: un id que se reutiliza choca; los ids y el contador de spawns **no retroceden** aunque se limpien agentes.
- `done` significa "terminó su turno" y el panel sigue vivo; solo `exited`, `killed` y `failed` son definitivos.
- Editar el texto de una skill llega al agente sin reiniciar (claude y codex); añadir o quitar una skill exige reiniciarlo.
- Push y PR siempre esperan una aprobación humana explícita, salvo `--auto-push` de ese run.
- Nada de lo que escribe Shepherd entra en el repo del usuario (skills e instrucciones se inyectan por sesión).

## Preguntas abiertas para el dueño del proyecto

1. ¿Existe ya un fork de herdr con cambios propios? No hay ninguno en este equipo. Si existe, ¿qué capacidad aporta que `hc` no tenga?
2. ¿La app gráfica es el objetivo principal de adopción, o la TUI de terminal? Cambia el orden de R3 y R4.
3. ¿Se mantiene el protocolo del socket de `shepherd` 0.9.5, o se deja solo `hc`?

---

## Mapeo `trait Terminals` → `ShepherdSocket` y `HcCli`

El módulo `crates/shepherd-core/src/terminals.rs` define `trait Terminals` y selecciona el adaptador activo mediante la variable de entorno `SHEPHERD_TERMINALS` (`socket` | `hc`) o la clave de configuración `shepherd.backend` (`socket` por defecto):

| Método en `trait Terminals` | `ShepherdSocket` (`shepherd` 0.9.5 socket NDJSON) | `HcCli` (`shepherd-core` `hc` CLI) |
|---|---|---|
| `open_agent(&OpenAgentRequest)` | `workspace.list` / `workspace.create` o `worktree.create` → `tab.create` (con `env`) → `pane.rename` → `metadata.report` → `agent.start` (o `pane.send_text` si `shepherd_agent` está vacío) | Worktree git (`git worktree add -B <branch> <path> <base>`) si aplica → `hc run --bg --json --outlive-parent --no-notify --name <agent_id> --cwd <cwd> --label role=<role> --label agent=<agent_id> --label vendor=<vendor> --label label=<label> -- <command> <args...>` (con variables `env` en el proceso hijo) |
| `prompt(target, text)` | `agent.prompt { agent, text }` (`agent_blocked` → `PromptOutcome::Blocked`) | `hc send --json <target> <text>` (código de salida `9` o `error.kind == "blocked"` → `PromptOutcome::Blocked`) |
| `send_text(target, text)` | `pane.send_text { pane_id, text, enter: true }` | `hc send --json <target> <text>` |
| `send_keys(target, keys)` | `pane.send_keys { pane_id, keys }` | `hc keys --json <target> <keys...>` |
| `read_screen(target, lines)` | `pane.read { pane_id, lines }` | `hc read --json --lines <lines> <target>` (extrae `.text` de `ReadResult`) |
| `wait(target, timeout)` | `agent.wait { agent, timeout_ms }` | `hc wait --json --timeout <secs> <target>` |
| `agent_status(target)` | `agent.list {}` (filtra por `name` o `pane_id`) | `hc ls --json` (filtra `.terminals` por `name`, `address` o `labels.agent`) |
| `label(target, labels)` | `pane.rename` (si incluye `label`) + `metadata.report { pane_id, source: "shepherd", status, data }` | `hc label --json <target> <k=v>...` |
| `close(target)` | `tab.close { tab_id }` | `hc kill --json <target>` |
| `attach(target, takeover)` | `shepherd [--session <s>] agent attach <target> [--takeover]` o `shepherd session attach <s>` | `hc attach [target]` |

---

## Estado R1 (completado)

### 1. Módulos en `crates/shepherd-core` tras R1

| Archivo | Estado en R1 |
|---|---|
| `src/terminals.rs` | **Nuevo en R1**: `trait Terminals`, `ShepherdSocket`, `HcCli`, `InMemoryTerminals`, selección de backend por `SHEPHERD_TERMINALS` y `shepherd.backend` (`socket\|hc`, por defecto `socket`). |
| `src/shepherd.rs` | Ampliado con `send_keys`, `read_pane` y `wait_agent` sobre el cliente NDJSON de socket, envuelto por `ShepherdSocket`. |
| `src/spawn.rs` | Refactorizado para depender exclusivamente de `&dyn Terminals` (`spawn_with_terminals`, `spawn_with_options`, `spawn_child`), verificando `can_spawn`, `max_spawn_depth`, `max_concurrent_agents`, `use_worktree`, e instalando skills de sesión mediante `skills::install`. |
| `src/workflow.rs` | Refactorizado para ejecutar `run_coders_with_terminals`, `review_and_check`, `revise_coder` y `revise_agent` a través de `&dyn Terminals`. |
| `src/skills.rs` | **Nuevo en R1**: catálogo con prioridad `Repo > User > Default (embebido desde skills/)`, `_global/`, `effective` / `effective_for` con auto-activación `lang-*` y `SkillAdjust` (`Global`, `Role`, `Session`), detección `edited`, validación de frontmatter (`validate`), gestión (`name_ok`, `list_in`, `new_skill`, `override_skill`, `toggle` con reversión automática si otra capa lo sombrea), e instalación por sesión (`claude-plugin`, `codex-developer`, `worktree-file`, `prompt`). |
| `src/mcp.rs` | Ampliado de 6 a **13 herramientas MCP**: añadidas `spawn_agent`, `wait_for`, `get_result`, `list_agents`, `read_output`, `ask_docs` y `get_workspace_instructions` junto a `submit_result`, `get_task_context`, `read_messages`, `send_message`, `get_diff` y `update_task_context`, respetando los permisos de `internal/daemon/mcp.go`. |
| `src/lib.rs` y `src/main.rs` | Añadido soporte `--json` en `shepherd agents --json`, `shepherd task list [--archived] --json`, `shepherd task show <id> [section] --json` y `shepherd roles list --json`, con omisión garantizada de tokens (`Agent::public()`). Esquemas documentados en `docs/rust-json-schema.md`. |

### 2. Verificado vs. No verificado en vivo

- **Verificado con `cargo test --workspace` (52 tests en verde, 0 warnings)**:
  - Selección de adaptador (`SHEPHERD_TERMINALS` > `shepherd.backend` > `"socket"`), validación de configuración.
  - `InMemoryTerminals` en tests unitarios de `terminals`, `spawn`, `workflow` y `mcp`.
  - `HcCli` contra un script `hc` falso en directorio temporal que verifica los argumentos exactos y parseo JSON de `run`, `send` (incluyendo bloqueo con código 9), `keys`, `read`, `wait`, `ls`, `label`, `kill` y `attach`.
  - `ShepherdSocket` / `ShepherdClient` contra un `UnixListener` temporal verificando el protocolo NDJSON.
  - No divergencia entre las 13 skills embebidas en Rust (`include_str!`) y los archivos de `skills/` en el árbol del repositorio.
  - Prioridad `Repo > User > Default`, niveles `Global / Role / Session`, `edited`, `toggle` con rollback ante sombreo, e instalación de sesión en los 4 estilos (`claude-plugin`, `codex-developer`, `worktree-file`, `prompt`).
  - Esquemas `--json` y ausencia total del campo `token` en `agents --json` y `list_agents`.
  - Las 13 herramientas MCP, permisos `can_spawn`, límites `max_spawn_depth` y `max_concurrent_agents`, restricción de `update_task_context` a `brain`/`docs`, `read_output`, `ask_docs` y `get_workspace_instructions`.
- **No verificado contra procesos reales en vivo**:
  - Por las reglas de seguridad de la sesión, no se ejecutaron comandos destructivos ni spawns reales contra el demonio `hc` ni `shepherd-app` de la máquina anfitriona (`hc run`, `hc kill`, `shepherd-app do`).

### 3. Qué queda para R2+

- **R2 (`shepherd serve`)**: demonio persistente con bucle de reconciliación contra `Terminals`, verificación y reenvío hasta 3 veces del primer prompt si la CLI tarda en arrancar, detección de aviso `"needs you"` cuando un agente queda bloqueado, y recuperación ante nombres ocupados.
- **R3**: Mod de `shepherd-app` consumiendo `shepherd ... --json`.
- **R4**: TUI de terminal en `ratatui`.
- **R5**: Importación de issues de GitHub, `doctor`, `init`, instalador y retirada del árbol Go según `docs/rust-migration.md`.

