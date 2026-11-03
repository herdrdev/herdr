# Encargo para el agente: Shepherd (fork de herdr), fase F3c — estado de Shepherd en la barra lateral de agentes

Mismo worktree que F1–F3b. **Siguen vigentes las reglas de los encargos F1–F3b** (cambios mínimos en archivos de herdr y listados en `SHEPHERD.md`; solo `./cargo-fork.sh`; **no commitear**; nada de push ni contra el repo original; no leer ni ejecutar nada contra el estado real del usuario ni sesiones/sockets reales; no inventar APIs; no contestar diálogos de confianza/permisos; si un comando no está permitido, para y dilo).

## Problema (hallazgo del revisor, ver "Corrección del revisor (F3b)" en `SHEPHERD.md`)
En `src/client/shell/agent_sidebar.rs` el marcador de Shepherd lee tokens `shepherd_state` / `shepherd_notice` / `needs_you` que **nadie rellena**, así que `[working]`, `[idle]`… no se ven. Además `[NEEDS YOU]` se dispara con cualquier agente `Blocked` de herdr, sea de Shepherd o no.

## Alcance (en orden; tests en verde tras cada punto)
1. **Origen de los datos**: el servicio de Shepherd ya conoce, por agente, su `pane_id`, estado (starting/working/idle/blocked/done/exited/killed/failed) y `notice`. Haz que esa información llegue a la barra lateral por el mecanismo que herdr ya usa para datos de agente (investiga cómo se construye `AgentRow` y de dónde vienen los datos del endpoint; reutilízalo, no abras un canal nuevo). Sin I/O en render. Si hace falta un campo nuevo en el esquema de API, que sea opcional, retrocompatible y esté en `docs/next/api/herdr-api.schema.json` y `SHEPHERD.md`.
2. **Marcador solo para agentes de Shepherd**: elimina la heurística por tokens. `[estado]` solo aparece si el pane pertenece a un agente de Shepherd. `[NEEDS YOU]` solo cuando el estado de Shepherd es `blocked` (no por el estado de herdr), y el aviso completo se lee (tooltip, línea extra o menú contextual, siguiendo el patrón existente). Un agente de herdr normal se dibuja **exactamente como antes** (test de caracterización).
3. **Tests** de render puro: agente de Shepherd en cada estado, bloqueado con aviso, agente ajeno bloqueado sin marcador, `shepherd.enabled` apagado sin cambios, ancho pequeño sin pánico.
4. **Estado F3c** en `SHEPHERD.md`: hecho, no verificado, archivos de herdr tocados, y qué NO queda conectado (sé explícito).

## Criterio de terminado
`./cargo-fork.sh build`, `./cargo-fork.sh test -p shepherd-core` y los tests de herdr de las áreas tocadas en verde (el test intermitente de plugins conocido pasa en solitario: anótalo, no lo arregles). Mensaje final claro.
