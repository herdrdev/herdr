# Patrones multi-agente: referencia

Un **patrón** es un equipo de agentes descrito en un YAML: quién participa, con qué rol, vendor y modelo, por qué tópicos se hablan y quién arranca. Un **run** es una ejecución de ese patrón con un objetivo.

Ubicación (gana el primero que exista con ese nombre):
1. `<repo>/.shepherd/patterns/<nombre>.yaml` (proyecto)
2. `~/.config/shepherd/patterns/<nombre>.yaml` (global, `pattern new --global`)
3. Plantillas incluidas: `orchestrator-worker`, `hierarchical`, `blackboard`, `market`.

```bash
shepherd pattern list                              # ver también cuál tapa a cuál (SHADOWED)
shepherd pattern new revision --from blackboard    # copia una plantilla o patrón existente
shepherd pattern edit revision                     # abre el YAML en tu editor
shepherd pattern validate revision                 # o --file ruta.yaml
shepherd pattern run revision --task "Objetivo del run"
```

## 1. Estructura del YAML

```yaml
name: webteam                     # obligatorio; letras, números, guion o guion bajo
kind: orchestrator-worker         # orchestrator-worker | hierarchical | blackboard | market
description: Texto libre
topics:
- name: orders                    # obligatorio y único; sin caracteres raros
  partitions: 4                   # 1 a 256 (por defecto 1)
  key: task_id                    # pista informativa sobre la clave de partición
  filter: hypothesis              # opcional: prefijo esperado en los mensajes
agents:
- id: orchestrator                # obligatorio y único
  role: brain                     # rol de la configuración (aporta permisos y skills)
  vendor: claude                  # opcional: sustituye al vendor del rol
  model: haiku                    # opcional: sustituye al modelo
  prompt: Instrucciones          # persona; hasta 16 384 caracteres
  count: 1                        # instancias (1 a 32; máximo 64 agentes en total)
  level: 0                        # solo hierarchical: piso de la jerarquía
  publishes: [orders]             # tópicos donde escribe
  consumes: [results]             # tópicos que lee
  group: worker-pool              # grupo de consumo compartido por las instancias
  start: true                     # este agente recibe el objetivo y arranca el run
```

Notas:
- `prompt` **sustituye** al guion de trabajo del rol: el rol solo aporta vendor/modelo, permisos y skills. El agente recibe además su topología, sus tópicos y el objetivo.
- Todo `publishes`/`consumes` debe referirse a un tópico declarado.
- Si ningún agente tiene `start: true`, se elige el iniciador por defecto de la topología (orquestador, director, researcher o requesters).
- Los agentes que no arrancan esperan: se despiertan solos cuando llega un mensaje a un tópico que consumen.
- Con `count > 1` las instancias comparten el grupo `group`: cada partición se reparte entre ellas y **cada mensaje lo recibe un solo miembro**.

## 2. Las cuatro topologías y sus reglas

### orchestrator-worker
Un orquestador reparte tareas y agrega resultados; un grupo de workers las ejecuta.
- Exactamente **1 orquestador** (`count: 1`, publica órdenes) y **al menos 1 worker** en un grupo de consumo.
- Flujo: orquestador → `orders` → workers → `results` → orquestador.
- Para que las órdenes se repartan entre workers, el tópico de órdenes necesita varias particiones y el orquestador debe publicar **sin clave** (o con claves distintas): con la misma clave todo cae en una partición y un solo worker lo recibe.

### hierarchical
Director → managers → especialistas, por niveles.
- Cada agente necesita `level`; los niveles no pueden tener huecos ni ciclos de tópicos.
- Plantilla: `director` (nivel 0) publica en `root-orders`; `manager` ×2 (nivel 1) lee `root-orders`, publica en `team-orders` y `root-results`; `specialist` ×4 (nivel 2) lee `team-orders` y publica en `team-results`.

