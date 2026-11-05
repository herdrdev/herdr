//! Startup resume and periodic reconciliation of agent states against live runtime.

use super::{
    is_missing_terminal_error, status_query_target, OrchestrationService, ScreenQuietTracker,
    BLOCKED_HUMAN_NOTICE,
};
use crate::bus::{Message, KIND_SYSTEM};
use crate::{Agent, AgentRegistryFile};

impl OrchestrationService {
    /// Requirement 4: Reconcile the persisted registry with the live runtime on startup (`resume`).
    /// - `starting` with missing pane -> `exited`
    /// - `starting` that is already working -> `working`
    /// - `starting` that never received its first prompt -> `failed` with relaunch instructions
    /// - Clear `notice` on all agents
    /// - Preserve `token`s (`los tokens sobreviven`)
    pub fn resume_on_startup(&self) -> Result<AgentRegistryFile, String> {
        let result = (|| {
            let mut registry = self.load_registry()?;
            let now = self.clock.now_rfc3339();
            for agent in &mut registry.agents {
                agent.notice.clear();
                if agent.gone() {
                    continue;
                }
                let target = status_query_target(agent);
                let live_status = if target.is_empty() {
                    None
                } else {
                    match self.terminals.agent_status(&target) {
                        Ok(st) if st.state != "finished" => Some(st),
                        Ok(_) => None,
                        Err(err) if is_missing_terminal_error(&err) => None,
                        Err(err) => return Err(err.to_string()),
                    }
                };

                if agent.state == "starting" {
                    match live_status {
                        None => {
                            agent.state = "exited".into();
                            agent.updated = now.clone();
                        }
                        Some(st) if st.status == "working" => {
                            agent.state = "working".into();
                            agent.updated = now.clone();
                        }
                        Some(_) => {
                            agent.state = "failed".into();
                            agent.error = format!(
                                "no recibió su primer prompt antes del reinicio; relánzalo con `shepherd shepherd spawn {} --vendor {}`",
                                agent.role, agent.vendor
                            );
                            agent.updated = now.clone();
                        }
                    }
                } else {
                    match live_status {
                        None => {
                            agent.state = "exited".into();
                            agent.updated = now.clone();
                        }
                        Some(st) => match st.status.as_str() {
                            "working" => {
                                agent.state = "working".into();
                                agent.updated = now.clone();
                            }
                            "blocked" => {
                                agent.state = "blocked".into();
                                agent.updated = now.clone();
                            }
                            "idle" | "done" if agent.state != "done" => {
                                agent.state = "idle".into();
                                agent.updated = now.clone();
                            }
                            "idle" | "done" => {}
                            _ => {}
                        },
                    }
                }
            }
            self.save_registry(&registry)?;
            self.auto_snapshot_tick(&registry);
            self.recover_snapshots(&registry);
            let _ = self.reconcile_pattern_runs(&registry, None);
            Ok(registry)
        })();
        self.log_err("resume", result)
    }

