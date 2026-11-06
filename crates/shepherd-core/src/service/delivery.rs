//! First prompt delivery and pending message delivery to non-MCP agents.

use super::{
    is_missing_terminal_error, status_query_target, OrchestrationService, BLOCKED_HUMAN_NOTICE,
    NEVER_READY_ERROR,
};
use crate::bus::{Message, KIND_SYSTEM};
use crate::service::format_shep_message;
use crate::service::format_shep_topic_message;
use crate::terminals::PromptOutcome;
use crate::{Agent, AgentRegistryFile};
use std::time::Duration;

impl OrchestrationService {
    /// Requirement 2: Wait for the agent to accept input (up to `ready_timeout`, default 10m),
    /// deliver its first prompt, and verify that it reacted (`working` or `seq` advanced),
    /// resending up to `max_prompt_resends` (3) times before failing with a clear message.
    ///
    /// Sets `notice` (`BLOCKED_HUMAN_NOTICE`) if the terminal stays `blocked` for >= `notice_after`
    /// (3s default), ONLY for `blocked` (never for "not ready"). Clears `notice` on delivery,
    /// failure, or kill. Never answers a dialog on behalf of the human.
    pub fn deliver_first_prompt(&self, agent_id: &str, prompt: &str) -> Result<(), String> {
        let result = self.deliver_first_prompt_inner(agent_id, prompt);
        self.log_err("deliver_first_prompt", result)
    }

