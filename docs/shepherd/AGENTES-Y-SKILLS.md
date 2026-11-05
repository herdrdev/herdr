# Agentes, skills, bus de eventos y MCP

Cómo trabaja un agente dentro de Shepherd, qué herramientas tiene y cómo extenderlo.

## 1. Ciclo de vida de un agente

`shepherd spawn <rol> <tarea>` (o `s` en la pestaña Agentes, o un patrón) hace esto:
1. Resuelve el **rol** (vendor, modelo, fallback) con la configuración (`CONFIGURACION.md`).
2. Comprueba los límites (`max_concurrent`, `max_depth`).
3. Si el rol tiene `worktree: true`, crea un `git worktree` y la rama `shep/<n>-<tarea>` en `~/.shepherd/worktrees/<repo>/`.
4. Instala las skills del rol y lanza el vendor en un panel de la terminal, con el MCP de Shepherd inyectado y un token propio.
5. Le envía el primer prompt (la tarea, o la persona del patrón).

Estados: `starting` → `working` ↔ `idle`; `blocked` (necesita a una persona: diálogo de confianza, login, pregunta), `done` (entregó resultado), `killed`/`exited`. En la barra lateral se ven con su insignia y los avisos `[NEEDS YOU]`.

- **Terminal:** `shepherd attach <agente>` (o `Enter` en Agentes) abre su panel; ahí contestas permisos y preguntas.
- **Reanudar y limpiar:** un agente terminado se limpia con `x` en la interfaz; los snapshots se conservan según `max_per_agent`.
- **Agentes sin MCP:** los mensajes del bus se escriben en su terminal como `[shep] from <rol>#<n>: …` cuando el panel está en reposo.
- Los agentes **heredan tu configuración global del vendor** (por ejemplo `~/.claude/CLAUDE.md`). Si exige aprobar planes antes de editar, el agente se quedará esperando; incluye la aprobación en la tarea.

## 2. Roles y vendors incluidos

| Rol | Vendor por defecto | Para qué | Worktree | Puede crear agentes |
|---|---|---|---|---|
| `brain` | claude/opus | Planificar, repartir, agregar | no | sí |
| `coder` | codex (fallback gemini, claude) | Implementar | sí | no |
| `reviewer` | gemini (distinto del coder) | Revisar código y seguridad | no | no |
| `docs` | gemini | Documentación y consultas de docs | no | no |
| `tester` | claude/sonnet | Pruebas y depuración | no | no |

Cambiarlos o añadir roles: `config.yaml` (ver `CONFIGURACION.md`, sección 5). En cada `spawn` puedes forzar `--vendor` y `--model` sin tocar la configuración.

## 3. Skills

Una skill es un conjunto de instrucciones (`SKILL.md`) que se instala en el agente según su rol.

**Incluidas** (por defecto, protegidas contra borrado): `code-review`, `conventional-commits`, `debug`, `docs-lookup`, `implement-task`, `lang-go`, `lang-python`, `lang-ts`, `plan-tasks`, `pr-workflow`, `security-review`, `shepherd-comms`, `shepherd-events`, `write-tests`. `shepherd skills list` las muestra con su nivel.

**Dónde se definen las propias** (carpeta con un `SKILL.md`):

| Nivel | Ruta |
|---|---|
| Proyecto | `<repo>/.shepherd/skills/<nombre>/SKILL.md` |
| Usuario | `~/.config/shepherd/skills/<nombre>/SKILL.md` |
| Globales (a todos los agentes) | carpeta `_global/` dentro de cualquiera de las anteriores, o `skills.global` en `config.yaml` |

Formato:

```markdown
---
name: api-conventions        # debe coincidir con el nombre de la carpeta
description: Use when … (una línea: cuándo debe usarla el agente)
---

# Contenido
Reglas e instrucciones en Markdown.
```

- Se asignan a un rol con `roles.<rol>.skills: [nombre, …]`; `skills.disabled` excluye alguna.
- Una skill sin frontmatter, con nombre distinto del de su carpeta o que un rol referencia sin existir aparece como problema en `shepherd config validate`.
- En la interfaz (pestaña Skills): `c` catálogo/por agente, `Espacio` activar o desactivar, `n` nueva, `g` global, `d` eliminar, `r` restaurar.
- Cada vendor las recibe a su manera (`native` plugin de claude, `instructions` de codex, `prompt` de gemini).

