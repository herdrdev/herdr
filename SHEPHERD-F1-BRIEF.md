# Encargo para el agente: Shepherd como fork de herdr, fase F1

Contexto: lee `SHEPHERD.md` (decisión del dueño: Shepherd es un fork de herdr, "un herdr con más funciones") y **el `AGENTS.md` de este repo, cuyas reglas aplican a los forks** (estado separado de runtime, render puro, sin god objects, código de plataforma aislado, rutas de rendimiento). Después `crates/shepherd-core/docs/rust-architecture.md` (qué hace el crate `shepherd-core`, que ya está en el workspace y pasa sus 52 tests).

## Reglas obligatorias

- **Cambios mínimos en archivos de herdr.** Todo lo nuevo va en archivos nuevos (`crates/shepherd-core/`, o un módulo nuevo `src/shepherd/`). Cada archivo original que tengas que tocar se limita a unas pocas líneas de enganche y se lista en una sección "Archivos de herdr modificados" de `SHEPHERD.md` (archivo, qué línea de enganche y por qué). Así los rebases desde herdr cuestan poco.
- **Compila y prueba solo con `./cargo-fork.sh <args>`** (fija Rust 1.96.1 y Zig). Ejemplos: `./cargo-fork.sh test -p shepherd-core`, `./cargo-fork.sh build`, `./cargo-fork.sh test --bin herdr <filtro>`. La suite completa de herdr tiene un test intermitente conocido (`app::api::plugins::tests::manifest_action_invoke_injects_plugin_paths`: pasa en solitario, falla dentro de la suite); no lo "arregles", anótalo.
- **No hagas commits** (tu regla global lo prohíbe): deja los cambios en el árbol, con las pruebas en verde tras cada punto; los reviso y los commiteo yo. **Nunca** `git push`, ni PRs, ni issues, ni nada contra el repositorio original; el remoto `upstream` tiene el push desactivado a propósito.
- **No ejecutes el binario compilado contra el herdr real del usuario** (sus sesiones, su socket, sus terminales, `hc`, `herdr-app`). Si necesitas ejecutarlo, que sea con una sesión propia y aislada (`--session f1test`, `XDG_CONFIG_HOME` y `XDG_STATE_HOME` en un directorio temporal) y al terminar para tu servidor. Preferible: tests que no lancen procesos reales.
- No inventes la API de herdr: lee su código (`src/api/`, `src/app/api/`, `src/api/schema/`) y `docs/`. Lo que no puedas verificar, márcalo **no verificado**.
- Dependencias: lo mínimo; las del crate ya están. Nada de secretos ni tokens en logs.
- Nunca contestes diálogos de confianza o login por un humano.
- Si un comando no está permitido y pide permiso, para y dilo en tu respuesta en vez de insistir.

## Alcance F1 (en este orden; pruebas en verde tras cada punto)

1. **Adaptador en proceso para `trait Terminals`** (`crates/shepherd-core/src/terminals.rs` ya define el trait y los adaptadores `HerdrSocket`, `HcCli`, `InMemoryTerminals`): estudia cómo `src/api/server.rs` y `src/app/api/*.rs` atienden las peticiones del socket contra el `App`, y crea un `HerdrInProcess` que implemente el trait **sin pasar por el socket** (misma ruta interna que usa el servidor de la API: canal de peticiones hacia el `App`). Si esa ruta interna no permite una integración limpia sin tocar mucho código de herdr, implementa primero el adaptador sobre el cliente de la API local ya existente (`src/api/client.rs`) y documenta en `SHEPHERD.md` qué cambio de herdr haría falta para ir en proceso de verdad y por qué no se hizo.
2. **Subcomando integrado**: el binario de herdr gana un grupo `shepherd` (`herdr shepherd agents --json`, `herdr shepherd task list --json`, `herdr shepherd roles list --json`) que delega en `shepherd_core` (extrae de `crates/shepherd-core/src/main.rs` una función `run_cli(args)` reutilizable) y usa el adaptador del punto 1 cuando hay un herdr en marcha. El renombrado del binario a `shepherd` es de la fase F5: no lo hagas ahora.
3. **Panel de agentes de solo lectura** (solo si 1 y 2 están completos y el tiempo lo permite): una vista nueva en el TUI que liste los agentes del registro de shepherd (rol, vendor, estado, aviso "needs you") siguiendo los patrones de UI de herdr y la regla de render puro. **La terminal nativa de herdr de cada agente se mantiene intacta y sigue siendo la forma de ver y hablar con él**; no emules terminales ni dibujes salidas de agentes dentro de nuestras vistas.
4. **Estado final**: sección "Estado F1" en `SHEPHERD.md` con lo hecho, lo no verificado, los archivos de herdr tocados y lo que queda para F2.

## Criterio de terminado

`./cargo-fork.sh test -p shepherd-core` y `./cargo-fork.sh build` en verde, más los tests de herdr de las áreas que toques. Si te bloqueas, deja los puntos anteriores completos, documenta el bloqueo y para.
