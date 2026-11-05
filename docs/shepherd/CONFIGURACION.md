# Configuración de Shepherd

Shepherd se configura con **dos tipos de archivo** que se combinan:

| Archivo | Formato | Qué contiene | Dónde |
|---|---|---|---|
| `config.toml` | TOML | La aplicación (atajos, tema, etc.) y la sección `[shepherd]`: activar, límites, snapshots | `~/.config/shepherd/config.toml` (compilación de depuración: `~/.config/shepherd-dev/`) y `<repo>/.shepherd/config.toml` |
| `config.yaml` | YAML | Roles, vendors, skills, flujo de trabajo, límites (forma antigua) | `~/.config/shepherd/config.yaml` (global) y `<repo>/.shepherd/config.yaml` (proyecto) |

Si no existe `config.toml` en la ruta de Shepherd, se lee en solo lectura `~/.config/herdr/config.toml` (herencia del proyecto base).

Comprobar: `shepherd config check` valida el `config.toml`; `shepherd config path` muestra las rutas del `config.yaml` y el directorio de estado; `shepherd config validate` valida el `config.yaml` (con `--json` ambos dan salida estructurada).

## 1. Orden de prioridad

1. Valores por defecto internos.
2. `config.yaml` global, y encima el del proyecto (los mapas se fusionan en profundidad; un valor `null` restablece la clave).
3. `config.toml` global y encima `<repo>/.shepherd/config.toml`: **gana sobre el YAML** en los límites y snapshots.

Por eso `limits` dentro del YAML está **obsoleto**: Shepherd avisa («'limits' … is deprecated; configure '[shepherd.limits]' in config.toml instead»). El YAML del proyecto se busca subiendo desde el directorio actual hasta encontrar `.shepherd/config.yaml` (o hasta la raíz del repo git).

Variables de entorno útiles:

| Variable | Efecto |
|---|---|
| `SHEPHERD_CONFIG_PATH` | Ruta explícita del `config.toml` |
| `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_DATA_HOME` | Cambian dónde se leen la configuración, el estado y los datos |
| `SHEPHERD_TERMINALS` | Fuerza el backend de terminales (prioridad sobre `shepherd.backend`) |
| `SHEPHERD_SESSION` | Nombre de la sesión (`--session` en la línea de comandos) |

El estado (registro de agentes, bus, snapshots) vive en `~/.local/state/shepherd` y los worktrees de los agentes en `~/.shepherd/worktrees/<repo>/`.

## 2. Activar Shepherd y atajo

```toml
# ~/.config/shepherd/config.toml
[shepherd]
enabled = true           # inicia el servicio de orquestación con el servidor (por defecto false)

[keys]
shepherd = "prefix+a"    # abre la sección Shepherd (valor por defecto)
```

Sin `enabled = true`, Shepherd se comporta como un multiplexor de terminal normal.

## 3. `[shepherd.limits]`

```toml
[shepherd.limits]
max_depth = 2              # profundidad máxima de agentes que crean agentes (por defecto 2)
max_concurrent = 4         # agentes activos a la vez (por defecto 4)
notice_after_secs = 3      # segundos antes de mostrar el aviso «iniciando» (por defecto 3)
run_quiescence_secs = 30   # reposo necesario para dar un run por terminado (30; 0 lo desactiva)
```

- `max_concurrent` cuenta solo agentes **activos** (no los que están en reposo ni terminados). Superarlo exige confirmación (`--confirm-over-limit` en `pattern run`).
- `run_quiescence_secs`: un run pasa a `done` cuando todos sus agentes están en reposo y nada se publica en sus tópicos durante ese tiempo; se reabre si llega un mensaje.

Otros límites solo existen en el YAML (obsoleto pero válido): `agent_timeout` (por defecto `30m`), `protected_branches` (`main`, `master`, `develop`: no se permite publicar desde ellas), `non_mcp_done_quiet_secs` (60: segundos sin cambios para dar por terminado a un agente sin MCP).

## 4. `[shepherd.snapshots]`

```toml
[shepherd.snapshots]
max_per_agent = 50                 # fotos conservadas por agente (se podan las más antiguas)
max_file_bytes = 2097152           # archivos mayores de 2 MiB no se capturan
max_capture_bytes = 67108864       # tope por captura (64 MiB)
interval_secs = 300                # foto periódica tras 300 s seguidos trabajando (0 la desactiva)
```

Las fotos se guardan en `refs/shepherd/snapshots/<agente>/<n>` dentro del repo, sin tocar tu rama ni tu índice.

