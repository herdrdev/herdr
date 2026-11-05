# Encargo para el agente: Shepherd, fase F9d — partir módulos grandes sin cambiar comportamiento

Contexto: «Diseño F9b–F9e» 4.4 y «Estado F9c» en `SHEPHERD.md`. Objetivo del dueño: que el código siga el estilo de herdr (módulos por responsabilidad, sin god objects) porque el fork se actualizará con upstream. **Refactor puro de `crates/shepherd-core`: ningún cambio de comportamiento, de API pública ni de salida.** Mismo worktree que F1–F9c. **Siguen vigentes las reglas anteriores** (solo `./cargo-fork.sh` y `./scripts/shepherd-iso.sh`; **no commitear**; nunca push/`gh`/`git fetch` reales; no tocar estado real del usuario; tests con tempdirs y XDG aislados, `grep -c testRepo` y `grep -c araquesebastian` de la salida de shepherd-core = 0; no contestar diálogos; no lanzar agentes reales; `scripts/rename-from-herdr.sh --check` limpio; formatea solo archivos propios y revierte los de herdr que `fmt` toque; no regeneres golden files: si alguno cambia, para y explica). **Ejecuta de verdad todas las áreas de tests y pega números reales** (`shepherd-core`, `shepherd::`, `client::shell`, `protocol::`, `api::`, `server::`).

## Tamaños actuales (líneas)
`service.rs` 4272, `cli.rs` 3984, `terminals.rs` 2164, `mcp.rs` 1941, `lib.rs` 1898, `patterns.rs` 1418, `skills.rs` 1418, `workflow.rs` 1361.

## Pasos (build + `-p shepherd-core` en verde tras CADA paso; un paso = mover código, no reescribirlo)
1. `terminals.rs` → `terminals/{mod,in_process,socket,fakes}.rs` (trait y tipos comunes en `mod.rs`).
2. `mcp.rs` → `mcp/{mod,server,tools,events}.rs`.
3. `service.rs` → `service/{mod,registry,spawn,reconcile,delivery,gates}.rs`.
4. `workflow.rs` → `workflow/{mod,runner,verification,pr}.rs`.
5. `lib.rs`: solo re-exportaciones y modelos de datos; mueve el resto (p. ej. el parser de planes `field_value`, helpers) a módulos con nombre propio.
6. `cli.rs` de core: tras F9c el CLI real vive en `src/cli/shepherd*.rs`. Averigua con evidencia (grep de usos, `main.rs` de core) qué parte de `crates/shepherd-core/src/cli.rs` sigue viva. Elimina lo muerto y parte lo vivo en módulos pequeños (`cli/{mod,tasks,patterns,topics,…}.rs`); si algo no puede quitarse (p. ej. lo usa el binario independiente de core o los tests de caracterización), déjalo y anota el motivo. Objetivo orientativo: ningún archivo de core por encima de ~1500 líneas salvo justificación.

## Reglas de movimiento
- Mantén las rutas públicas: `pub use` en cada `mod.rs` para que `shepherd_core::X` y los usos desde `src/shepherd/**` compilen sin tocar archivos de herdr (si hay que tocar alguno, mínimo y anotado).
- Los tests viajan con el código que prueban (`#[cfg(test)]` junto al módulo) o quedan en `tests/`; el número total de tests no debe bajar (compara antes/después).
- Sin renombres de funciones, sin cambios de visibilidad más allá de `pub(crate)`/`pub(super)` necesarios.

## Verificación
Build; `-p shepherd-core` (117 unit + 148 caracterización: mismos números o más); áreas del binario (`shepherd::` 52, `client::shell` 391, `protocol::` 156, `server::` 318, `api::` con los 4 tests de plugins intermitentes que pasan solos); `--check` del renombre; `fmt --check` en archivos propios; golden files sin tocar (`git status crates/shepherd-core/tests/golden`); una pasada con `./scripts/shepherd-iso.sh` (`start`, un comando de cada grupo con y sin `--no-server`, `stop`).

## Cierre
Sección **Estado F9d** en `SHEPHERD.md`: tabla de tamaños antes/después por archivo, número de tests antes/después, archivos de herdr tocados (idealmente 0), números reales por área, qué NO queda (F9e: config/persistencia). Mensaje final claro.
