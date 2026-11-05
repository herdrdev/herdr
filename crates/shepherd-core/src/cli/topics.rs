//! Message bus topic listing, creation, publishing, tailing, replay, and consumer groups CLI commands.

use crate::{bus::MessageBus, config, shepherd::ShepherdClient};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub(crate) fn resolve_bus_path(root: &Path) -> PathBuf {
    let local = root.join(".shepherd").join("messages.jsonl");
    if local.exists() {
        return local;
    }
    let local_direct = root.join("messages.jsonl");
    if local_direct.exists() {
        return local_direct;
    }
    let state = config::state_dir().join("messages.jsonl");
    if state.exists() {
        return state;
    }
    local
}

pub(crate) fn topic_list_command(
    root: PathBuf,
    _args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(client) = server {
        let data = client.shepherd_call("topics", serde_json::json!({}))?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            print_human_topics_from_value(writer, &data)?;
        }
        return Ok(());
    }
    let bus_path = resolve_bus_path(&root);
    let bus = MessageBus::open(&bus_path)?;
    let list = bus.list_topics()?;
    if json {
        writeln!(writer, "{}", serde_json::to_string_pretty(&list)?)?;
    } else {
        print_human_topics(writer, &list)?;
    }
    Ok(())
}

fn print_human_topics(
    writer: &mut dyn std::io::Write,
    list: &[crate::bus::TopicSummary],
) -> std::io::Result<()> {
    if list.is_empty() {
        writeln!(writer, "no topics found")?;
        return Ok(());
    }
    writeln!(writer, "NAME\tPARTITIONS\tMESSAGES")?;
    for t in list {
        writeln!(writer, "{}\t{}\t{}", t.name, t.partitions, t.messages_count)?;
    }
    Ok(())
}

fn print_human_topics_from_value(
    writer: &mut dyn std::io::Write,
    data: &Value,
) -> std::io::Result<()> {
    let Some(arr) = data.as_array() else {
        writeln!(writer, "{}", data)?;
        return Ok(());
    };
    if arr.is_empty() {
        writeln!(writer, "no topics found")?;
        return Ok(());
    }
    writeln!(writer, "NAME\tPARTITIONS\tMESSAGES")?;
    for item in arr {
        let name = item.get("name").and_then(Value::as_str).unwrap_or("-");
        let partitions = item.get("partitions").and_then(Value::as_u64).unwrap_or(1);
        let count = item
            .get("messages_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        writeln!(writer, "{name}\t{partitions}\t{count}")?;
    }
    Ok(())
}

pub(crate) fn topic_create_command(
    root: PathBuf,
    args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut name = None;
    let mut partitions = 1u32;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--partitions" => {
                if let Some(val) = iter.next() {
                    partitions = val.parse().unwrap_or(1);
                }
            }
            other if !other.starts_with("--") && name.is_none() => {
                name = Some(other.to_string());
            }
            _ => {}
        }
    }
    let name =
        name.ok_or("topic name is required: shepherd topic create <name> [--partitions <n>]")?;
    if let Some(client) = server {
        let res = client.shepherd_call(
            "topic_create",
            serde_json::json!({
                "name": name,
                "partitions": partitions,
            }),
        )?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&res)?)?;
        } else {
            writeln!(
                writer,
                "created topic '{}' with {} partition(s)",
                name, partitions
            )?;
        }
        return Ok(());
    }
    let bus_path = resolve_bus_path(&root);
    let bus = MessageBus::open(&bus_path)?;
    let topic = bus.topic_create(&name, partitions)?;
    if json {
        writeln!(writer, "{}", serde_json::to_string_pretty(&topic)?)?;
    } else {
        writeln!(
            writer,
            "created topic '{}' with {} partition(s)",
            topic.name, topic.partitions
        )?;
    }
    Ok(())
}

