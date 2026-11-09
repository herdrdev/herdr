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
        None => {
            format!("(no persona defined; act according to the `{role}` role within the pattern)")
        }
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
        "Start now: the run objective is your starting order; do your part and publish the result to your output topics.".to_owned()
    } else {
        format!(
            "Wait for messages on {}; act when notified. Do not start on your own: the run objective is context only.",
            topics(&def.consumes)
        )
    };
    format!(
        "You are in the pattern `{name}` (`{kind}`), your id is `{AGENT_ID_PLACEHOLDER}` (definition `{def_id}`, role `{role}`) and your persona is: {persona}\n\n\
         Your topics: you publish to {publishes}; you consume from {consumes}.\n\
         How to collaborate: skill `shepherd-events` or MCP tools (publish, poll, subscribe, ack). You are already subscribed to your topics: do not pass `group` to `subscribe` unless needed (it would change your consumer group). When publishing, do not pass `key` or `partition` unless ordering really matters: without a key the bus spreads messages across partitions and therefore across the members of a group (with the same key everything lands in one partition and a single worker receives it).\n\n\
         Run objective: {task}\n\n\
         {order}\n\n\
         Rules: act only according to your persona; do not ask for task context or a diff (there is no task or coder: `get_task_context` only returns this objective and the topology); publish your result to your topics and end your turn; you will be notified when messages arrive.\n",
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
    let level = a.level.map(|l| format!(" level {l}")).unwrap_or_default();
    format!(
        "- `{}` (role {}{level}{}): publishes to {}; consumes from {}",
        a.id,
        a.role,
        if a.start { ", starts the pattern" } else { "" },
        topics(&a.publishes),
        topics(&a.consumes),
    )
}

/// What `get_task_context` returns for an agent of a pattern run: objective, topology and the
/// caller's own persona.
pub(crate) fn run_context(run: &PatternRun, agent_id: &str) -> String {
    let me = run.agents.iter().find(|a| a.id == agent_id);
    let mut out = format!(
        "# Pattern run {}\n\nPattern: `{}` (`{}`)\nRun objective: {}\n\n## Topology\n",
        run.id, run.pattern, run.kind, run.task
    );
    for a in &run.agents {
        out.push_str(&line(a));
        out.push('\n');
    }
    if !run.topics.is_empty() {
        out.push_str(&format!("\nTopics: {}\n", topics(&run.topics)));
    }
    if let Some(me) = me {
        out.push_str(&format!(
            "\n## You\n\nid `{}`, role {}. Persona: {}\n{}\n",
            me.id,
            me.role,
            persona(me.prompt.as_deref(), &me.role),
            if me.start {
                "You are the starter: begin with the objective."
            } else {
                "You are not the starter: wait for messages on your input topics."
            }
        ));
    }
    out
}
