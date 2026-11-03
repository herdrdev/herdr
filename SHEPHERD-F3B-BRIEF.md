# Encargo para el agente: Shepherd (fork de herdr), fase F3b — cablear las vistas en el TUI

Mismo worktree que F1–F3 (ya commiteadas hasta `cbdfac06`). **Siguen vigentes todas las reglas de `SHEPHERD-F1-BRIEF.md`, `SHEPHERD-F2-BRIEF.md` y `SHEPHERD-F3-BRIEF.md`**: cambios mínimos en archivos de herdr y todos listados en `SHEPHERD.md` ("Archivos de herdr modificados"); solo `./cargo-fork.sh`; **no commitear**; nunca push ni nada contra el repo original; no leer ni ejecutar nada contra el estado real del usuario (`~/.local/state/shepherd`, `~/.config/herdr*`, sesiones o sockets reales); solo sesiones aisladas con `XDG_*` temporales; no inventar APIs; no contestar diálogos de confianza ni permisos; si un comando no está permitido, para y dilo. Lee `AGENTS.md` de herdr (estado puro, render puro, sin god objects, mouse-first, rutas de rendimiento).

## Problema

Las vistas de `src/shepherd/{panel,tasks,runs,skills}.rs` están hechas y probadas **aisladas**, pero nada las conecta al TUI: ninguna tecla ni clic llega a ellas, no se componen en pantalla, `AppState` solo guarda el panel de agentes, y nadie despacha las acciones que emiten. Con el binario actual el usuario no puede usarlas. Tu trabajo es dejarlas **usables de verdad**.

## Principio

La terminal nativa de herdr de cada agente es la forma de verlo y hablarle (acción "focus" = ir a su pane). Nunca dibujes ni emules salida de agente. Las vistas solo leen instantáneas de estado puro; **cero I/O en la ruta de render**; las acciones pasan por `shepherd.call` / API de herdr.

## Alcance (en orden; pruebas en verde tras cada punto; si te quedas sin tiempo deja completos los anteriores)

0. **Diseño primero (corto)**: antes de tocar código, investiga cómo se cablea hoy un overlay existente de herdr (p. ej. `settings_overlay`, `global_menu`, `overlays`, `overlay_input`, `composition`) y qué corre en servidor vs. cliente. Escribe en `SHEPHERD.md` una sección "Diseño F3b" de ≤25 líneas: dónde vive el estado, por dónde entran teclas/clics, dónde se compone, cómo vuelven los datos del servicio, y la lista exacta de archivos de herdr que vas a tocar. Luego impleméntalo siguiendo el patrón del overlay existente, no uno nuevo.
1. **Entrada**: un atajo global (configurable, con un valor por defecto que no choque con los de herdr; compruébalo en `src/input/keybindings.rs`) y una entrada en el menú global/`global_menu` abren un overlay "Shepherd" con pestañas **Agentes / Tareas / Runs / Skills** (clic o teclas para cambiar). `Esc` cierra. Solo existe si `shepherd.enabled` está activo; con él apagado, comportamiento idéntico al de herdr.
2. **Estado y datos**: `AppState` guarda el estado de las cuatro vistas (no solo agentes). Se rellenan con instantáneas que el servicio publica (evento/tick del servidor ya existente; reutiliza `shepherd.call` ops de lectura; si falta una op de lectura, añádela en el servicio con tests). Sin llamadas bloqueantes en render ni en el hilo de entrada.
3. **Composición y render**: el overlay se pinta con los `render` puros existentes dentro del patrón de composición de herdr; respeta tamaños pequeños (sin pánico con áreas diminutas; test).
4. **Despacho de acciones**: cada acción que emiten las vistas se ejecuta: `FocusAgentPane` → enfoca el pane nativo del agente (cierra el overlay); lanzar/matar/limpiar agentes, crear/importar/archivar tareas, lanzar/cancelar/aprobar/rechazar runs, skills → `shepherd.call` al servicio; `OpenEditorInNewPane` → abre `$EDITOR` en un pane nuevo de herdr. Los resultados y errores vuelven a la vista como mensaje visible. **Las guardas de confirmación se mantienen: ninguna acción destructiva o que publica (push/PR, matar, limpiar, archivar, cancelar) se despacha sin su confirmación** — test por cada una a través del camino completo de entrada (tecla → estado → acción despachada), usando un despachador falso.
5. **Ratón**: clic en pestañas, filas y botones llega a las vistas (hit-testing coherente con lo renderizado; test).
6. **Estado F3b** en `SHEPHERD.md`: hecho, no verificado, archivos de herdr tocados (lista exacta), pendiente. **Sé explícito sobre qué NO está conectado** (si algo queda sin cablear, dilo ahí y en el mensaje final; el informe de F3 omitió justo eso).

## Criterio de terminado

`./cargo-fork.sh build`, `./cargo-fork.sh test -p shepherd-core` y los tests de herdr de las áreas tocadas en verde (los tests intermitentes de plugins conocidos pasan en solitario: anótalos, no los arregles). Con `shepherd.enabled` apagado ningún test existente de herdr cambia de resultado. Termina con un mensaje final claro, que diga cómo abrir cada vista en el binario y qué falta.

## CORRECCIÓN DEL DUEÑO (prioritaria sobre el alcance de arriba)

Los **agentes NO llevan pestaña ni panel propio**: se usan la barra lateral de agentes, el estado por agente, las pestañas y las terminales nativas que **herdr ya trae** (`src/client/shell/agent_sidebar.rs`, `endpoint_agents.rs`, `sidebar.rs`, menú contextual). Lo que aporte Shepherd a los agentes (estado starting/working/idle/blocked/done, destacar "needs you" con su aviso, lanzar con rol/tarea, matar, limpiar con sus confirmaciones) se integra ahí: marcadores en las filas existentes, entradas en el menú contextual y en el menú global, formulario de lanzar como overlay del patrón existente. "Ir a la terminal" = enfocar el pane nativo, sin más. Todo lo que herdr ya hace sigue funcionando igual.
El overlay "Shepherd" nuevo contiene solo lo que herdr no tiene: **Tareas / Runs / Skills**. Retira o reutiliza `panel.rs` como piezas (formularios, confirmaciones) en vez de una vista de agentes paralela. Refleja esto en tu "Diseño F3b".