    fn deliver_first_prompt_inner(&self, agent_id: &str, prompt: &str) -> Result<(), String> {
        let start_ms = self.clock.now_ms();
        let ready_timeout_ms = self.service_config.ready_timeout.as_millis() as u64;
        let notice_after_ms = self.service_config.notice_after.as_millis() as u64;
        let mut blocked_since_ms: Option<u64> = None;
        let mut missing_since_ms: Option<u64> = None;

        // Phase 1: Wait until the agent is ready to accept input.
        let baseline_seq = loop {
            let mut registry = self.load_registry()?;
            let Some(agent) = registry.agents.iter_mut().find(|a| a.id == agent_id) else {
                return Err(format!("unknown agent {agent_id:?}"));
            };
            if agent.gone() {
                let state = agent.state.clone();
                if !agent.notice.is_empty() {
                    agent.notice.clear();
                    self.save_registry(&registry)?;
                }
                return Err(format!(
                    "agent {agent_id} ended in state {state} before prompt delivery"
                ));
            }
            let target = status_query_target(agent);
            let now_ms = self.clock.now_ms();

            let status_res = self.terminals.agent_status(&target);
            match status_res {
                Ok(st) if st.state == "finished" => {
                    let err_msg = "el panel se cerró antes de entregar el primer prompt".to_owned();
                    agent.state = "exited".into();
                    agent.notice.clear();
                    agent.error = err_msg.clone();
                    agent.updated = self.clock.now_rfc3339();
                    self.save_registry(&registry)?;
                    return Err(err_msg);
                }
                Ok(st) => match st.status.as_str() {
                    "idle" | "done" | "working" => {
                        if !agent.notice.is_empty() {
                            agent.notice.clear();
                            self.save_registry(&registry)?;
                        }
                        break st.seq;
                    }
                    "blocked" => {
                        let since = *blocked_since_ms.get_or_insert(now_ms);
                        if now_ms.saturating_sub(since) >= notice_after_ms
                            && agent.notice != BLOCKED_HUMAN_NOTICE
                        {
                            agent.notice = BLOCKED_HUMAN_NOTICE.into();
                            agent.updated = self.clock.now_rfc3339();
                            self.save_registry(&registry)?;
                            let newly_announced = self
                                .blocked_announced
                                .lock()
                                .unwrap()
                                .insert(agent_id.to_string());
                            if newly_announced {
                                let _ = self.terminals.notify(
                                    &format!("Shepherd: agente {} requiere intervención", agent_id),
                                    Some(BLOCKED_HUMAN_NOTICE),
                                );
                            }
                        }
                    }
                    _ => {
                        // Not ready (e.g. "starting", "unknown", "not_ready"):
                        // never set notice for "not ready", and clear if previously blocked.
                        blocked_since_ms = None;
                        if !agent.notice.is_empty() {
                            agent.notice.clear();
                            agent.updated = self.clock.now_rfc3339();
                            self.save_registry(&registry)?;
                        }
                    }
                },
                Err(err)
                    if is_missing_terminal_error(&err)
                        && now_ms.saturating_sub(*missing_since_ms.get_or_insert(now_ms))
                            < super::MISSING_GRACE_MS =>
                {
                    // Just created and not resolvable yet: keep polling instead of giving up.
                    blocked_since_ms = None;
                }
                Err(err) if is_missing_terminal_error(&err) => {
                    let err_msg =
                        "el panel desapareció antes de entregar el primer prompt".to_owned();
                    agent.state = "exited".into();
                    agent.notice.clear();
                    agent.error = err_msg.clone();
                    agent.updated = self.clock.now_rfc3339();
                    self.save_registry(&registry)?;
                    return Err(err_msg);
                }
                Err(_) => {
                    blocked_since_ms = None;
                }
            }

            if now_ms.saturating_sub(start_ms) >= ready_timeout_ms {
                let mut registry = self.load_registry()?;
                if let Some(agent) = registry.agents.iter_mut().find(|a| a.id == agent_id) {
                    agent.state = "failed".into();
                    agent.notice.clear();
                    agent.error = NEVER_READY_ERROR.into();
                    agent.updated = self.clock.now_rfc3339();
                    self.save_registry(&registry)?;
                }
                return Err(NEVER_READY_ERROR.to_owned());
            }

            self.clock.sleep(self.service_config.ready_poll_interval);
        };

        // Phase 2: Send prompt and verify reaction; resend up to `max_prompt_resends` (3) times.
        let total_sends = 1 + self.service_config.max_prompt_resends;
        let reaction_timeout_ms = self.service_config.reaction_timeout.as_millis() as u64;

        for attempt in 0..total_sends {
            let (prompt_target, status_target, is_raw_pane) = {
                let mut registry = self.load_registry()?;
                let Some(agent) = registry.agents.iter_mut().find(|a| a.id == agent_id) else {
                    return Err(format!("unknown agent {agent_id:?}"));
                };
                if agent.gone() {
                    agent.notice.clear();
                    self.save_registry(&registry)?;
                    return Err(format!(
                        "agent {agent_id} was stopped during prompt delivery"
                    ));
                }
                let shepherd_agent = self
                    .shepherd_config
                    .vendors
                    .get(&agent.vendor)
                    .map(|v| v.shepherd_agent.as_str())
                    .unwrap_or("claude");
                let is_raw = shepherd_agent.is_empty();
                let p_target = if is_raw && !agent.pane.is_empty() {
                    agent.pane.clone()
                } else {
                    agent.id.clone()
                };
                (p_target, status_query_target(agent), is_raw)
            };

            if !is_raw_pane {
                let wait_opts = crate::shepherd::PromptWaitOptions {
                    until: vec!["working".into(), "idle".into()],
                    timeout: Some(self.service_config.reaction_timeout),
                };
                match self
                    .terminals
                    .prompt(&prompt_target, prompt, Some(wait_opts))
                {
                    Ok(PromptOutcome::Delivered) => {
                        let mut registry = self.load_registry()?;
                        if let Some(agent) = registry.agents.iter_mut().find(|a| a.id == agent_id) {
                            if !agent.gone() && agent.state != "done" {
                                agent.state = "working".into();
                            }
                            agent.notice.clear();
                            agent.error.clear();
                            agent.updated = self.clock.now_rfc3339();
                            self.save_registry(&registry)?;
                        }
                        return Ok(());
                    }
                    Ok(PromptOutcome::Blocked) => {
                        self.wait_until_unblocked(
                            agent_id,
                            start_ms,
                            ready_timeout_ms,
                            notice_after_ms,
                        )?;
                        continue;
                    }
                    Err(err) => {
                        if attempt + 1 == total_sends {
                            let msg = format!(
                                "el agente {agent_id} no reaccionó al primer prompt tras {} reenvíos: {err}",
                                self.service_config.max_prompt_resends
                            );
                            self.mark_prompt_failed(agent_id, &msg)?;
                            return Err(format!(
                                "el agente {agent_id} no reaccionó al primer prompt tras {} reenvíos",
                                self.service_config.max_prompt_resends
                            ));
                        }
                        self.clock.sleep(self.service_config.ready_poll_interval);
                        continue;
                    }
                }
            } else {
                let send_outcome = self
                    .terminals
                    .send_text(&prompt_target, prompt)
                    .map(|()| PromptOutcome::Delivered);

                match send_outcome {
                    Ok(PromptOutcome::Blocked) => {
                        self.wait_until_unblocked(
                            agent_id,
                            start_ms,
                            ready_timeout_ms,
                            notice_after_ms,
                        )?;
                        continue;
                    }
                    Ok(PromptOutcome::Delivered) => {}
                    Err(err) => {
                        if attempt + 1 == total_sends {
                            self.mark_prompt_failed(
                                agent_id,
                                &format!(
                                    "no se pudo enviar el primer prompt tras {} reenvíos: {err}",
                                    self.service_config.max_prompt_resends
                                ),
                            )?;
                            return Err(format!("no se pudo enviar el primer prompt: {err}"));
                        }
                    }
                }

                // Poll for reaction (status == "working" or seq > baseline_seq).
                let check_start_ms = self.clock.now_ms();
                loop {
                    if let Ok(st) = self.terminals.agent_status(&status_target) {
                        if st.state != "finished"
                            && (st.status == "working" || st.seq > baseline_seq)
                        {
                            let mut registry = self.load_registry()?;
                            if let Some(agent) =
                                registry.agents.iter_mut().find(|a| a.id == agent_id)
                            {
                                if !agent.gone() && agent.state != "done" {
                                    agent.state = "working".into();
                                }
                                agent.notice.clear();
                                agent.error.clear();
                                agent.updated = self.clock.now_rfc3339();
                                self.save_registry(&registry)?;
                            }
                            return Ok(());
                        }
                    }
                    if self.clock.now_ms().saturating_sub(check_start_ms) >= reaction_timeout_ms {
                        break;
                    }
                    self.clock.sleep(self.service_config.reaction_poll_interval);
                }
            }
        }

        let msg = format!(
            "el agente {agent_id} no reaccionó al primer prompt tras {} reenvíos",
            self.service_config.max_prompt_resends
        );
        self.mark_prompt_failed(agent_id, &msg)?;
        Err(msg)
    }

