//! Shared agent-inbox health snapshot, collected by the fleet poller.

use serde::Deserialize;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct Healthcheck {
    pub(crate) inbox_enabled: bool,
    pub(crate) timer_state: String,
    pub(crate) surface_reachable: bool,
    #[serde(default)]
    pub(crate) sources: Vec<Source>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct Source {
    pub(crate) name: String,
    pub(crate) enabled: bool,
    pub(crate) last_intake_time: Option<String>,
    pub(crate) cursor_age_s: Option<u64>,
    #[serde(default)]
    pub(crate) undelivered: u64,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProducerState {
    Unconfigured,
    Unreachable(String),
    Read,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProducerSnapshot {
    pub(crate) host: String,
    pub(crate) state: ProducerState,
    pub(crate) data: Healthcheck,
    pub(crate) refreshed_at: Option<std::time::SystemTime>,
}

impl ProducerSnapshot {
    pub(crate) fn read(
        host: String,
        data: Healthcheck,
        refreshed_at: std::time::SystemTime,
    ) -> Self {
        Self {
            host,
            state: ProducerState::Read,
            data,
            refreshed_at: Some(refreshed_at),
        }
    }

    pub(crate) fn unreachable(host: String, error: String) -> Self {
        Self {
            host,
            state: ProducerState::Unreachable(error),
            data: Healthcheck::default(),
            refreshed_at: None,
        }
    }

    pub(crate) fn unconfigured(host: String) -> Self {
        Self {
            host,
            state: ProducerState::Unconfigured,
            data: Healthcheck::default(),
            refreshed_at: None,
        }
    }
}

pub(crate) fn parse_healthcheck(bytes: &[u8]) -> Result<Healthcheck, String> {
    serde_json::from_slice(bytes).map_err(|error| format!("invalid inbox healthcheck: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agent_inbox_healthcheck_contract() {
        let parsed = parse_healthcheck(br#"{"inbox_enabled":true,"timer_state":"active","surface_reachable":true,"sources":[{"name":"box","enabled":true,"last_intake_time":"2026-10-01T06:00:00Z","cursor_age_s":42,"undelivered":3,"error":null}]}"#).expect("contract JSON parses");
        assert!(parsed.inbox_enabled);
        assert!(parsed.surface_reachable);
        assert_eq!(parsed.timer_state, "active");
        assert_eq!(parsed.sources[0].name, "box");
        assert_eq!(parsed.sources[0].cursor_age_s, Some(42));
        assert_eq!(parsed.sources[0].undelivered, 3);
    }
}
