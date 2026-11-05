//! F12: startup prompt and task context for agents launched by `pattern run`.
//!
//! The pattern prompt *replaces* the role's workflow script (it is not appended): a pattern
//! agent may have any role; the role only contributes vendor/model, permissions and skills.

use super::patterns_run::{PatternRun, PatternRunAgent};
use crate::patterns::{Pattern, PatternAgent};

/// Placeholder replaced by the spawner with the registry id of the agent (ids are assigned
/// at spawn time, after the prompt is composed).
pub(crate) const AGENT_ID_PLACEHOLDER: &str = "{{agent_id}}";

fn topics(list: &[String]) -> String {
    if list.is_empty() {
        "ninguno".to_owned()
    } else {
        list.iter()
            .map(|t| format!("`{t}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn persona(prompt: Option<&str>, role: &str) -> String {
    match prompt.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => p.to_owned(),
        None => format!("(sin persona definida; actúa según el rol `{role}` dentro del patrón)"),
    }
}

/// Startup prompt for one agent slot of a pattern run.
pub(crate) fn startup_prompt(
    pattern: &Pattern,
    def: &PatternAgent,
    task: &str,
    starter: bool,
) -> String {
    let order = if starter {
        "Empieza ahora: el objetivo del run es tu orden de arranque; haz tu parte y publica el resultado en tus tópicos de salida.".to_owned()
    } else {
        format!(
            "Espera mensajes en {}; actúa cuando te avisen. No empieces por tu cuenta: el objetivo del run es solo contexto.",
            topics(&def.consumes)
        )
    };
    format!(
        "Estás en el patrón `{name}` (`{kind}`), tu id es `{AGENT_ID_PLACEHOLDER}` (definición `{def_id}`, rol `{role}`) y tu persona es: {persona}\n\n\
         Tus tópicos: publicas en {publishes}; consumes de {consumes}.\n\
         Cómo colaborar: skill `shepherd-events` o herramientas MCP (publish, poll, subscribe, ack). Ya estás suscrito a tus tópicos: no pases `group` en `subscribe` salvo necesidad (cambiaría tu grupo de consumo). Al publicar no pases `key` ni `partition` salvo necesidad real de orden: sin clave el bus reparte los mensajes entre particiones y, con ellos, entre los miembros de un grupo (con la misma clave todo cae en una partición y un solo worker lo recibe).\n\n\
         Objetivo del run: {task}\n\n\
         {order}\n\n\
         Reglas: actúa solo según tu persona; no pidas contexto de tarea ni diff (no hay tarea ni coder: `get_task_context` solo devuelve este objetivo y la topología); publica tu resultado en tus tópicos y termina tu turno; recibirás avisos cuando lleguen mensajes.\n",
        name = pattern.name,
        kind = pattern.kind,
        def_id = def.id,
        role = def.role,
        persona = persona(def.prompt.as_deref(), &def.role),
        publishes = topics(&def.publishes),
        consumes = topics(&def.consumes),
        task = task.trim(),
    )
}

fn line(a: &PatternRunAgent) -> String {
    let level = a.level.map(|l| format!(" nivel {l}")).unwrap_or_default();
    format!(
        "- `{}` (rol {}{level}{}): publica en {}; consume de {}",
        a.id,
        a.role,
        if a.start { ", inicia el patrón" } else { "" },
        topics(&a.publishes),
        topics(&a.consumes),
    )
}

/// What `get_task_context` returns for an agent of a pattern run: objective, topology and the
/// caller's own persona.
pub(crate) fn run_context(run: &PatternRun, agent_id: &str) -> String {
    let me = run.agents.iter().find(|a| a.id == agent_id);
    let mut out = format!(
        "# Run de patrón {}\n\nPatrón: `{}` (`{}`)\nObjetivo del run: {}\n\n## Topología\n",
        run.id, run.pattern, run.kind, run.task
    );
    for a in &run.agents {
        out.push_str(&line(a));
        out.push('\n');
    }
    if !run.topics.is_empty() {
        out.push_str(&format!("\nTópicos: {}\n", topics(&run.topics)));
    }
    if let Some(me) = me {
        out.push_str(&format!(
            "\n## Tú\n\nid `{}`, rol {}. Persona: {}\n{}\n",
            me.id,
            me.role,
            persona(me.prompt.as_deref(), &me.role),
            if me.start {
                "Eres iniciador: empieza con el objetivo."
            } else {
                "No eres iniciador: espera mensajes en tus tópicos de entrada."
            }
        ));
    }
    out
}