    fn wait_until_unblocked(
        &self,
        agent_id: &str,
        start_ms: u64,
        ready_timeout_ms: u64,
        notice_after_ms: u64,
    ) -> Result<(), String> {
        let blocked_since = self.clock.now_ms();
        loop {
            let mut registry = self.load_registry()?;
            let Some(agent) = registry.agents.iter_mut().find(|a| a.id == agent_id) else {
                return Err(format!("unknown agent {agent_id:?}"));
            };
            if agent.gone() {
                let state = agent.state.clone();
                agent.notice.clear();
                self.save_registry(&registry)?;
                return Err(format!("agent {agent_id} ended in state {state}"));
            }
            let target = status_query_target(agent);
            let now_ms = self.clock.now_ms();
            let mut dirty = false;
            if now_ms.saturating_sub(blocked_since) >= notice_after_ms
                && agent.notice != BLOCKED_HUMAN_NOTICE
            {
                agent.notice = BLOCKED_HUMAN_NOTICE.into();
                agent.updated = self.clock.now_rfc3339();
                dirty = true;
                let newly_announced = self
                    .blocked_announced
                    .lock()
                    .unwrap()
                    .insert(agent_id.to_string());
                if newly_announced {
                    let _ = self.terminals.notify(
                        &format!("Shepherd: agente {} requiere intervención", agent_id),
                        Some(BLOCKED_HUMAN_NOTICE),
                    );
                }
            }
            if let Ok(st) = self.terminals.agent_status(&target) {
                if matches!(st.status.as_str(), "idle" | "done" | "working") {
                    if !agent.notice.is_empty() {
                        agent.notice.clear();
                        dirty = true;
                    }
                    if dirty {
                        self.save_registry(&registry)?;
                    }
                    return Ok(());
                }
            }
            if now_ms.saturating_sub(start_ms) >= ready_timeout_ms {
                agent.state = "failed".into();
                agent.notice.clear();
                agent.error = NEVER_READY_ERROR.into();
                agent.updated = self.clock.now_rfc3339();
                self.save_registry(&registry)?;
                return Err(NEVER_READY_ERROR.to_owned());
            }
            if dirty {
                self.save_registry(&registry)?;
            }
            self.clock.sleep(self.service_config.ready_poll_interval);
        }
    }