## 4. Herramientas MCP de un agente

Cada agente con MCP las ve como herramientas del servidor `shepherd` (HTTP local, token por agente).

| Herramienta | Qué hace |
|---|---|
| `spawn_agent` | Crea un subagente (solo roles con `can_spawn: true`) |
| `send_message` / `read_messages` | Mensaje directo a otro agente por id o rol; leer y vaciar los pendientes |
| `wait_for` | Espera a que los agentes indicados entreguen resultado o salgan |
| `submit_result` / `get_result` | Entregar el resultado final / consultar el de otro sin bloquear |
| `list_agents` | Agentes de la sesión con rol, vendor, estado y worktree |
| `get_diff` | Diff completo del worktree de un coder contra la rama base |
| `read_output` | Últimas N líneas del panel de un agente |
| `ask_docs` | Pregunta al agente `docs` (lo crea si hace falta) y espera |
| `get_task_context` / `update_task_context` | Leer la tarea (issue, spec, criterios, plan) o, solo brain y docs, actualizarla. En runs de patrón devuelve objetivo y topología |
| `get_workspace_instructions` | Instrucciones del repo, skills efectivas y checks detectados |
| `publish`, `subscribe`, `poll`, `ack`, `replay`, `list_topics` | Bus de eventos (sección siguiente) |

## 5. Bus de eventos

Mensajería por **tópicos** con particiones y grupos de consumo, persistida en el estado de Shepherd.

- **Tópico:** nombre + número de particiones. Los de un patrón se crean al ejecutarlo; los demás con `shepherd topic create <nombre> --partitions N`. Publicar en uno inexistente lo crea con 1 partición.
- **Particionado:** `--key` (hash estable → misma partición) o, sin clave, reparto rotatorio. El orden solo se garantiza dentro de una partición.
- **Grupos de consumo:** `subscribe` sin grupo conserva el que el run ya asignó (o, si no hay, un grupo propio con el id del agente: cada agente recibe todo). Los miembros de un mismo grupo se reparten las particiones y cada mensaje lo recibe **uno solo**.
- **Offsets:** `poll` entrega los mensajes sin confirmar; `ack` confirma hasta un offset. `replay` relee el histórico sin mover offsets.
- **Despertar:** publicar escribe un aviso («Hay N mensaje(s) nuevo(s) en '<tópico>': usa poll») en los suscriptores en reposo (excepto el publicador).

Desde la línea de comandos: `topic list|create|publish|tail|replay|groups` (ver `USO-CLI.md`); en la interfaz, la pestaña Eventos.

Consejo: para repartir trabajo entre N workers usa un tópico con al menos N particiones y publica sin clave.

## 6. Flujo de tareas con `shepherd run`

Para una tarea de `.shepherd/tasks`:

1. **Plan:** el `brain` descompone la tarea en subtareas (`task plan <id> --save` lo guarda). Con `workflow.confirm: true` espera tu `shepherd approve <id> plan`.
2. **Implementación:** un `coder` por subtarea, cada uno en su worktree.
3. **Revisión:** hasta `max_review_rounds` rondas con un `reviewer` de otro vendor.
4. **Comprobaciones:** los `workflow.checks` (o los detectados por lenguaje).
5. **Publicar:** push y PR (borrador) con `gh`, **solo tras tu aprobación** (`approve <id> <n>`) o con `--auto-push`. No se publica desde ramas protegidas.

Rechazar: `shepherd reject <id> plan|<n> --reason "…"`. Estado: `shepherd runs list`, pestaña Runs.

## 7. Snapshots de un agente

Fotos del worktree que Shepherd toma solo (agente en reposo, al terminar, cada `interval_secs` trabajando) o con `snapshot capture`. Antes de acciones destructivas (restaurar, matar) se guarda una foto `pre-restore`/`pre-kill`. Comandos y límites: `USO-CLI.md` y `CONFIGURACION.md`.
