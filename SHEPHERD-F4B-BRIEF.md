# Encargo para el agente: Shepherd (fork de herdr), fase F4b — pestaña «Git» del overlay (ramas, worktrees, PRs; solo lectura)

Mismo worktree que F1–F4a. **Siguen vigentes las reglas de F1–F4a** (cambios mínimos en archivos de herdr y listados en `SHEPHERD.md`; solo `./cargo-fork.sh` y `./scripts/shepherd-iso.sh`; **no commitear**; nada de push ni contra el repo original; no leer ni ejecutar nada contra el estado real del usuario; los tests NO deben tocar `~/.local/state`, `~/.config` ni imprimir estado real: usa tempdirs y XDG aislados; no contestar diálogos de confianza/permisos; **no lances agentes reales**; si un comando no está permitido, para y dilo). Lee el estado de F3, F3b y F4a en `SHEPHERD.md` y el patrón de las pestañas existentes (`src/shepherd/{tasks,runs,skills}.rs`, `overlay.rs`, `src/client/shell/shepherd.rs`).

## Objetivo
Una cuarta pestaña **Git** en el overlay de Shepherd (junto a Tareas/Runs/Skills) que muestre, **solo lectura**, el estado git del proyecto y de los agentes:
- **Ramas y worktrees**: lista de worktrees del repo (`git worktree list --porcelain`), rama de cada uno, si está limpio o sucio, adelante/atrás respecto a su base, y a qué agente de Shepherd pertenece (por ruta de worktree o rama `shep/…` cruzando con el registro).
- **PRs**: si `gh` está instalado y autenticado, estado de PR por rama (`gh pr list --json …`, solo lectura, con timeout y sin red obligatoria); si no, la columna muestra «—» y una nota «gh no disponible». Nunca crear/mergear/cerrar nada.
- **Operaciones**: ninguna que escriba en esta fase (nada de push, merge, delete, checkout). Solo refrescar (`r`) y copiar/mostrar la ruta del worktree.

## Alcance (en orden; tests en verde tras cada punto)
1. **Modelo puro** `src/shepherd/git.rs`: `ShepherdGitState` (filas, selección, nota de error) y render puro en el estilo de `runs.rs`; parseo de `git worktree list --porcelain`, `git status --porcelain=v1 -b` y `gh pr list --json` en funciones puras testeadas con fixtures de texto.
2. **Recolección fuera del render**: una op nueva `git_status` en `handle_call` (`crates/shepherd-core/src/service.rs`) que ejecuta los comandos git/gh con timeout corto (reutiliza el ayudante de procesos del crate; no bloquees el tick ni el render) y devuelve JSON; accesible por `shepherd.call` y por `herdr shepherd git --json` (subcomando nuevo de la CLI de F4a, con salida JSON uniforme).
3. **Pestaña en el overlay**: `ShepherdTab::Git` (ajusta `ALL`, índices, ancho de tabs, ayudas de teclado, ratón si las otras pestañas lo soportan), carga al abrir la pestaña y con `r`, sin sondeo continuo.
4. **Tests**: parseos con fixtures, render a buffer con filas, pestaña en la navegación Tab/BackTab, la op `git_status` sobre un repo temporal real creado en el test (con `git init` en tempdir y un worktree) y gh ausente (sin red).
5. **Verificación real con `./scripts/shepherd-iso.sh`** sin agentes reales: en el repo de pruebas aislado crea un worktree con rama `shep/…`, abre el overlay, cambia a la pestaña Git y comprueba con `screen` que lista el worktree, su rama y su estado limpio/sucio; ensucia el worktree y refresca con `r`. Pega capturas en `SHEPHERD.md`. Termina con `stop`.
6. **Estado F4b** en `SHEPHERD.md`: hecho, qué verificaste, archivos de herdr tocados, qué NO queda (operaciones de escritura sobre ramas/PR, F5).

## Criterio de terminado
`./cargo-fork.sh build`, `./cargo-fork.sh test -p shepherd-core` y los tests de herdr por áreas (`client::shell`, `shepherd::`, `protocol::`, `api::`, `server::`) en verde (la suite entera muere por SIGPIPE y plugins/config son intermitentes: anótalo, no lo arregles). Comprueba que `./cargo-fork.sh test -p shepherd-core 2>&1 | grep -c testRepo` da 0. Mensaje final claro.