    fn mark_prompt_failed(&self, agent_id: &str, error_msg: &str) -> Result<(), String> {
        let mut registry = self.load_registry()?;
        if let Some(agent) = registry.agents.iter_mut().find(|a| a.id == agent_id) {
            agent.state = "failed".into();
            agent.notice.clear();
            agent.error = error_msg.to_owned();
            agent.updated = self.clock.now_rfc3339();
            self.save_registry(&registry)?;
        }
        Ok(())
    }

    /// An idle MCP agent only reads the bus when it calls `read_messages`, which an idle agent never
    /// does. Type one short line telling it to look, once per new message.
    pub(crate) fn nudge_idle_mcp_agents(&self, registry: &AgentRegistryFile) {
        let Ok(bus) = self.bus() else { return };
        for agent in &registry.agents {
            if !agent.mcp || agent.gone() || agent.state != "idle" {
                continue;
            }
            let Ok(pending) = bus.peek_pending(&agent.id) else {
                continue;
            };
            let Some(newest) = pending.iter().map(|m| m.id).max() else {
                continue;
            };
            if self
                .nudged
                .lock()
                .unwrap()
                .get(&agent.id)
                .is_some_and(|&seen| seen >= newest)
            {
                continue;
            }
            if !self.is_pane_free_for_delivery(agent) {
                continue;
            }
            let target = if agent.pane.is_empty() {
                &agent.id
            } else {
                &agent.pane
            };
            let line = format!(
                "[shep] You have {} unread message(s). Call read_messages() now and act on them.",
                pending.len()
            );
            if matches!(
                self.terminals.prompt(target, &line, None),
                Ok(PromptOutcome::Delivered)
            ) {
                self.nudged.lock().unwrap().insert(agent.id.clone(), newest);
            }
        }
    }

    pub(crate) fn deliver_pending_non_mcp_messages(&self, registry: &mut AgentRegistryFile) {
        let bus = match self.bus() {
            Ok(b) => b,
            Err(e) => {
                self.record_failure(
                    "deliver_bus_messages",
                    &format!("cannot open message bus: {e}"),
                );
                return;
            }
        };

        let mut registry_dirty = false;
        let reg_snapshot = registry.clone();

        let paused_agent_ids: std::collections::HashSet<String> = {
            let runs_file = crate::service::patterns_run::pattern_runs_path(
                &self.service_config.state_dir,
                &self.service_config.state_dir,
            );
            crate::service::patterns_run::load_pattern_runs(&runs_file)
                .unwrap_or_default()
                .into_iter()
                .filter(|r| r.state == crate::service::patterns_run::PatternRunState::Paused)
                .flat_map(|r| r.agents.into_iter().map(|a| a.id))
                .collect()
        };

        for agent in &mut registry.agents {
            // Only live non-MCP agents (not mcp, not gone, not done, not starting, and not paused)
            if agent.mcp
                || agent.gone()
                || agent.state == "done"
                || agent.state == "starting"
                || paused_agent_ids.contains(&agent.id)
            {
                continue;
            }

            let messages = match bus.since(agent.delivery_cursor) {
                Ok(all) => {
                    let mut msgs: Vec<Message> = all
                        .into_iter()
                        .filter(|m| m.to == agent.id && m.kind != KIND_SYSTEM)
                        .collect();
                    msgs.sort_by_key(|m| m.id);
                    msgs
                }
                Err(e) => {
                    self.record_failure(
                        "deliver_bus_messages",
                        &format!(
                            "cannot read bus since {} for agent {}: {e}",
                            agent.delivery_cursor, agent.id
                        ),
                    );
                    Vec::new()
                }
            };

            let target = if !agent.pane.is_empty() {
                agent.pane.clone()
            } else {
                agent.id.clone()
            };

            // 1. Deliver directed messages (if pane is free)
            if !messages.is_empty() && self.is_pane_free_for_delivery(agent) {
                for msg in messages {
                    if !self.is_pane_free_for_delivery(agent) {
                        break;
                    }

                    let line = format_shep_message(&msg.from, &msg.content, &reg_snapshot);
                    let delivered =
                        self.deliver_single_message_with_retries(agent, &target, msg.id, &line);
                    if delivered {
                        agent.delivery_cursor = msg.id;
                        agent.updated = self.clock.now_rfc3339();
                        registry_dirty = true;
                    } else {
                        break;
                    }
                }
            }

            // 2. Deliver subscribed topic events for live non-MCP agents
            let groups = bus.consumer_groups(None).unwrap_or_default();
            let live_ids: Vec<String> = reg_snapshot
                .live()
                .into_iter()
                .map(|a| a.id.clone())
                .collect();
            for grp_info in groups {
                if grp_info.members.contains(&agent.id) {
                    if !self.is_pane_free_for_delivery(agent) {
                        break;
                    }
                    if let Ok(topic_msgs) =
                        bus.poll_active(&agent.id, &grp_info.topic, Some(10), Some(&live_ids))
                    {
                        for t_msg in topic_msgs {
                            if !self.is_pane_free_for_delivery(agent) {
                                break;
                            }
                            let line = format_shep_topic_message(&t_msg, &reg_snapshot);
                            let delivered = self.deliver_single_message_with_retries(
                                agent, &target, t_msg.id, &line,
                            );
                            if delivered {
                                let _ = bus.ack(
                                    &agent.id,
                                    &t_msg.topic,
                                    Some(t_msg.partition),
                                    Some(t_msg.offset),
                                );
                            } else {
                                break;
                            }
                        }
                    }
                }
            }
        }

        if registry_dirty {
            if let Err(e) = self.save_registry(registry) {
                self.record_failure(
                    "deliver_bus_messages",
                    &format!("cannot save registry after message delivery: {e}"),
                );
            }
        }
    }

