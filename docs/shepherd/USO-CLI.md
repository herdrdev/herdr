# Usar Shepherd por comandos (CLI)

Todo lo que hace la interfaz se puede hacer con el comando `shepherd`. Los comandos se ejecutan **desde la raíz del repositorio** del proyecto: ahí viven `.shepherd/tasks`, `.shepherd/patterns` y los worktrees de los agentes.

- Casi todos aceptan `--json` (salida estructurada para scripts).
- Hablan con el servidor de la sesión de Shepherd; si no hay servidor, algunos comandos admiten `--no-server`.
- Compilar y probar: `./cargo-fork.sh build` genera `target/debug/shepherd`.

## 1. Mapa de comandos

| Grupo | Comandos |
|---|---|
| Tareas | `task new`, `task list [--archived]`, `task show <id> [sección]`, `task plan <id> [--save]`, `task edit`, `task path`, `task archive` |
| Agentes | `spawn <rol> <tarea…> [--task-id] [--vendor] [--model]`, `agents`, `roles list`, `vendors list`, `attach [agente] [--takeover]` |
| Runs con gates | `run <id-tarea> [--auto-push] [--wait]`, `approve <id> plan\|<n>`, `reject <id> plan\|<n> [--reason]`, `runs list` |
| Patrones | `pattern list`, `show`, `new <nombre> --from <plantilla>`, `validate`, `edit`, `delete`, `run`, `runs`, `pause`, `resume`, `stop` |
| Bus de eventos | `topic list`, `create`, `publish`, `tail`, `replay`, `groups` |
| Snapshots | `snapshot capture`, `list`, `show`, `diff`, `restore`, `delete`, `prune` |
| Otros | `skills list`, `git status`, `config check|path|validate`, `mcp serve` |

Ayuda de cualquiera: `shepherd <comando> --help`.

## 2. Tareas

```
$ shepherd task new 'Añadir login' --description 'Formulario de login' --criteria 'tests pasan'
created task 1-anadir-login
/tmp/shfiso/repo/.shepherd/tasks/1-anadir-login

$ shepherd task list
ID	STATE	TITLE	ISSUE	PR
1	new	Añadir login	-	-

$ shepherd task show 1
# Task 1-anadir-login
# Añadir login
Formulario de login
# Spec
...
# Acceptance criteria
- [ ] tests pasan
```

`--criteria` se puede repetir. `task plan 1 --save` pide un plan al agente `brain` y lo guarda; `task edit 1` abre la tarea en tu editor.

## 3. Agentes y worktrees

```bash
shepherd roles list      # brain claude/opus · coder codex/default · docs gemini/default · reviewer gemini/default · tester claude/sonnet
shepherd vendors list    # claude · codex · gemini(agy), con su disponibilidad

shepherd spawn coder "Implementa GET /health en main.go y añade test. Commitea." \
    --vendor claude --model haiku
# started coder-1 (claude/haiku) in Shepherd pane w2:p2
# worktree ~/.shepherd/worktrees/repo/shep-1-implementa-… (shep/1-implementa-…)

shepherd agents
# ID       STATE    ROLE   VENDOR  MODEL  TASK
# coder-1  working  coder  claude  haiku  Implementa GET /health …
```

- Un rol con `worktree = true` recibe su propio `git worktree` y rama `shep/<n>-…`. Un rol sin worktree trabaja en la raíz del repo.
- `--task-id` asocia el agente a una tarea de `.shepherd/tasks`.
- Estados: `starting`, `working`, `idle`, `blocked`, `done`, `killed`.
- La primera vez, el agente puede pedir permisos de confianza en su terminal; los contestas tú (`shepherd attach coder-1`).
- Integrar el trabajo es cosa tuya, con git normal: `git merge shep/1-implementa-…`.

## 4. Snapshots (fotos de seguridad del worktree)

```bash
shepherd snapshot list --agent coder-1     # coder-1/1  2026-11-05 21:39:30  idle  4 archivos  +40 -0
shepherd snapshot show coder-1/1
shepherd snapshot diff coder-1/1           # contra el estado actual
shepherd snapshot restore coder-1/1 --confirm            # antes guarda un 'pre-restore'
shepherd snapshot restore coder-1/1 --path main.go --confirm
shepherd snapshot prune --agent coder-1 --keep 10
```

Se crean solas (agente en reposo, al terminar, periódicas) y manualmente con `snapshot capture <agente>`. Viven en `refs/shepherd/snapshots/…`; no tocan tu rama ni tu índice.

## 5. Patrones multi-agente

```
$ shepherd pattern list
NAME                 KIND                 SOURCE   AGENTS  TOPICS
blackboard           blackboard           builtin  3       1
hierarchical         hierarchical         builtin  7       4
market               market               builtin  6       3
orchestrator-worker  orchestrator-worker  builtin  4       2

$ shepherd pattern new revision --from blackboard
created pattern 'revision' at .shepherd/patterns/revision.yaml
$ shepherd pattern validate revision
pattern is valid
$ shepherd pattern show revision
Agents (3): researcher (brain), analyst (tester), synthesizer (docs)
Topics (1): blackboard (1 partitions)
```

