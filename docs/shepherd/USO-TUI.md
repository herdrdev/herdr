# Usar Shepherd desde la interfaz (TUI)

Shepherd vive dentro de la terminal de herdr como una **pestaña virtual «Shepherd»**: la primera de la barra de pestañas. Reemplaza el área central y deja visible la barra lateral. Todo se puede hacer con teclado o con ratón (cada botón del panel es clicable); no hay ventanas emergentes.

## 1. Abrir y cerrar

| Acción | Cómo |
|---|---|
| Abrir | `Ctrl+b` y luego `a`, o clic en la pestaña **Shepherd** |
| Cerrar | `Esc` (cierra primero un formulario o confirmación si hay uno abierto) o botón **× Cerrar** |
| Cambiar de sección | `1`…`7`, `Alt+←` / `Alt+→` o clic en la barra de secciones |

Tamaño mínimo: si la terminal es pequeña, Shepherd pide ampliarla; por debajo de 70 columnas muestra solo la lista.

Los agentes se ven en la terminal normal de herdr: en **Agentes**, `Enter` abre la terminal del agente seleccionado.

## 2. Las siete secciones

Barra superior: `Agentes (1) · Tareas (2) · Runs (3) · Patrones (4) · Skills (5) · Git (6) · Eventos (7)`. Cada sección tiene una lista a la izquierda, el detalle a la derecha, botones de acción abajo y una barra de estado con las teclas.

Teclas comunes: `↑/↓` o `j/k` mueven la selección, `PgUp/PgDn` desplazan el detalle, `Tab` pasa de campo en los formularios, `Enter` confirma, `Esc` cancela.

### Agentes (1)
Lista de agentes (rol, vendor, modelo, estado). Barra: `↑/↓: lista | Enter: terminal | s: nuevo | t: snapshots | K: matar | x: limpiar | r: refrescar`

- `s` nuevo agente: formulario **Rol**, **Tarea** (id o descripción) y **Vendor / Modelo** (opcional), como `claude/haiku`.
- `Enter` abre la terminal del agente. `K` mata al agente (pide confirmación). `x` limpia la sesión de un agente terminado; `X` limpia todos los definitivos.
- `t` abre la línea de tiempo de snapshots del agente: comparar y restaurar con confirmación.

### Tareas (2)
Barra: `↑/↓: lista | n: nueva | i: importar | a: archivar | e: editar`, filtro Todas/Abiertas/Completadas (`f`), botón **Iniciar Run (r)**.
- `n` abre el formulario: **Título**, **Descripción / Especificación** y **Criterios de aceptación** separados por `;`.
- `i` importa un issue de GitHub (necesita `gh`). `e` abre la tarea en tu editor.

### Runs (3)
Ejecuciones con gates de aprobación. Barra: `l: iniciar run | c: cancelar | a: aprobar gate | d: rechazar`. Cuando un run espera aprobación del plan o de una subtarea, `a` aprueba y `d` rechaza.

### Patrones (4)
Galería de patrones y editor. Filtros: Todos / Proyecto / Global / Plantillas. Barra: `n: nuevo | e: editar | c: duplicar | v: validar | d: eliminar`, botón **Ejecutar (r)**.
- El detalle muestra descripción, un **diagrama ASCII de la topología**, los agentes (rol, tópicos, persona) y los tópicos.
- `n`: elige la plantilla base con `Espacio` y escribe el nombre. `e` abre el editor con validación (error con ruta de campo). `r` ejecuta el patrón con un objetivo.

### Skills (5)
Instrucciones que se instalan a los agentes. `c`: alternar catálogo / por agente · `Espacio`: activar o desactivar · `r`: restaurar · `n`: nueva · `g`: global · `d`: eliminar.

### Git (6)
Ramas y worktrees, con su agente y PR. Barra: `w: nuevo worktree | b: nueva rama | p: publicar | t: snapshots | x: eliminar | c: copiar ruta`. Publicar y eliminar piden confirmación.

### Eventos (7)
Dos vistas, `Tab` alterna: **1. Tópicos y flujo** y **2. Ejecuciones de patrones**.
- Tópicos: mensajes en vivo con OFFSET, PART, DE, CLAVE, TIPO, CONTENIDO. `Space` pausa/reanuda, `r` replay desde un offset, `/` filtra.
- Ejecuciones: `p` pausa/reanuda el run, `K` lo detiene, y el detalle indica «✓ COMPLETADA» cuando está en reposo.

## 3. Pantallas reales

Sección **Tareas** con una tarea creada:

```
 Shepherd  [ Agentes (1) ] [ Tareas (2) ] [ Runs (3) ] [ Patrones (4) ] [ Skills (5) ] [ Git (6) ] [ Eventos (7) ]
           [ + Nueva (n) ]│TAREA #1: Añadir login [NEW]                                   [ Editar ]
 [✓ Todas] [Abiertas] [Completadas]│Estado: new  |  Rama: ninguna  |  PR: #0
 ▸ [NEW] #01 Añadir login │ESPECIFICACIÓN:
                          │Formulario de login
                          │CRITERIOS DE ACEPTACIÓN:
                          │[ ] tests pasan
                          │[ Iniciar Run (r) ]  [ Archivar Tarea (a) ]  [ Importar GitHub (i) ]
 1 tareas (0 abiertas) | ↑/↓: lista | n: nueva | i: importar | a: archivar | e: editar | Esc: salir
```

Sección **Patrones** (extracto del detalle de `blackboard`):

```
┌────────────────────────────────────────────────┐
│      PIZARRA COMPARTIDA: blackboard (p:1)      │
└────────────────────────────────────────────────┘
    ▲               ▲               ▲
    │ pub/sub       │ pub/sub       │ pub/sub
    ▼               ▼               ▼
[researcher:brain] [analyst:tester] [synthesizer:docs]
```

Sección **Eventos**:

```
 [ 1. Tópicos y Flujo ]  [ 2. Ejecuciones de Patrones ]
 [ ▶ En vivo (p) ]  Offset: [0] [ ⟲ Replay (r) ]  Filtro: [ / ]
▸ 2p avisos     OFFSET PART DE   CLAVE  TIPO  CONTENIDO
                0      0    yo          msg   hola equipo
                0      1    yo          msg   segundo aviso
 Tópicos y Streaming (2 msgs) | Tab: alternar vista | Space: pausa/reanudar | r: replay | /: filtrar | Esc: salir
```

## 4. Ejemplo de uso paso a paso: servidor web en Go

Escenario: tienes un repo `webgo` con un servidor mínimo y quieres añadir `/health`, `/version` y logging con un equipo de agentes, cada uno en su worktree. Abre Shepherd en la carpeta del repo.

1. **Abrir Shepherd:** `Ctrl+b a`.
2. **Crear la tarea** (sección 2): `n`, título `Ampliar webgo`, descripción con lo que quieres, criterios `go test pasa; GET /health devuelve 200`. `Enter`.
3. **Un agente suelto** (sección 1): `s`, rol `coder`, tarea `Implementa GET /health y su test`, vendor/modelo `claude/haiku`, `Enter`. El agente aparece como `starting` → `working`. Si pide permisos de confianza, `Enter` sobre él abre su terminal para contestarlos.
4. **Ver su worktree** (sección 6): aparece `shep/<n>-implementa-get-health` ligada a ese agente. Cuando el agente esté en `idle`, tendrá un commit y un snapshot (`t` en Agentes o Git).
5. **Lanzar un patrón de equipo** (sección 4): selecciona `orchestrator-worker`, `c` para duplicarlo con el nombre `webteam`, `e` para poner vendor `claude`, modelo `haiku` y 2 workers, `v` para validar. Con `webteam` seleccionado pulsa `r` (**Ejecutar**) y escribe el objetivo (campo «Objetivo o tarea»): «Un worker añade GET /version, el otro un middleware de logging; cada uno en su worktree y commit».
6. **Seguir el run** (sección 7, vista 2, o sección 3): los tres agentes trabajan; en la vista de tópicos ves la orden del orquestador en `orders` y los resultados en `results`. `p` pausa el run si quieres revisar; `K` lo detiene.
7. **Revisar y restaurar:** en Agentes, `t` sobre un worker muestra su línea de tiempo; si algo salió mal, restaura un snapshot (pide confirmación y guarda antes un `pre-restore`).
8. **Integrar:** cuando el run diga «✓ COMPLETADA», cierra Shepherd (`Esc`) y fusiona las ramas con git desde una terminal normal (`git merge shep/…`). Shepherd nunca fusiona ni hace push por sí solo. Para limpiar, `x` en Git elimina worktrees (con confirmación).

Con agentes reales este mismo flujo se verificó por CLI; los atajos de arriba son los que muestra cada barra de estado.

## 5. Consejos

- Si un formulario está abierto, los números `1`…`7` escriben en el campo en vez de cambiar de sección: cierra con `Esc` primero.
- Todo botón con una letra entre paréntesis tiene ese atajo cuando la sección está activa.
- La interfaz y el CLI usan los mismos datos: lo que creas en una se ve en la otra (`shepherd agents`, `shepherd pattern runs`…). Ver `USO-CLI.md`.