    /// Takes agents that are gone (or no longer in the registry) out of the consumer groups, so the
    /// topic partitions are only shared among agents that can read them.
    pub(crate) fn prune_dead_subscribers(&self, registry: &AgentRegistryFile) {
        let Ok(bus) = self.bus() else { return };
        let Ok(groups) = bus.consumer_groups(None) else {
            return;
        };
        for grp in groups {
            for member in &grp.members {
                let dead = registry
                    .agents
                    .iter()
                    .find(|a| &a.id == member)
                    .is_none_or(|a| a.gone());
                if dead {
                    let _ = bus.unsubscribe(member, &grp.topic);
                }
            }
        }
    }

    pub(crate) fn wake_dormant_topic_subscribers(
        &self,
        registry: &mut AgentRegistryFile,
        topic_filter: Option<&str>,
    ) {
        let bus = match self.bus() {
            Ok(b) => b,
            Err(e) => {
                self.record_failure(
                    "wake_dormant_topic_subscribers",
                    &format!("cannot open message bus: {e}"),
                );
                return;
            }
        };

        // Determine agents belonging to paused, stopped, failed, or finished pattern runs (must not wake them)
        let paused_or_inactive_agent_ids: std::collections::HashSet<String> = {
            let mut paths = vec![crate::service::patterns_run::pattern_runs_path(
                &self.service_config.state_dir,
                &self.service_config.state_dir,
            )];
            for ag in &registry.agents {
                if !ag.root.is_empty() {
                    paths.push(crate::service::patterns_run::pattern_runs_path(
                        std::path::Path::new(&ag.root),
                        &self.service_config.state_dir,
                    ));
                }
            }
            paths.sort();
            paths.dedup();
            paths
                .into_iter()
                .flat_map(|p| {
                    crate::service::patterns_run::load_pattern_runs(&p)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|r| {
                            matches!(
                                r.state,
                                crate::service::patterns_run::PatternRunState::Paused
                                    | crate::service::patterns_run::PatternRunState::Stopped
                                    | crate::service::patterns_run::PatternRunState::Failed
                                    | crate::service::patterns_run::PatternRunState::Done
                            ) && !r.quiesced
                        })
                        .flat_map(|r| r.agents.into_iter().map(|a| a.id))
                })
                .collect()
        };

