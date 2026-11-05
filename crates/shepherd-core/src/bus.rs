//! Shepherd's durable agent message bus. Shepherd owns terminals; Shepherd owns agent mail and its timeline.

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

pub const KIND_MESSAGE: &str = "msg";
pub const KIND_RESULT: &str = "result";
pub const KIND_SYSTEM: &str = "system";

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, schemars::JsonSchema)]
pub struct Message {
    pub id: i64,
    #[serde(default)]
    pub ts: String,
    pub from: String,
    #[serde(default)]
    pub to: String,
    pub kind: String,
    pub content: String,
    #[serde(default)]
    pub topic: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub partition: u32,
    #[serde(default)]
    pub offset: u64,
}

impl Message {
    pub fn directed(
        from: impl Into<String>,
        to: impl Into<String>,
        kind: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: 0,
            ts: String::new(),
            from: from.into(),
            to: to.into(),
            kind: kind.into(),
            content: content.into(),
            topic: String::new(),
            key: String::new(),
            partition: 0,
            offset: 0,
        }
    }
}

impl Serialize for Message {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut count = 4; // id, ts, from, kind, content
        if !self.to.is_empty() {
            count += 1;
        }
        if !self.topic.is_empty() {
            count += 3; // topic, partition, offset
            if !self.key.is_empty() {
                count += 1;
            }
        }
        let mut state = serializer.serialize_struct("Message", count)?;
        state.serialize_field("id", &self.id)?;
        state.serialize_field("ts", &self.ts)?;
        state.serialize_field("from", &self.from)?;
        if !self.to.is_empty() {
            state.serialize_field("to", &self.to)?;
        }
        state.serialize_field("kind", &self.kind)?;
        state.serialize_field("content", &self.content)?;
        if !self.topic.is_empty() {
            state.serialize_field("topic", &self.topic)?;
            if !self.key.is_empty() {
                state.serialize_field("key", &self.key)?;
            }
            state.serialize_field("partition", &self.partition)?;
            state.serialize_field("offset", &self.offset)?;
        }
        state.end()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Topic {
    pub name: String,
    pub partitions: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TopicSummary {
    pub name: String,
    pub partitions: u32,
    pub messages_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConsumerGroupSummary {
    pub topic: String,
    pub group: String,
    pub members: Vec<String>,
    pub committed_offsets: HashMap<u32, u64>,
    /// Messages in the topic (all partitions). Omitted when the topic is empty.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub total_messages: u64,
    /// Unacknowledged messages per partition (only partitions holding messages).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub lag_by_partition: HashMap<u32, u64>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// 64-bit FNV-1a stable hash for deterministic partition routing.
/// Same key always hashes to the same u64 value across restarts and platforms.
pub fn fnv1a_hash(data: &str) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET_BASIS;
    for byte in data.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[derive(Default)]
struct State {
    all: Vec<Message>,
    pending: HashMap<String, Vec<Message>>,
    read_through: HashMap<String, i64>,
    next: i64,
    // Topics: topic_name -> Topic
    topics: HashMap<String, Topic>,
    // Next offset to assign per (topic, partition)
    topic_partition_next_offset: HashMap<(String, u32), u64>,
    // Subscriptions: topic -> group_name -> sorted list of member consumer_ids
    subscriptions: HashMap<String, HashMap<String, Vec<String>>>,
    // Committed offsets: topic -> group_name -> partition -> offset
    committed_offsets: HashMap<String, HashMap<String, HashMap<u32, u64>>>,
    // Round-robin counter per topic for unkeyed messages
    round_robin: HashMap<String, u32>,
    // Paused topics (no message polling/delivery while paused)
    paused_topics: std::collections::HashSet<String>,
}

pub struct MessageBus {
    path: PathBuf,
    state: Mutex<State>,
}

impl MessageBus {
    pub fn open(
        path: impl Into<PathBuf>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let path = path.into();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut state = State {
            next: 1,
            ..State::default()
        };

        // 1. Load topics sidecar if present
        let topics_path = path_with_suffix(&path, ".topics");
        if let Ok(bytes) = std::fs::read(&topics_path) {
            if let Ok(topics) = serde_json::from_slice::<HashMap<String, Topic>>(&bytes) {
                state.topics = topics;
            }
        }

        // 2. Load subscriptions sidecar if present
        let subs_path = path_with_suffix(&path, ".subscriptions");
        if let Ok(bytes) = std::fs::read(&subs_path) {
            if let Ok(subs) =
                serde_json::from_slice::<HashMap<String, HashMap<String, Vec<String>>>>(&bytes)
            {
                state.subscriptions = subs;
            }
        }

        // 3. Load committed offsets sidecar if present
        let offsets_path = path_with_suffix(&path, ".offsets");
        if let Ok(bytes) = std::fs::read(&offsets_path) {
            if let Ok(offsets) = serde_json::from_slice::<
                HashMap<String, HashMap<String, HashMap<u32, u64>>>,
            >(&bytes)
            {
                state.committed_offsets = offsets;
            }
        }

        // 3b. Load paused topics sidecar if present
        let paused_path = path_with_suffix(&path, ".paused");
        if let Ok(bytes) = std::fs::read(&paused_path) {
            if let Ok(paused) = serde_json::from_slice::<std::collections::HashSet<String>>(&bytes)
            {
                state.paused_topics = paused;
            }
        }

        // 4. Load messages JSONL
        match std::fs::File::open(&path) {
            Ok(file) => {
                let mut reader = BufReader::new(file);
                let mut line = Vec::new();
                loop {
                    line.clear();
                    if reader.read_until(b'\n', &mut line)? == 0 {
                        break;
                    }
                    if line.len() > 16 * 1024 * 1024 {
                        return Err("message timeline line exceeds 16 MiB".into());
                    }
                    if let Ok(mut message) = serde_json::from_slice::<Message>(&line) {
                        state.next = state.next.max(message.id + 1);
                        if !message.topic.is_empty() {
                            let next_off = state
                                .topic_partition_next_offset
                                .entry((message.topic.clone(), message.partition))
                                .or_insert(0);
                            message.offset = *next_off;
                            *next_off += 1;

                            // Ensure topic is registered with at least partition + 1
                            let entry =
                                state
                                    .topics
                                    .entry(message.topic.clone())
                                    .or_insert_with(|| Topic {
                                        name: message.topic.clone(),
                                        partitions: 1,
                                    });
                            entry.partitions = entry.partitions.max(message.partition + 1);
                        }
                        state.all.push(message);
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        // 5. Load directed read checkpoints
        match std::fs::read(path_with_suffix(&path, ".read")) {
            Ok(bytes) => state.read_through = serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        // 6. Build pending directed mail
        for message in &state.all {
            if message.topic.is_empty()
                && !message.to.is_empty()
                && message.kind != KIND_SYSTEM
                && message.id > *state.read_through.get(&message.to).unwrap_or(&0)
            {
                state
                    .pending
                    .entry(message.to.clone())
                    .or_default()
                    .push(message.clone());
            }
        }

        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    /// Send a message (directed or topic).
    pub fn send(
        &self,
        mut message: Message,
    ) -> Result<Message, Box<dyn std::error::Error + Send + Sync>> {
        if !message.topic.is_empty() {
            let key = if message.key.is_empty() {
                None
            } else {
                Some(message.key.as_str())
            };
            let partition = if message.partition > 0 {
                Some(message.partition)
            } else {
                None
            };
            return self.publish(
                &message.topic,
                &message.content,
                key,
                partition,
                &message.from,
            );
        }

        let mut state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        message.id = state.next;
        message.ts = super::now_rfc3339();
        state.next += 1;
        let mut line = serde_json::to_vec(&message)?;
        line.push(b'\n');
        append_to_file(&self.path, &line)?;
        state.all.push(message.clone());
        if !message.to.is_empty() && message.kind != KIND_SYSTEM {
            state
                .pending
                .entry(message.to.clone())
                .or_default()
                .push(message.clone());
        }
        Ok(message)
    }

    /// Read and commit directed pending messages for a recipient.
    pub fn read(
        &self,
        recipient: &str,
    ) -> Result<Vec<Message>, Box<dyn std::error::Error + Send + Sync>> {
        let mut state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        let pending = state.pending.remove(recipient).unwrap_or_default();
        if pending.is_empty() {
            return Ok(pending);
        }
        let max_id = pending.iter().map(|message| message.id).max().unwrap_or(0);
        let previous = state.read_through.get(recipient).copied().unwrap_or(0);
        state
            .read_through
            .insert(recipient.to_owned(), previous.max(max_id));
        if let Err(error) = save_json_sidecar(&self.path, ".read", &state.read_through) {
            state.read_through.insert(recipient.to_owned(), previous);
            state.pending.insert(recipient.to_owned(), pending);
            return Err(error);
        }
        Ok(pending)
    }

    /// Read messages since a given global ID.
    pub fn since(
        &self,
        after: i64,
    ) -> Result<Vec<Message>, Box<dyn std::error::Error + Send + Sync>> {
        let state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        Ok(state
            .all
            .iter()
            .filter(|message| message.id > after)
            .cloned()
            .collect())
    }

    /// Last global message ID.
    pub fn last(&self) -> Result<i64, Box<dyn std::error::Error + Send + Sync>> {
        let state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        Ok(state.next - 1)
    }

    // ==========================================
    // Topic & Event Log capabilities (F8b)
    // ==========================================

    /// Declare or update a topic with partition count.
    pub fn topic_create(
        &self,
        name: &str,
        partitions: u32,
    ) -> Result<Topic, Box<dyn std::error::Error + Send + Sync>> {
        let mut state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        let partitions = partitions.max(1);
        let topic = Topic {
            name: name.to_owned(),
            partitions,
        };
        state.topics.insert(name.to_owned(), topic.clone());
        save_json_sidecar(&self.path, ".topics", &state.topics)?;
        Ok(topic)
    }

    /// List registered topics and their message counts.
    pub fn list_topics(
        &self,
    ) -> Result<Vec<TopicSummary>, Box<dyn std::error::Error + Send + Sync>> {
        let state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        let mut summaries = Vec::new();
        for (name, topic) in &state.topics {
            let messages_count = state.all.iter().filter(|m| &m.topic == name).count();
            summaries.push(TopicSummary {
                name: name.clone(),
                partitions: topic.partitions,
                messages_count,
            });
        }
        summaries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(summaries)
    }

    /// Publish a message to a topic.
    /// Auto-creates topic with 1 partition if undeclared.
    /// Chooses partition by explicit value, or `fnv1a_hash(key) % partitions`, or round-robin if no key.
    pub fn publish(
        &self,
        topic: &str,
        content: &str,
        key: Option<&str>,
        partition: Option<u32>,
        from: &str,
    ) -> Result<Message, Box<dyn std::error::Error + Send + Sync>> {
        let mut state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        let partitions = match state.topics.get(topic) {
            Some(t) => t.partitions,
            None => {
                let new_topic = Topic {
                    name: topic.to_owned(),
                    partitions: 1,
                };
                state.topics.insert(topic.to_owned(), new_topic);
                save_json_sidecar(&self.path, ".topics", &state.topics)?;
                1
            }
        };

        let selected_partition = if let Some(p) = partition {
            if p >= partitions {
                return Err(format!(
                    "partition {p} out of range for topic '{topic}' (has {partitions} partitions)"
                )
                .into());
            }
            p
        } else if let Some(k) = key.filter(|k| !k.is_empty()) {
            (fnv1a_hash(k) % (partitions as u64)) as u32
        } else {
            let rr = state.round_robin.entry(topic.to_owned()).or_insert(0);
            let p = *rr % partitions;
            *rr = (*rr + 1) % partitions;
            p
        };

        let next_offset = state
            .topic_partition_next_offset
            .entry((topic.to_owned(), selected_partition))
            .or_insert(0);
        let offset = *next_offset;
        *next_offset += 1;

        let id = state.next;
        state.next += 1;

        let message = Message {
            id,
            ts: super::now_rfc3339(),
            from: from.to_owned(),
            to: String::new(),
            kind: KIND_MESSAGE.into(),
            content: content.to_owned(),
            topic: topic.to_owned(),
            key: key.unwrap_or("").to_owned(),
            partition: selected_partition,
            offset,
        };

        let mut line = serde_json::to_vec(&message)?;
        line.push(b'\n');
        append_to_file(&self.path, &line)?;
        state.all.push(message.clone());
        Ok(message)
    }

    /// Subscribe a consumer agent to a topic in a consumer group.
    /// Default group is consumer's own ID (pub/sub style).
    pub fn subscribe(
        &self,
        consumer: &str,
        topic: &str,
        group: Option<&str>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let mut state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        if !state.topics.contains_key(topic) {
            state.topics.insert(
                topic.to_owned(),
                Topic {
                    name: topic.to_owned(),
                    partitions: 1,
                },
            );
            save_json_sidecar(&self.path, ".topics", &state.topics)?;
        }
        let grp = match group {
            Some(g) if !g.trim().is_empty() => g.trim().to_owned(),
            _ => consumer.to_owned(),
        };

        let groups_for_topic = state.subscriptions.entry(topic.to_owned()).or_default();
        for (g_name, members) in groups_for_topic.iter_mut() {
            if g_name != &grp {
                members.retain(|m| m != consumer);
            }
        }
        let members = groups_for_topic.entry(grp.clone()).or_default();
        if !members.contains(&consumer.to_owned()) {
            members.push(consumer.to_owned());
            members.sort();
        }
        save_json_sidecar(&self.path, ".subscriptions", &state.subscriptions)?;
        Ok(grp)
    }

    /// Unsubscribe a consumer agent from a topic.
    pub fn unsubscribe(
        &self,
        consumer: &str,
        topic: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        if let Some(groups) = state.subscriptions.get_mut(topic) {
            for members in groups.values_mut() {
                members.retain(|m| m != consumer);
            }
            save_json_sidecar(&self.path, ".subscriptions", &state.subscriptions)?;
        }
        Ok(())
    }

    /// Poll topic messages for a consumer in its consumer group.
    pub fn poll(
        &self,
        consumer: &str,
        topic: &str,
        max: Option<usize>,
    ) -> Result<Vec<Message>, Box<dyn std::error::Error + Send + Sync>> {
        self.poll_active(consumer, topic, max, None)
    }

    /// Poll topic messages with an explicit active/alive members filter for rebalancing.
    pub fn poll_active(
        &self,
        consumer: &str,
        topic: &str,
        max: Option<usize>,
        active_members: Option<&[String]>,
    ) -> Result<Vec<Message>, Box<dyn std::error::Error + Send + Sync>> {
        let state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        if state.paused_topics.contains(topic) {
            return Ok(Vec::new());
        }
        let grp = find_consumer_group(&state.subscriptions, topic, consumer)
            .unwrap_or_else(|| consumer.to_owned());

        let all_members = state
            .subscriptions
            .get(topic)
            .and_then(|groups| groups.get(&grp))
            .cloned()
            .unwrap_or_else(|| vec![consumer.to_owned()]);

        let mut live_members: Vec<String> = all_members
            .into_iter()
            .filter(|m| active_members.is_none_or(|actives| actives.contains(m)))
            .collect();
        live_members.sort();

        let consumer_idx = match live_members.iter().position(|m| m == consumer) {
            Some(idx) => idx,
            None => return Ok(Vec::new()),
        };

        let partitions_count = state.topics.get(topic).map(|t| t.partitions).unwrap_or(1);

        let assigned_partitions: Vec<u32> = (0..partitions_count)
            .filter(|p| (*p as usize % live_members.len()) == consumer_idx)
            .collect();

        if assigned_partitions.is_empty() {
            return Ok(Vec::new());
        }

        let mut matched: Vec<Message> = Vec::new();
        for &p in &assigned_partitions {
            let committed = state
                .committed_offsets
                .get(topic)
                .and_then(|g| g.get(&grp))
                .and_then(|pm| pm.get(&p))
                .copied();

            for m in state
                .all
                .iter()
                .filter(|m| m.topic == topic && m.partition == p)
            {
                let is_eligible = match committed {
                    Some(c) => m.offset > c,
                    None => true,
                };
                if is_eligible {
                    matched.push(m.clone());
                }
            }
        }

        matched.sort_by_key(|m| m.id);
        if let Some(limit) = max {
            matched.truncate(limit);
        }
        Ok(matched)
    }

    /// Commit/ack confirmed offset for (topic, group, partition).
    /// Advances committed offset; does not re-deliver confirmed messages.
    pub fn ack(
        &self,
        consumer: &str,
        topic: &str,
        partition: Option<u32>,
        offset: Option<u64>,
    ) -> Result<HashMap<u32, u64>, Box<dyn std::error::Error + Send + Sync>> {
        let mut state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        let grp = find_consumer_group(&state.subscriptions, topic, consumer)
            .unwrap_or_else(|| consumer.to_owned());

        // Pre-compute offsets to commit from state.all before mutably borrowing committed_offsets
        let updates: Vec<(u32, u64)> = if let Some(p) = partition {
            let off = match offset {
                Some(o) => o,
                None => state
                    .all
                    .iter()
                    .filter(|m| m.topic == topic && m.partition == p)
                    .map(|m| m.offset)
                    .max()
                    .unwrap_or(0),
            };
            vec![(p, off)]
        } else if let Some(off) = offset {
            let partitions_matching: Vec<u32> = state
                .all
                .iter()
                .filter(|m| m.topic == topic && m.offset == off)
                .map(|m| m.partition)
                .collect();
            if let Some(&p) = partitions_matching.first() {
                vec![(p, off)]
            } else {
                vec![(0, off)]
            }
        } else {
            let mut max_per_p: HashMap<u32, u64> = HashMap::new();
            for m in state.all.iter().filter(|m| m.topic == topic) {
                let cur = max_per_p.entry(m.partition).or_insert(0);
                *cur = (*cur).max(m.offset);
            }
            max_per_p.into_iter().collect()
        };

        let mut updated = HashMap::new();
        let topic_offsets = state
            .committed_offsets
            .entry(topic.to_owned())
            .or_default()
            .entry(grp)
            .or_default();

        for (p, off) in updates {
            let current = topic_offsets.entry(p).or_insert(0);
            *current = (*current).max(off);
            updated.insert(p, *current);
        }

        save_json_sidecar(&self.path, ".offsets", &state.committed_offsets)?;
        Ok(updated)
    }

    /// Read log from offset without affecting consumer group offsets (zero side effects).
    pub fn replay(
        &self,
        topic: &str,
        partition: Option<u32>,
        from_offset: u64,
        limit: Option<usize>,
    ) -> Result<Vec<Message>, Box<dyn std::error::Error + Send + Sync>> {
        let state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        let mut messages: Vec<Message> = state
            .all
            .iter()
            .filter(|m| m.topic == topic)
            .filter(|m| partition.is_none_or(|p| m.partition == p))
            .filter(|m| m.offset >= from_offset)
            .cloned()
            .collect();
        messages.sort_by_key(|m| m.id);
        if let Some(lim) = limit {
            messages.truncate(lim);
        }
        Ok(messages)
    }

    /// Summary of consumer groups and their committed offsets.
    pub fn consumer_groups(
        &self,
        topic_filter: Option<&str>,
    ) -> Result<Vec<ConsumerGroupSummary>, Box<dyn std::error::Error + Send + Sync>> {
        let state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        let mut groups = Vec::new();
        for (topic, topic_groups) in &state.subscriptions {
            if let Some(tf) = topic_filter {
                if topic != tf {
                    continue;
                }
            }
            for (grp_name, members) in topic_groups {
                let committed_offsets = state
                    .committed_offsets
                    .get(topic)
                    .and_then(|g| g.get(grp_name))
                    .cloned()
                    .unwrap_or_default();
                let mut lag_by_partition: HashMap<u32, u64> = HashMap::new();
                let mut total_messages = 0u64;
                for m in state.all.iter().filter(|m| &m.topic == topic) {
                    total_messages += 1;
                    let lag = lag_by_partition.entry(m.partition).or_insert(0);
                    if committed_offsets
                        .get(&m.partition)
                        .is_none_or(|c| m.offset > *c)
                    {
                        *lag += 1;
                    }
                }
                groups.push(ConsumerGroupSummary {
                    topic: topic.clone(),
                    group: grp_name.clone(),
                    members: members.clone(),
                    committed_offsets,
                    total_messages,
                    lag_by_partition,
                });
            }
        }
        groups.sort_by(|a, b| {
            (a.topic.clone(), a.group.clone()).cmp(&(b.topic.clone(), b.group.clone()))
        });
        Ok(groups)
    }

    /// Pause message consumption on a topic without losing queued or future published messages.
    pub fn pause_topic(&self, topic: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        state.paused_topics.insert(topic.to_string());
        save_json_sidecar(&self.path, ".paused", &state.paused_topics)?;
        Ok(())
    }

    /// Resume message consumption on a previously paused topic.
    pub fn resume_topic(
        &self,
        topic: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        state.paused_topics.remove(topic);
        save_json_sidecar(&self.path, ".paused", &state.paused_topics)?;
        Ok(())
    }

    /// Check if a topic is currently paused.
    pub fn is_topic_paused(&self, topic: &str) -> bool {
        self.state
            .lock()
            .map(|st| st.paused_topics.contains(topic))
            .unwrap_or(false)
    }
}

fn find_consumer_group(
    subscriptions: &HashMap<String, HashMap<String, Vec<String>>>,
    topic: &str,
    consumer: &str,
) -> Option<String> {
    subscriptions.get(topic).and_then(|groups| {
        for (g_name, members) in groups {
            if members.contains(&consumer.to_owned()) {
                return Some(g_name.clone());
            }
        }
        None
    })
}

fn append_to_file(
    path: &Path,
    bytes: &[u8],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut options = OpenOptions::new();
    options.append(true).create(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)?;
    Ok(())
}

fn save_json_sidecar<T: Serialize>(
    path: &Path,
    suffix: &str,
    data: &T,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let tmp = path_with_suffix(path, &format!("{suffix}.tmp"));
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(&tmp)?.write_all(&serde_json::to_vec(data)?)?;
    std::fs::rename(tmp, path_with_suffix(path, suffix))?;
    Ok(())
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn message(from: &str, to: &str, kind: &str, content: &str) -> Message {
        Message {
            id: 0,
            ts: String::new(),
            from: from.into(),
            to: to.into(),
            kind: kind.into(),
            content: content.into(),
            topic: String::new(),
            key: String::new(),
            partition: 0,
            offset: 0,
        }
    }

    fn clean_bus_files(path: &Path) {
        for suffix in [
            "",
            ".read",
            ".read.tmp",
            ".topics",
            ".topics.tmp",
            ".offsets",
            ".offsets.tmp",
            ".subscriptions",
            ".subscriptions.tmp",
            ".paused",
            ".paused.tmp",
        ] {
            let _ = std::fs::remove_file(path_with_suffix(path, suffix));
        }
    }

    #[test]
    fn persists_timeline_and_restores_only_unread_mail_after_restart() {
        let path =
            std::env::temp_dir().join(format!("shepherd-messages-{}.jsonl", std::process::id()));
        clean_bus_files(&path);
        {
            let bus = MessageBus::open(&path).unwrap();
            assert_eq!(
                bus.send(message("brain-1", "coder-1", KIND_MESSAGE, "build the API"))
                    .unwrap()
                    .id,
                1
            );
            assert_eq!(
                bus.send(message("coder-1", "", KIND_SYSTEM, "coder started"))
                    .unwrap()
                    .id,
                2
            );
            assert_eq!(
                bus.send(message(
                    "reviewer-1",
                    "coder-1",
                    KIND_RESULT,
                    "needs changes"
                ))
                .unwrap()
                .id,
                3
            );
            assert_eq!(bus.last().unwrap(), 3);
            let mail = bus.read("coder-1").unwrap();
            assert_eq!(mail.iter().map(|m| m.id).collect::<Vec<_>>(), [1, 3]);
            assert!(bus.read("coder-1").unwrap().is_empty());
        }
        {
            let bus = MessageBus::open(&path).unwrap();
            assert!(bus.read("coder-1").unwrap().is_empty());
            assert_eq!(
                bus.since(1)
                    .unwrap()
                    .iter()
                    .map(|m| m.id)
                    .collect::<Vec<_>>(),
                [2, 3]
            );
            assert_eq!(
                bus.send(message("brain-1", "coder-1", KIND_MESSAGE, "follow up"))
                    .unwrap()
                    .id,
                4
            );
            assert_eq!(bus.read("coder-1").unwrap()[0].id, 4);
        }
        clean_bus_files(&path);
    }

    #[test]
    fn reads_legacy_jsonl_format_with_default_fields() {
        let path = std::env::temp_dir().join(format!(
            "shepherd-legacy-jsonl-{}.jsonl",
            std::process::id()
        ));
        clean_bus_files(&path);

        // Write raw legacy lines without topic, key, partition, offset
        let legacy_line_1 = r#"{"id":1,"ts":"2026-11-05T00:00:00Z","from":"brain-1","to":"coder-1","kind":"msg","content":"legacy message"}"#;
        let legacy_line_2 = r#"{"id":2,"ts":"2026-11-05T00:00:01Z","from":"coder-1","to":"","kind":"system","content":"ready"}"#;
        std::fs::write(&path, format!("{legacy_line_1}\n{legacy_line_2}\n")).unwrap();

        let bus = MessageBus::open(&path).unwrap();
        assert_eq!(bus.last().unwrap(), 2);
        let mail = bus.read("coder-1").unwrap();
        assert_eq!(mail.len(), 1);
        assert_eq!(mail[0].id, 1);
        assert_eq!(mail[0].topic, "");
        assert_eq!(mail[0].key, "");
        assert_eq!(mail[0].partition, 0);
        assert_eq!(mail[0].offset, 0);

        clean_bus_files(&path);
    }

    #[test]
    fn same_key_produces_same_partition() {
        let path =
            std::env::temp_dir().join(format!("shepherd-key-hash-{}.jsonl", std::process::id()));
        clean_bus_files(&path);
        let bus = MessageBus::open(&path).unwrap();
        bus.topic_create("orders", 4).unwrap();

        let msg1 = bus
            .publish("orders", "task 1", Some("customer-42"), None, "brain-1")
            .unwrap();
        let msg2 = bus
            .publish("orders", "task 2", Some("customer-42"), None, "brain-1")
            .unwrap();
        let msg3 = bus
            .publish("orders", "task 3", Some("customer-42"), None, "reviewer-1")
            .unwrap();

        // Same key must always yield the exact same partition
        assert_eq!(msg1.partition, msg2.partition);
        assert_eq!(msg2.partition, msg3.partition);

        // Expected partition calculated with fnv1a_hash
        let expected_partition = (fnv1a_hash("customer-42") % 4) as u32;
        assert_eq!(msg1.partition, expected_partition);

        clean_bus_files(&path);
    }

    #[test]
    fn partition_ordering_and_monotonic_offsets() {
        let path =
            std::env::temp_dir().join(format!("shepherd-part-order-{}.jsonl", std::process::id()));
        clean_bus_files(&path);
        let bus = MessageBus::open(&path).unwrap();
        bus.topic_create("orders", 2).unwrap();

        // Publish explicitly to partition 0
        let p0_m0 = bus
            .publish("orders", "p0 m0", None, Some(0), "brain-1")
            .unwrap();
        let p0_m1 = bus
            .publish("orders", "p0 m1", None, Some(0), "brain-1")
            .unwrap();
        let p0_m2 = bus
            .publish("orders", "p0 m2", None, Some(0), "brain-1")
            .unwrap();

        // Publish explicitly to partition 1
        let p1_m0 = bus
            .publish("orders", "p1 m0", None, Some(1), "coder-1")
            .unwrap();
        let p1_m1 = bus
            .publish("orders", "p1 m1", None, Some(1), "coder-1")
            .unwrap();

        assert_eq!(p0_m0.offset, 0);
        assert_eq!(p0_m1.offset, 1);
        assert_eq!(p0_m2.offset, 2);

        assert_eq!(p1_m0.offset, 0);
        assert_eq!(p1_m1.offset, 1);

        // Global IDs are strictly increasing
        assert_eq!(
            [p0_m0.id, p0_m1.id, p0_m2.id, p1_m0.id, p1_m1.id],
            [1, 2, 3, 4, 5]
        );

        clean_bus_files(&path);
    }

    #[test]
    fn deterministic_distribution_and_rebalancing_on_member_exit() {
        let path =
            std::env::temp_dir().join(format!("shepherd-rebalance-{}.jsonl", std::process::id()));
        clean_bus_files(&path);
        let bus = MessageBus::open(&path).unwrap();
        bus.topic_create("tasks", 3).unwrap();

        // c1 and c2 join the "worker-pool" consumer group
        bus.subscribe("coder-1", "tasks", Some("worker-pool"))
            .unwrap();
        bus.subscribe("coder-2", "tasks", Some("worker-pool"))
            .unwrap();

        // Publish 1 message per partition (0, 1, 2)
        bus.publish("tasks", "task p0", None, Some(0), "brain-1")
            .unwrap();
        bus.publish("tasks", "task p1", None, Some(1), "brain-1")
            .unwrap();
        bus.publish("tasks", "task p2", None, Some(2), "brain-1")
            .unwrap();

        // Sorted members: ["coder-1", "coder-2"]
        // coder-1 (idx 0) gets p0 and p2 (0 % 2 == 0, 2 % 2 == 0)
        // coder-2 (idx 1) gets p1 (1 % 2 == 1)
        let poll1 = bus.poll("coder-1", "tasks", None).unwrap();
        let poll2 = bus.poll("coder-2", "tasks", None).unwrap();

        assert_eq!(
            poll1.iter().map(|m| m.partition).collect::<Vec<_>>(),
            [0, 2]
        );
        assert_eq!(poll2.iter().map(|m| m.partition).collect::<Vec<_>>(), [1]);

        // Now coder-2 exits/dies (active_members = ["coder-1"])
        let poll1_rebalanced = bus
            .poll_active("coder-1", "tasks", None, Some(&["coder-1".to_string()]))
            .unwrap();
        // Since coder-2 never acked partition 1, coder-1 now gets partition 1 as well!
        // coder-1 had un-acked p0 and p2, plus took over un-acked p1 -> receives all 3!
        assert_eq!(
            poll1_rebalanced
                .iter()
                .map(|m| m.partition)
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );

        clean_bus_files(&path);
    }

    #[test]
    fn committed_offsets_persist_across_bus_restart() {
        let path = std::env::temp_dir().join(format!(
            "shepherd-restart-offsets-{}.jsonl",
            std::process::id()
        ));
        clean_bus_files(&path);

        {
            let bus = MessageBus::open(&path).unwrap();
            bus.topic_create("orders", 1).unwrap();
            bus.subscribe("c1", "orders", Some("grp")).unwrap();

            bus.publish("orders", "msg 0", None, Some(0), "brain-1")
                .unwrap();
            bus.publish("orders", "msg 1", None, Some(0), "brain-1")
                .unwrap();
            bus.publish("orders", "msg 2", None, Some(0), "brain-1")
                .unwrap();

            let msgs = bus.poll("c1", "orders", None).unwrap();
            assert_eq!(msgs.len(), 3);

            // Ack through offset 1
            bus.ack("c1", "orders", Some(0), Some(1)).unwrap();

            // Next poll only returns offset 2
            let msgs_after_ack = bus.poll("c1", "orders", None).unwrap();
            assert_eq!(msgs_after_ack.len(), 1);
            assert_eq!(msgs_after_ack[0].offset, 2);
        }

        // Reopen bus from disk
        {
            let bus = MessageBus::open(&path).unwrap();
            // c1 poll should still only see offset 2 because committed offset 1 was persisted
            let msgs_after_restart = bus.poll("c1", "orders", None).unwrap();
            assert_eq!(msgs_after_restart.len(), 1);
            assert_eq!(msgs_after_restart[0].offset, 2);

            // Ack offset 2
            bus.ack("c1", "orders", Some(0), Some(2)).unwrap();
            assert!(bus.poll("c1", "orders", None).unwrap().is_empty());
        }

        clean_bus_files(&path);
    }

    #[test]
    fn at_least_once_delivery_without_ack_redelivers() {
        let path = std::env::temp_dir().join(format!(
            "shepherd-at-least-once-{}.jsonl",
            std::process::id()
        ));
        clean_bus_files(&path);
        let bus = MessageBus::open(&path).unwrap();
        bus.topic_create("orders", 1).unwrap();
        bus.subscribe("c1", "orders", None).unwrap();

        bus.publish("orders", "important task", None, Some(0), "brain-1")
            .unwrap();

        // First poll
        let poll1 = bus.poll("c1", "orders", None).unwrap();
        assert_eq!(poll1.len(), 1);
        assert_eq!(poll1[0].content, "important task");

        // Polling again without ack re-delivers the exact same message
        let poll2 = bus.poll("c1", "orders", None).unwrap();
        assert_eq!(poll2.len(), 1);
        assert_eq!(poll2[0].content, "important task");

        // Once acked, poll is empty
        bus.ack("c1", "orders", Some(0), Some(0)).unwrap();
        assert!(bus.poll("c1", "orders", None).unwrap().is_empty());

        clean_bus_files(&path);
    }

    #[test]
    fn replay_has_no_side_effects() {
        let path =
            std::env::temp_dir().join(format!("shepherd-replay-{}.jsonl", std::process::id()));
        clean_bus_files(&path);
        let bus = MessageBus::open(&path).unwrap();
        bus.topic_create("events", 2).unwrap();
        bus.subscribe("c1", "events", Some("grp1")).unwrap();

        bus.publish("events", "e0", None, Some(0), "b").unwrap();
        bus.publish("events", "e1", None, Some(0), "b").unwrap();
        bus.publish("events", "e2", None, Some(1), "b").unwrap();

        // Consumer polls and acks e0
        let polled = bus.poll("c1", "events", None).unwrap();
        assert!(!polled.is_empty());
        bus.ack("c1", "events", Some(0), Some(0)).unwrap();

        // Replay all events from offset 0
        let replayed = bus.replay("events", None, 0, None).unwrap();
        assert_eq!(replayed.len(), 3);

        // Replay specific partition from offset 1
        let replayed_p0 = bus.replay("events", Some(0), 1, None).unwrap();
        assert_eq!(replayed_p0.len(), 1);
        assert_eq!(replayed_p0[0].content, "e1");

        // Consumer poll is unchanged by replay (still respects committed offset)
        let poll_after_replay = bus.poll("c1", "events", None).unwrap();
        // Since c1 gets partition 0 (or all if single member), it only sees e1 for p0
        assert!(poll_after_replay.iter().all(|m| m.content != "e0"));

        clean_bus_files(&path);
    }

    #[test]
    fn concurrent_publishing_no_lost_messages_or_duplicate_ids() {
        let path = std::env::temp_dir().join(format!(
            "shepherd-concurrent-pub-{}.jsonl",
            std::process::id()
        ));
        clean_bus_files(&path);
        let bus = Arc::new(MessageBus::open(&path).unwrap());
        bus.topic_create("concurrent", 4).unwrap();

        let mut handles = Vec::new();
        for thread_idx in 0..4 {
            let bus_clone = Arc::clone(&bus);
            handles.push(std::thread::spawn(move || {
                for i in 0..25 {
                    bus_clone
                        .publish(
                            "concurrent",
                            &format!("msg t{thread_idx} i{i}"),
                            Some(&format!("key-{i}")),
                            None,
                            &format!("worker-{thread_idx}"),
                        )
                        .unwrap();
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(bus.last().unwrap(), 100);

        // Read all messages and verify unique IDs and strictly monotonic offsets per partition
        let all = bus.replay("concurrent", None, 0, None).unwrap();
        assert_eq!(all.len(), 100);

        let mut ids: Vec<i64> = all.iter().map(|m| m.id).collect();
        ids.sort();
        let expected_ids: Vec<i64> = (1..=100).collect();
        assert_eq!(ids, expected_ids);

        for p in 0..4 {
            let p_msgs: Vec<_> = all.iter().filter(|m| m.partition == p).collect();
            for (idx, m) in p_msgs.iter().enumerate() {
                assert_eq!(m.offset, idx as u64);
            }
        }

        clean_bus_files(&path);
    }

    #[test]
    fn test_topic_pause_and_resume_semantics() {
        let path = std::env::temp_dir().join(format!(
            "shepherd-messages-pause-resume-{}.jsonl",
            std::process::id()
        ));
        clean_bus_files(&path);
        let bus = MessageBus::open(&path).unwrap();

        bus.topic_create("pipeline", 2).unwrap();
        bus.subscribe("worker-1", "pipeline", Some("pool")).unwrap();

        // 1. Publish while running
        bus.publish("pipeline", "job-1", Some("k1"), None, "user")
            .unwrap();
        let polled = bus.poll("worker-1", "pipeline", None).unwrap();
        assert_eq!(polled.len(), 1);
        assert_eq!(polled[0].content, "job-1");
        bus.ack(
            "worker-1",
            "pipeline",
            Some(polled[0].partition),
            Some(polled[0].offset),
        )
        .unwrap();

        // 2. Pause topic
        assert!(!bus.is_topic_paused("pipeline"));
        bus.pause_topic("pipeline").unwrap();
        assert!(bus.is_topic_paused("pipeline"));

        // Publishing during pause still succeeds without loss
        bus.publish("pipeline", "job-2", Some("k2"), None, "user")
            .unwrap();
        bus.publish("pipeline", "job-3", Some("k3"), None, "user")
            .unwrap();

        // Polling during pause yields NO messages
        let paused_polled = bus.poll("worker-1", "pipeline", None).unwrap();
        assert_eq!(paused_polled.len(), 0);

        // 3. Resume topic
        bus.resume_topic("pipeline").unwrap();
        assert!(!bus.is_topic_paused("pipeline"));

        // Polling now yields the retained messages in order
        let resumed_polled = bus.poll("worker-1", "pipeline", None).unwrap();
        assert_eq!(resumed_polled.len(), 2);
        assert_eq!(resumed_polled[0].content, "job-2");
        assert_eq!(resumed_polled[1].content, "job-3");

        clean_bus_files(&path);
    }
}