## 5. `config.yaml`: roles, vendors y flujo

Ejemplo mínimo de proyecto (`<repo>/.shepherd/config.yaml`); solo hay que escribir lo que cambia, el resto viene de los valores por defecto:

```yaml
roles:
  coder:
    vendor: claude          # en vez de codex
    model: sonnet
    worktree: true
workflow:
  base_branch: main
  checks: ["go vet ./...", "go test ./..."]
  draft: true
skills:
  global: [lang-go]         # skills instaladas en todos los agentes
```

### 5.1 Roles (`roles.<nombre>`)

| Campo | Significado | Valor por defecto de los roles incluidos |
|---|---|---|
| `vendor` | Vendor que ejecuta el rol | brain→claude, coder→codex, reviewer→gemini, docs→gemini, tester→claude |
| `model` | Modelo (vacío = el del vendor) | brain `opus`, tester `sonnet` |
| `fallback` | Vendors alternativos si el principal no está disponible | coder: `gemini`, `claude` |
| `must_differ_from` | El vendor debe ser distinto del de otro rol (revisión cruzada) | reviewer: `coder` |
| `worktree` | Trabaja en su propio worktree y rama | solo coder |
| `can_spawn` | Puede crear subagentes (herramienta `spawn_agent`) | solo brain |
| `skills` | Skills específicas del rol | ver `AGENTES-Y-SKILLS.md` |
| `prompt` | Instrucciones propias del rol | — |

Roles incluidos: `brain`, `coder`, `reviewer`, `docs`, `tester`. Puedes añadir roles nuevos con cualquier nombre.

### 5.2 Vendors (`vendors.<nombre>`)

Incluidos: `claude` (comando `claude`), `codex` (`codex`) y `gemini` (comando `agy`).

| Campo | Significado |
|---|---|
| `command`, `args` | Ejecutable y argumentos de arranque |
| `models`, `default_model`, `model_flag` | Modelos disponibles, el predeterminado y la bandera para elegirlo (`--model`, `-m`) |
| `print_args` | Argumentos del modo no interactivo (`-p`, `exec`) |
| `mcp.supported`, `mcp.style` | Si admite MCP y cómo se le inyecta (`claude-config`, `codex-overrides`) |
| `skills.mode`, `skills.style`, `skills.file` | Cómo recibe las skills (`native`, `instructions`, `prompt`) |
| `shepherd_agent` | Nombre con el que lo reconoce el detector de agentes (`claude`, `codex`, `agy`) |
| `disabled` | Excluye el vendor (los roles caen a su `fallback`) |

`shepherd vendors list` indica cuáles están disponibles en tu máquina. Para desactivar uno: `vendors: { gemini: { disabled: true } }`.

### 5.3 Flujo de trabajo (`workflow`)

Controla `shepherd run` (plan → subtareas → revisión → publicar).

| Campo | Significado | Por defecto |
|---|---|---|
| `max_review_rounds` | Rondas de revisión por subtarea (0 = sin revisión) | 3 |
| `confirm` | Exige aprobar el plan antes de programar (`shepherd approve <tarea> plan`) | false |
| `base_branch` | Rama base de los worktrees y diffs | la actual |
| `checks` | Comandos que deben pasar antes de publicar | ninguno (se detectan por lenguaje) |
| `pr_template` | Plantilla del cuerpo del PR | — |
| `draft` | Crear el PR como borrador | true |

Publicar (push y PR) siempre requiere aprobación humana; `shepherd run --auto-push` la da por adelantado. Necesita `gh` instalado.

### 5.4 Otras secciones

- `skills.global`: nombres de skills instaladas en todos los agentes. `skills.disabled`: skills excluidas.
- `shepherd.backend`: `socket` (por defecto, usa la API del servidor) o `hc`. `shepherd.session`: nombre de la sesión (`shepherd`). `shepherd.autostart`: arranca la sesión sola (true).
- `general.log_level` (`info`) y `general.theme` (`auto`).

## 6. Errores de validación frecuentes

`shepherd config validate` devuelve problemas como:
- `role "x" refers to unknown vendor "y"`: el rol usa un vendor que no existe en `vendors`.
- `role "x" refers to unknown fallback vendor "y"`.
- `role "x" must_differ_from unknown role "y"` (y los ciclos entre roles se rechazan al resolver).
- `limits.max_concurrent_agents must be at least 1`.
- `shepherd.backend must be "socket" or "hc"`.

Un `config.toml` mal formado hace que la aplicación use los valores por defecto y lo diga; `shepherd config check` da el detalle.