### blackboard
Pizarra compartida: varios agentes leen y escriben en el mismo tópico.
- Al menos **2 instancias** de agente y al menos un tópico de pizarra usado (publicado o leído) por **todos**.
- Plantilla: `researcher` (hipótesis), `analyst` (validación), `synthesizer` (síntesis); el tópico `blackboard` filtra por prefijo (`hypothesis:`, `validation:`, `synthesis:`).

### market
Compradores piden, vendedores pujan, un árbitro decide.
- Requiere los tópicos `ask`, `bid` y `transaction`.
- Exactamente **1 market-maker** que lee `ask` y `bid` y publica `transaction`, y **al menos 2 solvers** que publican en `bid`.
- Flujo: `requester` publica `ask` → `solver`s publican `bid` → `market-maker` publica `transaction` → ganador trabaja e informa.

## 3. Validación

`pattern validate` clasifica cada problema por gravedad y ruta de campo:

```
$ shepherd pattern validate t-market        # solvers en 1
pattern has validation problems:
  [warning] agents[0].vendor: vendor 'claude' is configured but its command 'claude' is not installed or not in PATH
  [error] agents: market pattern requires at least 2 solver agent instances publishing to 'bid', found 1
```

- `error` impide ejecutar. `warning` avisa (por ejemplo, vendor no instalado).
- Comprobaciones generales: al menos un agente, ids y tópicos únicos y sin caracteres inválidos, `count` y `partitions` en rango, rol existente, vendor y modelo definidos en la configuración, tópicos referidos que existan.
- La interfaz (pestaña Patrones) muestra los mismos errores con su ruta de campo en el editor.

## 4. Ejecutar y gobernar un run

```bash
shepherd pattern run webteam --task "…" [--worktree /ruta] [--confirm-over-limit]
shepherd pattern runs                      # ID PATTERN KIND STATE AGENTS LIVE TOPICS CREATED
shepherd pattern pause|resume <id>
shepherd pattern stop <id> --confirm
```

- Cada agente se crea con su rol, vendor, modelo, prompt, skills y tópicos; los de rol con `worktree: true` reciben rama y worktree propios (`shep/<n>-run-<patrón>-…`), o todos comparten `--worktree`.
- Si el run supera `max_concurrent` agentes activos, hace falta `--confirm-over-limit`.
- Estados: `running`, `paused`, `done`, `degraded` (algún agente terminó o murió sin completarse el run), `failed` (todos fallaron o murieron), `stalled` (agentes en reposo o bloqueados sin que el run terminara bien: hay un agente bloqueado o no se publicó nada) y `stopped`.
- `done` ocurre cuando todos los agentes viven en reposo y nada se publica en los tópicos del run durante `run_quiescence_secs`. Un mensaje nuevo reabre el run. Un agente parado esperando respuesta humana cuenta como en reposo: contesta a sus preguntas o dale un objetivo que no requiera aprobación.
- El run no hace merge ni push: cada agente deja commits en su rama y tú integras con git.

## 5. Ejemplo: orchestrator-worker con dos workers

```yaml
name: webteam
kind: orchestrator-worker
topics:
- {name: orders, partitions: 4, key: task_id}
- {name: results, partitions: 1}
agents:
- id: orchestrator
  role: brain
  vendor: claude
  model: haiku
  prompt: Divide el objetivo en tareas independientes, publícalas en orders (sin clave) y agrega los resultados.
  publishes: [orders]
  consumes: [results]
  start: true
- id: worker
  role: coder
  vendor: claude
  model: haiku
  prompt: Cuando llegue una orden, hazla en tu worktree, commitea y publica el resultado en results.
  count: 2
  group: worker-pool
  publishes: [results]
  consumes: [orders]
```

```
$ shepherd pattern run webteam --task "Dos tareas: A) crea a.txt con A; B) crea b.txt con B"
started pattern run run-webteam-9228 for pattern webteam (running) with 3 agents
$ shepherd topic groups orders
TOPIC   GROUP        MEMBERS  COMMITTED  TOTAL  LAG
orders  worker-pool  2        -          2      p0:1, p1:1
```

Resultado real: `coder-1` hizo la orden A y `coder-2` la B, cada uno con su commit en su worktree.
