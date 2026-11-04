# Encargo para el agente: Shepherd en Rust, etapa R1

Lee primero `docs/rust-architecture.md` y `docs/rust-migration.md`. Trabajas en el worktree actual (rama derivada de `rust-core`). Implementas **solo la etapa R1**, en `crates/shepherd-core`, con TDD.

## Reglas (obligatorias)

- Solo editas `crates/`, `Cargo.toml`, `Cargo.lock`, `docs/` y `Makefile`. **No toques el código Go.** Go es la referencia de comportamiento: léelo, no lo cambies.
- Nunca hagas `git push`, ni crees PRs, ni cambies ramas fuera de tu worktree. Commits pequeños con rutas explícitas (`git add <rutas>`; nunca `git add -A`), cada uno con `cargo test --workspace` en verde.
- Nunca ejecutes contra el sistema real: `hc kill|rm|keep|server|update|machine`, `shepherd-app do`, ni nada que cree, mate o cambie terminales, agentes o la app del usuario. Los comandos de **solo lectura** (`hc --help`, `hc <cmd> --help`, `hc ls --json`) puedes usarlos para averiguar formatos; no copies en el repo nombres de proyectos ni rutas de terceros que veas en su salida. Los tests del adaptador `hc` usan un **`hc` falso** (un script en un directorio temporal puesto delante en `PATH`).
- No inventes la API de shepherd/hc: verifícala (`hc --help`, `hc <cmd> --help`, `~/.config/shepherd-app/shepherd.d.ts`, `internal/shepherd/schema` y `docs/vendors.md`). Lo que no puedas verificar, márcalo en `docs/rust-architecture.md` como **no verificado**.
- Dependencias: lo mínimo; evita añadir crates que no necesites. Si añades una, justifícala en el mensaje de commit.
- Nunca contestes diálogos de confianza ni de login por el humano; nunca expongas secretos ni tokens en logs o en `--json` (el token de un agente no se imprime).
- No marques nada como hecho sin haber ejecutado `cargo test --workspace` y leído su salida.

## Alcance R1 (en este orden; un commit por punto, tests incluidos)

1. **`trait Terminals`** (módulo `terminals`): abrir un agente en una terminal nueva dentro de un worktree (con env), prompt, enviar texto y teclas, leer la pantalla, esperar, estado del agente, etiquetar, cerrar, adjuntarse. Refactoriza `shepherd.rs` + `spawn.rs` + `workflow.rs` para depender del trait, no del cliente concreto.
   - `ShepherdSocket`: lo existente, detrás del trait, sin cambiar su comportamiento.
   - `HcCli`: nuevo; ejecuta `hc` (`run --bg`, `send`, `keys`, `read`, `wait`, `ls --json`, `label`, `kill`, `attach`). Descubre los formatos reales con `--help` y deja documentado el mapeo trait→comando en `docs/rust-architecture.md`.
   - Falso en memoria para los tests del núcleo, y un `hc` falso (script) para los del adaptador.
   - Selección del adaptador por configuración (`shepherd.backend: socket|hc`, por defecto `socket`) y por la variable `SHEPHERD_TERMINALS`.
2. **`--json`** en `agents`, `task list`, `task show`, `roles list`. Esquema estable y documentado en `docs/rust-json-schema.md`; nunca incluye tokens. Tests que fijan el esquema.
3. **Herramientas MCP que faltan** (hoy hay `submit_result`, `get_task_context`, `read_messages`, `send_message`, `get_diff`, `update_task_context`): `spawn_agent`, `wait_for`, `get_result`, `list_agents`, `read_output`, `ask_docs`, `get_workspace_instructions`. Comportamiento y permisos como en `internal/daemon/mcp.go` (por ejemplo, solo brain y docs actualizan el contexto de la tarea; límites de profundidad y de agentes concurrentes).
4. **Skills** (módulo `skills`), portando `internal/skills` (`catalog.go`, `effective.go`, `install.go`, `manage.go`) y sus tests: catálogo con prioridad repo > usuario > embebidas, niveles global/rol/sesión (`SkillAdjust`), `effective_for`, detección de copia editada, validación de `SKILL.md`, e instalación por sesión para los estilos `claude-plugin`, `codex-developer`, `worktree-file` y prompt. Las skills embebidas están en `skills/` del repo Go; incluye una forma de leerlas sin duplicarlas a mano (por ejemplo `include_dir`-like con `include_str!` generado, o copiarlas a `crates/shepherd-core/skills` con un test que compruebe que no divergen).

## Criterio de terminado

- `cargo test --workspace` en verde, con tests nuevos por cada punto.
- `cargo clippy --workspace` sin errores nuevos si clippy está instalado.
- Lista final en `docs/rust-architecture.md` (sección "Estado R1") con lo hecho, lo no verificado y lo que queda. Si te bloqueas en un punto, deja los anteriores completos, documenta el bloqueo y para.
