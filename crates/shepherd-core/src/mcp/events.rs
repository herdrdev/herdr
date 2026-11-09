//! MCP tools for Shepherd event bus (`publish`, `subscribe`, `poll`, `ack`, `replay`, `list_topics`).

use super::State;
use crate::bus::MessageBus;
use crate::load_agent_registry;
use serde_json::{json, Value};

pub(crate) fn handle_event_tool(
    state: &State,
    agent: &crate::Agent,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    let res = match name {
        "publish" => {
            let topic = args
                .get("topic")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("topic is required")?;
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .ok_or("content is required")?;
            let key = args.get("key").and_then(Value::as_str);
            let bus = MessageBus::open(&state.bus_path).map_err(|e| e.to_string())?;
            let msg = bus
                .publish(topic, content, key, None, &agent.id)
                .map_err(|e| e.to_string())?;
            // F8g: any publish (agent or human) on a run topic reopens a quiesced `done` run.
            if let (Some(dir), Ok(reg)) = (
                state.registry.parent(),
                load_agent_registry(&state.registry),
            ) {
                let _ = crate::service::patterns_run::reopen_quiesced_runs(
                    dir,
                    &reg,
                    topic,
                    &crate::tasks::now_rfc3339(),
                );
            }
            serde_json::to_string(&msg).map_err(|e| e.to_string())?
        }
        "subscribe" => {
            let topic = args
                .get("topic")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("topic is required")?;
            let group = args.get("group").and_then(Value::as_str);
            let bus = MessageBus::open(&state.bus_path).map_err(|e| e.to_string())?;
            let grp = bus
                .subscribe(&agent.id, topic, group)
                .map_err(|e| e.to_string())?;
            serde_json::to_string(&json!({
                "status": "subscribed",
                "agent": agent.id,
                "topic": topic,
                "group": grp
            }))
            .map_err(|e| e.to_string())?
        }
        "poll" => {
            let topic = args
                .get("topic")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("topic is required")?;
            let max = args.get("max").and_then(Value::as_u64).map(|m| m as usize);
            let registry = load_agent_registry(&state.registry).map_err(|e| e.to_string())?;
            let active_members: Vec<String> =
                registry.live().into_iter().map(|a| a.id.clone()).collect();
            let bus = MessageBus::open(&state.bus_path).map_err(|e| e.to_string())?;
            let messages = bus
                .poll_active(&agent.id, topic, max, Some(&active_members))
                .map_err(|e| e.to_string())?;
            serde_json::to_string(&json!({
                "topic": topic,
                "messages": messages
            }))
            .map_err(|e| e.to_string())?
        }
        "ack" => {
            let topic = args
                .get("topic")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("topic is required")?;
            let partition = args
                .get("partition")
                .and_then(Value::as_u64)
                .map(|p| p as u32);
            let offset = args
                .get("offset")
                .or_else(|| args.get("through"))
                .and_then(Value::as_u64);
            let bus = MessageBus::open(&state.bus_path).map_err(|e| e.to_string())?;
            let committed = bus
                .ack(&agent.id, topic, partition, offset)
                .map_err(|e| e.to_string())?;
            serde_json::to_string(&json!({
                "status": "ok",
                "topic": topic,
                "committed": committed
            }))
            .map_err(|e| e.to_string())?
        }
        "replay" => {
            let topic = args
                .get("topic")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("topic is required")?;
            let from_offset = args.get("from_offset").and_then(Value::as_u64).unwrap_or(0);
            let partition = args
                .get("partition")
                .and_then(Value::as_u64)
                .map(|p| p as u32);
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .map(|l| l as usize);
            let bus = MessageBus::open(&state.bus_path).map_err(|e| e.to_string())?;
            let messages = bus
                .replay(topic, partition, from_offset, limit)
                .map_err(|e| e.to_string())?;
            serde_json::to_string(&json!({
                "topic": topic,
                "from_offset": from_offset,
                "messages": messages
            }))
            .map_err(|e| e.to_string())?
        }
        "list_topics" => {
            let bus = MessageBus::open(&state.bus_path).map_err(|e| e.to_string())?;
            let topics = bus.list_topics().map_err(|e| e.to_string())?;
            serde_json::to_string(&json!({
                "topics": topics
            }))
            .map_err(|e| e.to_string())?
        }
        _ => return Err(format!("unknown tool {name:?}")),
    };
    Ok(res)
}