pub(crate) fn topic_publish_command(
    root: PathBuf,
    args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut topic = None;
    let mut content = None;
    let mut key = None;
    let mut partition = None;
    let mut from = "user".to_string();

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--key" => key = iter.next(),
            "--partition" => partition = iter.next().and_then(|v| v.parse().ok()),
            "--from" => from = iter.next().unwrap_or_else(|| "user".into()),
            other if !other.starts_with("--") => {
                if topic.is_none() {
                    topic = Some(other.to_string());
                } else if content.is_none() {
                    content = Some(other.to_string());
                } else {
                    let mut c = content.take().unwrap();
                    c.push(' ');
                    c.push_str(other);
                    content = Some(c);
                }
            }
            _ => {}
        }
    }
    let topic = topic.ok_or("topic is required: shepherd topic publish <topic> <content>")?;
    let content = content.ok_or("content is required: shepherd topic publish <topic> <content>")?;

    if let Some(client) = server {
        let mut payload = serde_json::json!({
            "topic": topic,
            "content": content,
            "from": from,
        });
        if let Some(ref k) = key {
            payload["key"] = serde_json::json!(k);
        }
        if let Some(p) = partition {
            payload["partition"] = serde_json::json!(p);
        }
        let res = client.shepherd_call("publish", payload)?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&res)?)?;
        } else {
            let id = res["id"].as_i64().unwrap_or(0);
            let part = res["partition"].as_u64().unwrap_or(0);
            let offset = res["offset"].as_u64().unwrap_or(0);
            writeln!(
                writer,
                "published message {} to {}[{}]#{}",
                id, topic, part, offset
            )?;
        }
        return Ok(());
    }
    let bus_path = resolve_bus_path(&root);
    let bus = MessageBus::open(&bus_path)?;
    let msg = bus.publish(&topic, &content, key.as_deref(), partition, &from)?;
    if json {
        writeln!(writer, "{}", serde_json::to_string_pretty(&msg)?)?;
    } else {
        writeln!(
            writer,
            "published message {} to {}[{}]#{}",
            msg.id, msg.topic, msg.partition, msg.offset
        )?;
    }
    Ok(())
}

pub(crate) fn topic_tail_command(
    root: PathBuf,
    args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    topic_replay_inner(root, args, 0, server, json, writer)
}

pub(crate) fn topic_replay_command(
    root: PathBuf,
    args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    topic_replay_inner(root, args, 0, server, json, writer)
}

fn topic_replay_inner(
    root: PathBuf,
    args: Vec<String>,
    default_from: u64,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut topic = None;
    let mut partition = None;
    let mut from_offset = default_from;
    let mut limit = None;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--partition" => partition = iter.next().and_then(|v| v.parse().ok()),
            "--from" | "--from-offset" => {
                from_offset = iter.next().and_then(|v| v.parse().ok()).unwrap_or(0)
            }
            "--limit" => limit = iter.next().and_then(|v| v.parse().ok()),
            other if !other.starts_with("--") && topic.is_none() => {
                topic = Some(other.to_string());
            }
            _ => {}
        }
    }
    let topic = topic.ok_or("topic is required: shepherd topic tail|replay <topic>")?;

    if let Some(client) = server {
        let mut payload = serde_json::json!({
            "topic": topic,
            "from_offset": from_offset,
        });
        if let Some(p) = partition {
            payload["partition"] = serde_json::json!(p);
        }
        if let Some(l) = limit {
            payload["limit"] = serde_json::json!(l);
        }
        let data = client.shepherd_call("topic_tail", payload)?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            print_human_topic_messages_from_value(writer, &data)?;
        }
        return Ok(());
    }
    let bus_path = resolve_bus_path(&root);
    let bus = MessageBus::open(&bus_path)?;
    let msgs = bus.replay(&topic, partition, from_offset, limit)?;
    if json {
        writeln!(writer, "{}", serde_json::to_string_pretty(&msgs)?)?;
    } else {
        print_human_topic_messages(writer, &msgs)?;
    }
    Ok(())
}

fn print_human_topic_messages(
    writer: &mut dyn std::io::Write,
    msgs: &[crate::bus::Message],
) -> std::io::Result<()> {
    if msgs.is_empty() {
        writeln!(writer, "no messages")?;
        return Ok(());
    }
    for m in msgs {
        let key_str = if m.key.is_empty() {
            String::new()
        } else {
            format!(" (key={})", m.key)
        };
        writeln!(
            writer,
            "[{}] {}[{}]:#{} from {}{}: {}",
            m.ts, m.topic, m.partition, m.offset, m.from, key_str, m.content
        )?;
    }
    Ok(())
}