        let groups = match bus.consumer_groups(topic_filter) {
            Ok(g) => g,
            Err(_) => return,
        };

        let now_ms = self.clock.now_ms();
        let mut registry_dirty = false;
        // Partitions are shared out among the members that are alive: a member left over from an
        // earlier run must not keep the messages of its partitions away from the live ones.
        let live_ids: Vec<String> = registry
            .agents
            .iter()
            .filter(|a| !a.gone()) // a `done` agent can still be woken by a new message
            .map(|a| a.id.clone())
            .collect();

        for grp in groups {
            for member_id in &grp.members {
                if paused_or_inactive_agent_ids.contains(member_id) {
                    continue;
                }

                // Check agent status in registry
                let Some(agent) = registry.agents.iter_mut().find(|a| &a.id == member_id) else {
                    continue;
                };

                // Dead, exited, failed, killed agents must not be woken
                if agent.gone()
                    || agent.state == "failed"
                    || agent.state == "exited"
                    || agent.state == "killed"
                {
                    continue;
                }

                // If agent is actively working, do not interfere (e.g. active wait_for or generating)
                if agent.state == "working" {
                    continue;
                }

                // Never answer human intervention dialogs
                if agent.notice == BLOCKED_HUMAN_NOTICE {
                    continue;
                }

                // Query live terminal status: if working or finished, do not wake
                let target = status_query_target(agent);
                if !target.is_empty() {
                    if let Ok(st) = self.terminals.agent_status(&target) {
                        if st.state == "finished" || st.status == "working" {
                            continue;
                        }
                    }
                }

                // Check unread messages on the topic
                // Only unacknowledged messages from other publishers count (not its own).
                let unread: Vec<_> =
                    match bus.poll_active(member_id, &grp.topic, None, Some(&live_ids)) {
                        Ok(msgs) => msgs.into_iter().filter(|m| &m.from != member_id).collect(),
                        Err(_) => continue,
                    };
                if unread.is_empty() {
                    continue;
                }

                let count = unread.len();
                let max_offset = unread.iter().map(|m| m.offset).max().unwrap_or(0);

                let key = (member_id.clone(), grp.topic.clone());
                {
                    let wake_map = self.topic_wake_notified.lock().unwrap();
                    if let Some(&(last_offset, last_time_ms)) = wake_map.get(&key) {
                        if max_offset <= last_offset {
                            // Idempotent: already notified for this or newer offset
                            continue;
                        }
                        if now_ms.saturating_sub(last_time_ms) < 3000 {
                            // Rate limit: at most 1 alert every 3 seconds
                            continue;
                        }
                    }
                }

                let prompt_text = if count == 1 {
                    format!("Hay 1 mensaje nuevo en '{}': usa poll", grp.topic)
                } else {
                    format!("Hay {} mensajes nuevos en '{}': usa poll", count, grp.topic)
                };

                let prompt_target = if !agent.pane.is_empty() {
                    agent.pane.clone()
                } else {
                    agent.id.clone()
                };

                let delivered = self.deliver_single_message_with_retries(
                    agent,
                    &prompt_target,
                    0,
                    &prompt_text,
                );
                if delivered {
                    self.topic_wake_notified
                        .lock()
                        .unwrap()
                        .insert(key, (max_offset, now_ms));
                    if agent.state == "done" || agent.state == "idle" {
                        agent.state = "working".into();
                    }
                    agent.updated = self.clock.now_rfc3339();
                    registry_dirty = true;
                }
            }
        }

