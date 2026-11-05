# F13: dos fallos hallados en la prueba real del servidor Go

Trabaja en tu worktree (`shep/24-…`; antes `git merge --ff-only shepherd`). Reglas de siempre: rustfmt solo en archivos propios de Shepherd, nunca `cargo fmt --all`, cambios en archivos de herdr mínimos, `scripts/rename-from-herdr.sh --check` limpio, tests por áreas, commitea con rutas explícitas (sin `git add -A`), sin push.

## Fallo 1: `shepherd spawn` (CLI) no envía la raíz del proyecto
`shepherd spawn coder "x" --vendor claude --model haiku` ejecutado desde la raíz de un repo falla con
`invalid_request: worktree path must be absolute`. Causa probable: `ShepherdAgentSpawn` no recibe `root` (a diferencia de snapshot, ver `fill_snapshot_root` en `src/cli/shepherd.rs`), el servidor usa `current_dir()` propio y acaba con una ruta relativa. Con `"root":"/ruta/repo"` en la API funciona.
- Arreglo: rellenar `root` con el cwd del CLI cuando falte, para spawn y para cualquier otro método con `root` opcional que tenga el mismo problema (revisa `run`, `task`, `pattern run`, `git status`: hazlo donde falte, de forma genérica y no a parches).
- Defensa en el servidor: en `service/spawn.rs`, si `root` resulta relativo, hacerlo absoluto/canónico o devolver un error claro («root debe ser absoluto»), nunca la excepción críptica del worktree.
- Tests: CLI (`--no-server` y por socket si hay infraestructura) y un test de servicio con `root` vacío/relativo.

## Fallo 2: los workers de un patrón no comparten grupo de consumo
En `webteam` (`group: worker-pool`, `count: 2`), `shepherd topic groups orders` mostró dos grupos (`coder-4`, `coder-5`) con lag propio; ambos workers recibieron las dos órdenes y coder-5 estuvo a punto de duplicar el trabajo. Esperado: un único grupo `worker-pool` con 2 miembros que se reparten las particiones.
- `service/patterns_run.rs` (~l.370) ya hace `bus.subscribe(&spawned.id, topic, Some(group))`, pero el agente luego llama a `subscribe` por MCP (herramientas del bus, skill `shepherd-events`) y parece crear su propio grupo por id. Investiga con un test que lo reproduzca (suscripción previa del run + `subscribe`/`poll` posterior del agente sin `group`) y arregla la causa: que el agente herede la suscripción/grupo del run (p. ej. `subscribe` sin `group` reutiliza el grupo ya asignado al agente en ese tópico; `poll` sin grupo usa el del run).
- Revisa también que el prompt del modo patrón y la skill `shepherd-events` indiquen no pasar `group` salvo necesidad, y que el reparto de particiones entre miembros del grupo funcione con 2 workers y 4 particiones (cada orden la recibe un solo worker).
- Tests: núcleo (bus + patterns_run) con 2 workers del mismo grupo: cada mensaje lo ve un solo miembro; `topic groups` muestra un grupo con 2 miembros.
- La quiescencia (F8h) y el despertar (publicar despierta a los miembros del grupo, no al publicador) deben seguir funcionando; añade test si el cambio los toca.

## Verificación y entrega
- `cargo build`; áreas: `-p shepherd-core`, `--bin shepherd shepherd:: client::shell protocol:: server:: api:: config::`; golden existentes intactos; greps de fuga (`testRepo`, `araquesebastian`) en 0.
- Añade a `SHEPHERD.md` una sección «Estado F13» (qué cambió, archivos, tests, límites). Commit en tu rama. Dime al terminar con un resumen corto.