fn print_human_topic_messages_from_value(
    writer: &mut dyn std::io::Write,
    data: &Value,
) -> std::io::Result<()> {
    let Some(arr) = data.as_array() else {
        writeln!(writer, "{}", data)?;
        return Ok(());
    };
    if arr.is_empty() {
        writeln!(writer, "no messages")?;
        return Ok(());
    }
    for m in arr {
        let ts = m.get("ts").and_then(Value::as_str).unwrap_or("-");
        let topic = m.get("topic").and_then(Value::as_str).unwrap_or("-");
        let partition = m.get("partition").and_then(Value::as_u64).unwrap_or(0);
        let offset = m.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let from = m.get("from").and_then(Value::as_str).unwrap_or("-");
        let content = m.get("content").and_then(Value::as_str).unwrap_or("");
        let key = m.get("key").and_then(Value::as_str).unwrap_or("");
        let key_str = if key.is_empty() {
            String::new()
        } else {
            format!(" (key={key})")
        };
        writeln!(
            writer,
            "[{ts}] {topic}[{partition}]:#{offset} from {from}{key_str}: {content}"
        )?;
    }
    Ok(())
}

pub(crate) fn topic_groups_command(
    root: PathBuf,
    args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let topic = args.into_iter().find(|a| !a.starts_with("--"));
    if let Some(client) = server {
        let mut payload = serde_json::json!({});
        if let Some(ref t) = topic {
            payload["topic"] = serde_json::json!(t);
        }
        let data = client.shepherd_call("consumer_groups", payload)?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            print_human_consumer_groups_from_value(writer, &data)?;
        }
        return Ok(());
    }
    let bus_path = resolve_bus_path(&root);
    let bus = MessageBus::open(&bus_path)?;
    let groups = bus.consumer_groups(topic.as_deref())?;
    if json {
        writeln!(writer, "{}", serde_json::to_string_pretty(&groups)?)?;
    } else {
        print_human_consumer_groups(writer, &groups)?;
    }
    Ok(())
}

fn print_human_consumer_groups(
    writer: &mut dyn std::io::Write,
    groups: &[crate::bus::ConsumerGroupSummary],
) -> std::io::Result<()> {
    if groups.is_empty() {
        writeln!(writer, "no consumer groups")?;
        return Ok(());
    }
    writeln!(writer, "TOPIC\tGROUP\tMEMBERS\tOFFSETS")?;
    for g in groups {
        let members_str = if g.members.is_empty() {
            "-".into()
        } else {
            g.members.join(",")
        };
        let mut offsets_vec: Vec<_> = g
            .committed_offsets
            .iter()
            .map(|(p, off)| format!("p{p}:#{off}"))
            .collect();
        offsets_vec.sort();
        let offsets_str = if offsets_vec.is_empty() {
            "-".into()
        } else {
            offsets_vec.join(", ")
        };
        writeln!(
            writer,
            "{}\t{}\t{}\t{}",
            g.topic, g.group, members_str, offsets_str
        )?;
    }
    Ok(())
}

fn print_human_consumer_groups_from_value(
    writer: &mut dyn std::io::Write,
    data: &Value,
) -> std::io::Result<()> {
    let Some(arr) = data.as_array() else {
        writeln!(writer, "{}", data)?;
        return Ok(());
    };
    if arr.is_empty() {
        writeln!(writer, "no consumer groups")?;
        return Ok(());
    }
    writeln!(writer, "TOPIC\tGROUP\tMEMBERS\tOFFSETS")?;
    for item in arr {
        let topic = item.get("topic").and_then(Value::as_str).unwrap_or("-");
        let group = item.get("group").and_then(Value::as_str).unwrap_or("-");
        let members_arr = item.get("members").and_then(Value::as_array);
        let members_str = match members_arr {
            Some(m) if !m.is_empty() => m
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(","),
            _ => "-".into(),
        };
        let offsets_map = item.get("committed_offsets").and_then(Value::as_object);
        let offsets_str = match offsets_map {
            Some(o) if !o.is_empty() => {
                let mut parts: Vec<_> = o.iter().map(|(k, v)| format!("p{k}:#{v}")).collect();
                parts.sort();
                parts.join(", ")
            }
            _ => "-".into(),
        };
        writeln!(writer, "{topic}\t{group}\t{members_str}\t{offsets_str}")?;
    }
    Ok(())
}