    /// Requirement 3: Periodic reconciliation against the live runtime.
    /// - Never touches `starting` or definitive states (`exited, killed, failed`).
    /// - Missing pane -> `exited`.
    /// - `working` -> `working`.
    /// - `blocked` -> `blocked` (announces ONCE per blocked episode in the bus timeline).
    /// - `idle`/`done` -> `idle` if agent has MCP, or `done` if non-MCP AND screen has been
    ///   unchanged for `>= non_mcp_done_quiet_secs` (capturing final screen as `result` if
    ///   `!agent.has_result`).
    pub fn reconcile_once(&self) -> Result<AgentRegistryFile, String> {
        let result = (|| {
            let mut registry = self.load_registry()?;
            let mut changed = false;
            let now_ms = self.clock.now_ms();
            let now_str = self.clock.now_rfc3339();
            let quiet_ms = self
                .service_config
                .non_mcp_done_quiet_secs
                .saturating_mul(1000);
            let bus = self.bus()?;

            for agent in &mut registry.agents {
                if agent.state == "starting" || agent.gone() {
                    continue;
                }
                let target = status_query_target(agent);
                let status = match self.terminals.agent_status(&target) {
                    Ok(st) if st.state != "finished" => st,
                    Ok(_) => {
                        agent.state = "exited".into();
                        agent.notice.clear();
                        agent.updated = now_str.clone();
                        self.blocked_announced.lock().unwrap().remove(&agent.id);
                        self.quiet_screens.lock().unwrap().remove(&agent.id);
                        changed = true;
                        continue;
                    }
                    Err(err) if is_missing_terminal_error(&err) => {
                        agent.state = "exited".into();
                        agent.notice.clear();
                        agent.updated = now_str.clone();
                        self.blocked_announced.lock().unwrap().remove(&agent.id);
                        self.quiet_screens.lock().unwrap().remove(&agent.id);
                        changed = true;
                        continue;
                    }
                    Err(err) => return Err(err.to_string()),
                };

                match status.status.as_str() {
                    "working" => {
                        self.blocked_announced.lock().unwrap().remove(&agent.id);
                        self.quiet_screens.lock().unwrap().remove(&agent.id);
                        if agent.state != "working" || !agent.notice.is_empty() {
                            agent.state = "working".into();
                            agent.notice.clear();
                            agent.updated = now_str.clone();
                            changed = true;
                        }
                    }
                    "blocked" => {
                        self.quiet_screens.lock().unwrap().remove(&agent.id);
                        if agent.state != "blocked" || agent.notice != BLOCKED_HUMAN_NOTICE {
                            agent.state = "blocked".into();
                            agent.notice = BLOCKED_HUMAN_NOTICE.into();
                            agent.updated = now_str.clone();
                            changed = true;
                        }
                        let newly_blocked = self
                            .blocked_announced
                            .lock()
                            .unwrap()
                            .insert(agent.id.clone());
                        if newly_blocked {
                            let _ = self.terminals.notify(
                                &format!("Shepherd: agente {} requiere intervención", agent.id),
                                Some(BLOCKED_HUMAN_NOTICE),
                            );
                            let _ = bus.send(Message::directed(
                                "system",
                                "brain",
                                KIND_SYSTEM,
                                format!(
                                    "agent {} ({}) is blocked waiting for human attention in pane {}",
                                    agent.id, agent.role, agent.pane
                                ),
                            ));
                        }
                    }
                    "idle" | "done" => {
                        self.blocked_announced.lock().unwrap().remove(&agent.id);
                        if !agent.notice.is_empty() {
                            agent.notice.clear();
                            changed = true;
                        }
                        if agent.state == "blocked" {
                            agent.state = if agent.mcp {
                                "idle".into()
                            } else {
                                "working".into()
                            };
                            agent.updated = now_str.clone();
                            changed = true;
                        }
                        if agent.mcp {
                            if agent.state != "done" && agent.state != "idle" {
                                agent.state = "idle".into();
                                agent.updated = now_str.clone();
                                changed = true;
                            }
                        } else if agent.state != "done" {
                            let read_target = if !agent.pane.is_empty() {
                                agent.pane.as_str()
                            } else {
                                agent.id.as_str()
                            };
                            let screen = self
                                .terminals
                                .read_screen(read_target, 200)
                                .unwrap_or_default();
                            let is_quiet_enough = {
                                let mut trackers = self.quiet_screens.lock().unwrap();
                                let entry = trackers.entry(agent.id.clone()).or_insert_with(|| {
                                    ScreenQuietTracker {
                                        last_screen: screen.clone(),
                                        unchanged_since_ms: now_ms,
                                    }
                                });
                                if entry.last_screen != screen {
                                    entry.last_screen = screen.clone();
                                    entry.unchanged_since_ms = now_ms;
                                }
                                now_ms.saturating_sub(entry.unchanged_since_ms) >= quiet_ms
                            };
                            if is_quiet_enough {
                                agent.state = "done".into();
                                if !agent.has_result && agent.result.trim().is_empty() {
                                    let trimmed = screen.trim().to_owned();
                                    agent.result = trimmed;
                                    agent.has_result = !agent.result.is_empty();
                                }
                                agent.updated = now_str.clone();
                                self.quiet_screens.lock().unwrap().remove(&agent.id);
                                changed = true;
                            }
                        }
                    }
                    _ => {}
                }
            }

            if changed {
                self.save_registry(&registry)?;
            }

            self.auto_snapshot_tick(&registry);
            self.deliver_pending_non_mcp_messages(&mut registry);
            self.wake_dormant_topic_subscribers(&mut registry, None);
            let _ = self.reconcile_pattern_runs(&registry, None);

            Ok(registry)
        })();
        self.log_err("reconcile", result)
    }

    pub fn reconcile_agents(&self) -> Result<Vec<Agent>, String> {
        let reg = self.reconcile_once()?;
        Ok(reg.agents.iter().map(Agent::public).collect())
    }

    pub fn resume_agents(&self) -> Result<Vec<Agent>, String> {
        let reg = self.resume_on_startup()?;
        Ok(reg.agents.iter().map(Agent::public).collect())
    }
}
