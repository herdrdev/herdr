# Encargo para el agente: Shepherd, fase F8g — dos pendientes de la tercera prueba real

Contexto en `SHEPHERD.md`: «Estado F8f» y «Tercera prueba real de patrones tras F8f» (final del archivo). Para cada punto: test que reproduce el fallo primero, luego la corrección. Código implicado: `crates/shepherd-core/src/service/{patterns_run,reconcile,delivery,mod}.rs`, `bus.rs`.

1. **Reabrir un run `done` cuando llega tráfico.** Un `pattern run` en `done` por quiescencia debe volver a `running` cuando se publica un mensaje nuevo en cualquiera de sus tópicos (por el evento de publicación, no por muestreo del estado de los agentes, que puede volver a `idle` más rápido que el ciclo de reconciliación). Debe volver también a `done` por quiescencia cuando se calme. No reabrir `stopped`/`failed`/`degraded`; respetar `paused`. El mensaje publicado por un agente ajeno al run también cuenta.
2. **Grupo fantasma `results brain`** (0 miembros, lag creciente, creado por la topología, un grupo por rol). Averigua cuándo y por qué se crea. Decide con evidencia: no crearlo si nadie lo consume, o excluir de `topic groups` y de la quiescencia los grupos sin miembros que no pertenecen a ningún agente del run; que `topic groups` siga mostrando los grupos reales. Documenta la regla. Revisa si afecta al aviso de despertar (no debe avisar por grupos sin miembros).

## Verificación
Tests por áreas con números reales (`shepherd-core`, `shepherd::`, `client::shell`, `protocol::`, `api::`, `server::`), `--check` del renombre, golden existentes intactos salvo lo explicado (si cambia el de `topic groups`, para y explica), `fmt` **solo** con `rustfmt --edition 2021` sobre archivos propios (NUNCA `cargo fmt --all`; verifica con `git status` que no hay archivos de herdr modificados). Prueba aislada con `./scripts/shepherd-iso.sh` sin agentes reales: run `done` → publicar → `running` → `done`; `topic groups` sin fantasma. Salidas recortadas en `SHEPHERD.md`.

## Cierre
**Estado F8g** en `SHEPHERD.md` con causa raíz de cada punto, regla elegida, archivos de herdr tocados (idealmente 0) y números reales por área. Yo repetiré la prueba con claude y codex.

Reglas operativas de siempre: solo `./cargo-fork.sh` y `./scripts/shepherd-iso.sh`; **no commitear**; nunca push/`gh`/fetch reales; no tocar estado real (nada bajo el `$HOME` real; `SHF_HOME` aislado); tests con tempdirs y XDG aislados (`grep -c testRepo`/`araquesebastian` = 0); no contestar diálogos; **no lanzar agentes reales**; ejecuta de verdad TODAS las áreas de tests y pega números reales (los tests de plugins y `workflow::tests::explicit_approval_pushes_and_opens_pr_then_persists_url` son intermitentes: repítelos solos si fallan).
