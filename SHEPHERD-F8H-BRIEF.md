# Encargo para el agente: Shepherd, fase F8h — quiescencia robusta del run (sin depender del lag)

Contexto en `SHEPHERD.md`: «Estado F8f», «Estado F8g» y «Cuarta prueba real de patrones tras F8g» (final del archivo). Con agentes reales el criterio de F8f (sin lag en los grupos) no se cumple: el orquestador no confirma su propio resumen y el worker se suscribe a tópicos que no declara. Código: `crates/shepherd-core/src/service/{patterns_run,reconcile,mod}.rs` y `bus.rs` (+ `tests_f8g.rs`, tests de F8f).

## Cambio
Sustituye el criterio por uno robusto, con test que reproduce primero el caso real (agentes `idle`, mensajes sin confirmar, el propio resumen del orquestador pendiente, grupo extra del worker) y luego la corrección:
- Un `pattern run` pasa a `done` cuando **todos sus agentes están `idle`/`done`** (ninguno `working`/`blocked`/`starting`) de forma sostenida durante T segundos **y no se ha publicado ningún mensaje** en los tópicos del run durante esos T segundos. **El lag/ack de los grupos ya no cuenta.** T es el periodo configurable de F8f (reutilízalo, mismo default).
- Reapertura: un mensaje publicado en un tópico del run (por cualquiera) lo devuelve de `done` a `running` en el acto, por el evento de publicación (F8g, mantenlo), y vuelve a `done` cuando se vuelve a calmar.
- Se mantienen: no actúa sobre `stopped`/`failed`/`degraded`/`paused`; reconciliación tras reinicio del servidor; `topic groups` y el aviso de despertar (F8e/F8f/F8g) sin cambios de comportamiento; el aviso sigue excluyendo al publicador.
- Documenta la nueva definición y qué dejó de usarse del código de F8f (borra lo muerto, conserva lo que use otra cosa).

## Verificación
Tests por áreas con números reales (`shepherd-core`, `shepherd::`, `client::shell`, `protocol::`, `api::`, `server::`), `--check` del renombre, golden existentes intactos (si alguno cambia, para y explica), `rustfmt --edition 2021` solo sobre archivos propios (NUNCA `cargo fmt --all`; comprueba con `git status` que no hay archivos de herdr modificados). Prueba aislada con `./scripts/shepherd-iso.sh` sin agentes reales: run → `done` con lag pendiente → publicar → `running` → `done`; salidas recortadas en `SHEPHERD.md`.

## Cierre
**Estado F8h** en `SHEPHERD.md` con la definición final, qué se borró, archivos de herdr tocados (idealmente 0) y números reales por área. Yo repetiré la prueba con claude y codex.

Reglas operativas de siempre: solo `./cargo-fork.sh` y `./scripts/shepherd-iso.sh`; **no commitear**; nunca push/`gh`/fetch reales; no tocar estado real (nada bajo el `$HOME` real; `SHF_HOME` aislado); tests con tempdirs y XDG aislados (`grep -c testRepo`/`araquesebastian` = 0); no contestar diálogos; **no lanzar agentes reales**; ejecuta de verdad TODAS las áreas de tests y pega números reales (los tests de plugins y `workflow::tests::explicit_approval_pushes_and_opens_pr_then_persists_url` son intermitentes: repítelos solos si fallan).