Edita `.shepherd/patterns/<nombre>.yaml` (`pattern edit`) para cambiar vendor, modelo, `count`, tópicos y quién arranca (`start: true`). Ejecutar y vigilar:

```bash
shepherd pattern run webteam --task "Añade GET /version y un middleware de logging"
# started pattern run run-webteam-189105 for pattern webteam (running) with 3 agents
shepherd pattern runs      # ID PATTERN KIND STATE AGENTS LIVE TOPICS CREATED
shepherd pattern pause|resume <id>
shepherd pattern stop <id> --confirm
```

Estados del run: `running`, `paused`, `done` (todos en reposo 30 s y sin mensajes nuevos; se reabre si llega uno), `degraded`, `stalled`, `stopped`. Si el patrón supera `max_concurrent`, `pattern run` pide `--confirm-over-limit`.

## 6. Bus de eventos (tópicos)

```
$ shepherd topic create avisos --partitions 2
created topic avisos (2 partitions)
$ shepherd topic publish avisos 'hola equipo' --from yo
published message - to avisos[0] (offset 0)
$ shepherd topic publish avisos 'segundo aviso' --from yo
published message - to avisos[1] (offset 0)
$ shepherd topic list
NAME    PARTITIONS  MESSAGES
avisos  2           2
$ shepherd topic tail avisos
[2026-11-05T23:22:12Z] avisos[0]:#0 from yo: hola equipo
[2026-11-05T23:22:12Z] avisos[1]:#0 from yo: segundo aviso
$ shepherd topic replay avisos --partition 0 --from 0
[2026-11-05T23:22:12Z] avisos[0]:#0 from yo: hola equipo
$ shepherd topic groups avisos
no consumer groups
```

- Sin `--key` los mensajes se reparten entre particiones; con la misma clave caen en la misma partición.
- Los miembros de un grupo de consumo se reparten las particiones: con 2 workers y 4 particiones, cada orden la recibe un solo worker. Ver `topic groups orders` (columnas GROUP, MEMBERS, LAG).
- Publicar un mensaje despierta a los agentes suscritos que estén en reposo.

## 7. Otros

```
$ shepherd skills list      # skills disponibles: lang-go, code-review, shepherd-events…
$ shepherd git status
BRANCH  STATUS  SYNC        AGENT  PR  PATH
main    clean   up to date  -      —   /ruta/repo
$ shepherd config check          # valida config.toml (límites, snapshots, atajos)
config: ok
$ shepherd config path           # rutas de la configuración de roles/vendors y el estado
global	~/.config/shepherd/config.yaml
project	<repo>/.shepherd/config.yaml
state	~/.local/state/shepherd
$ shepherd config validate       # valida esa configuración de roles/vendors (--json disponible)
configuration is valid
```

Hay dos configuraciones: `config.toml` (sección `[shepherd]`: límites, snapshots) la valida `config check`; `config.yaml` (roles, vendors, flujo) la muestran `config path` y `config validate`.

## 8. Ejemplo completo: servidor web en Go con 2 workers

Probado con agentes reales (claude haiku). Repositorio con `go.mod` y un `main.go` mínimo, commit inicial en `main`.

```bash
cd ~/webgo

# 1) Un patrón de equipo: copia, ajusta y valida
shepherd pattern new webteam --from orchestrator-worker
#   edita .shepherd/patterns/webteam.yaml: vendor: claude, model: haiku, worker count: 2
shepherd pattern validate webteam

# 2) Lánzalo con el objetivo
shepherd pattern run webteam --task \
  "Un worker añade GET /version (version.go + test); el otro un middleware de logging (logging.go + test). Cada uno en su worktree, sin tocar archivos del otro, y commitea."

# 3) Vigila
shepherd pattern runs
shepherd agents
shepherd topic groups orders        # un grupo worker-pool con 2 miembros, lag p0:1, p1:1
shepherd topic tail results         # informes de los workers

# 4) Cuando el run esté en done, integra y prueba con git normal
git worktree list
git merge --no-edit shep/6-run-webteam-189105
git merge --no-edit shep/7-run-webteam-189105
go vet ./... && go test ./...

# 5) Limpia
git worktree remove ~/.shepherd/worktrees/repo/shep-6-run-webteam-189105
git branch -d shep/6-run-webteam-189105 shep/7-run-webteam-189105
```

Resultado real: `/version` y el logging quedaron en ramas distintas, cada una con su commit y snapshots por agente; las tres fusionadas y `go test` en verde.

## 9. Problemas conocidos

- Los permisos de confianza de cada agente nuevo los contesta una persona.
- Los agentes heredan tu `~/.claude/CLAUDE.md` global: si exige aprobar planes, un worker puede quedarse esperando. Dale el plan aprobado en el objetivo del run.
- Un agente parado esperando respuesta humana cuenta como «en reposo» y el run puede marcarse `done` antes de tiempo (se reabre al seguir trabajando).
- `shepherd` nunca hace push, PR ni merge por su cuenta.
