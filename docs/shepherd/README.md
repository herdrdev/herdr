# Documentación de Shepherd

| Documento | Para qué sirve |
|---|---|
| [USO-TUI.md](USO-TUI.md) | Usar Shepherd desde la interfaz: sección Shepherd, pestañas, teclas y ejemplo paso a paso |
| [USO-CLI.md](USO-CLI.md) | Usar Shepherd por comandos, con salidas reales y un ejemplo completo (servidor web en Go) |
| [CONFIGURACION.md](CONFIGURACION.md) | `config.toml` y `config.yaml`: límites, snapshots, roles, vendors, flujo de trabajo y prioridades |
| [PATRONES.md](PATRONES.md) | Referencia del YAML de patrones, las cuatro topologías, validación y estados de un run |
| [AGENTES-Y-SKILLS.md](AGENTES-Y-SKILLS.md) | Ciclo de vida de un agente, roles, skills propias, herramientas MCP, bus de eventos y flujo `run` |

Orden de lectura recomendado: USO-TUI o USO-CLI → CONFIGURACION → PATRONES → AGENTES-Y-SKILLS.

Para empezar: compila con `./cargo-fork.sh build`, activa `[shepherd] enabled = true` en `~/.config/shepherd/config.toml`, abre la sección con `Ctrl+b a` y comprueba la configuración con `shepherd config check`.

Documentos de diseño y estado interno: `SHEPHERD.md` en la raíz del repositorio.