        if registry_dirty {
            if let Err(e) = self.save_registry(registry) {
                self.record_failure(
                    "wake_dormant_topic_subscribers",
                    &format!("cannot save registry after waking dormant subscribers: {e}"),
                );
            }
        }
    }

    fn is_pane_free_for_delivery(&self, agent: &Agent) -> bool {
        if !agent.notice.is_empty() || agent.state == "blocked" {
            return false;
        }

        let st = if !agent.id.is_empty() {
            if let Ok(st) = self.terminals.agent_status(&agent.id) {
                if st.status != "unknown" {
                    Some(st)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        let status = match st {
            Some(s) => s,
            None => {
                let target = if !agent.pane.is_empty() {
                    &agent.pane
                } else {
                    &agent.id
                };
                match self.terminals.agent_status(target) {
                    Ok(s) => s,
                    Err(_) => return false,
                }
            }
        };

        if status.state == "finished" || status.status != "idle" {
            return false;
        }

        let s1 = self.read_screen_best_effort(agent);
        self.clock.sleep(Duration::from_millis(50));
        let s2 = self.read_screen_best_effort(agent);

        s1 == s2
    }

    fn read_screen_best_effort(&self, agent: &Agent) -> String {
        let targets = [&agent.pane, &agent.id];
        for t in targets {
            if !t.is_empty() {
                if let Ok(screen) = self.terminals.read_screen(t, 200) {
                    if !screen.is_empty() {
                        return screen;
                    }
                }
            }
        }
        for t in targets {
            if !t.is_empty() {
                if let Ok(screen) = self.terminals.read_screen(t, 200) {
                    return screen;
                }
            }
        }
        String::new()
    }

    fn deliver_single_message_with_retries(
        &self,
        agent: &Agent,
        target: &str,
        msg_id: i64,
        line: &str,
    ) -> bool {
        // F7 Item 1: Deliver via `agent.prompt` without typing with send_text or scraping screen.
        // The host runtime handles vendor-specific input encoding, Enter keystroke delays, focus, and paste boundaries.
        let prompt_result = self.terminals.prompt(target, line, None);
        match prompt_result {
            Ok(PromptOutcome::Delivered) => true,
            Ok(PromptOutcome::Blocked) => {
                // Agent is blocked; cannot receive prompt now, do not advance cursor.
                false
            }
            Err(crate::terminals::TerminalError::Api { ref code, .. })
                if code == "agent_not_ready"
                    || code == "agent_not_found"
                    || code == "not_found" =>
            {
                // Fallback: The pane has no agent detected by runtime (e.g. raw shell).
                // Falls back to typing with `send_text` and screen verification.
                self.deliver_message_via_raw_pane_fallback(agent, target, msg_id, line)
            }
            Err(crate::terminals::TerminalError::Protocol(ref msg))
                if msg.contains("agent did not become ready") =>
            {
                // Fallback: prompt timed out waiting for agent to become ready (raw shell).
                self.deliver_message_via_raw_pane_fallback(agent, target, msg_id, line)
            }
            Err(err) => {
                self.record_failure(
                    "deliver_bus_messages",
                    &format!(
                        "agent.prompt for agent {} in {target} (msg {msg_id}) failed: {err}",
                        agent.id
                    ),
                );
                false
            }
        }
    }

    fn deliver_message_via_raw_pane_fallback(
        &self,
        agent: &Agent,
        target: &str,
        msg_id: i64,
        line: &str,
    ) -> bool {
        let max_retries = self.service_config.max_prompt_resends;
        let total_attempts = 1 + max_retries;
        let reaction_timeout_ms = self.service_config.reaction_timeout.as_millis() as u64;
        let poll_interval = self.service_config.reaction_poll_interval;

        for attempt in 0..total_attempts {
            if let Err(e) = self.terminals.send_text(target, line) {
                if attempt + 1 == total_attempts {
                    self.record_failure(
                        "deliver_bus_messages",
                        &format!(
                            "cannot send text to {target} for message {msg_id} after {max_retries} retries: {e}"
                        ),
                    );
                }
                continue;
            }

            let check_start_ms = self.clock.now_ms();
            let mut verified = false;
            let normalized_line: String = line.split_whitespace().collect::<Vec<_>>().join(" ");

            loop {
                let screen = self.read_screen_best_effort(agent);
                if screen.contains(line) {
                    verified = true;
                    break;
                }
                let normalized_screen: String =
                    screen.split_whitespace().collect::<Vec<_>>().join(" ");
                if normalized_screen.contains(&normalized_line) {
                    verified = true;
                    break;
                }

                if self.clock.now_ms().saturating_sub(check_start_ms) >= reaction_timeout_ms {
                    break;
                }
                self.clock.sleep(poll_interval);
            }

            if verified {
                return true;
            }
        }

        self.record_failure(
            "deliver_bus_messages",
            &format!(
                "message {msg_id} delivered to agent {} in pane {target} was not verified on screen after {max_retries} retries",
                agent.id
            ),
        );
        false
    }
}
