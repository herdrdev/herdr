use std::fmt;

use serde::{Deserialize, Serialize};

/// Opaque identity for one managed agent lifetime.
///
/// Unlike pane, terminal, process, or provider-conversation identifiers, this
/// value names the Herdr-managed agent instance itself. It survives movement
/// and live handoff, and it is never reused by a replacement agent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentInstanceId(uuid::Uuid);

impl AgentInstanceId {
    pub fn alloc() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    pub fn parse(value: &str) -> Option<Self> {
        uuid::Uuid::parse_str(value).ok().map(Self)
    }
}

impl fmt::Display for AgentInstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn allocated_ids_are_canonical_uuid_v4_values() {
        let first = AgentInstanceId::alloc();
        let second = AgentInstanceId::alloc();

        assert_ne!(first, second);
        assert_eq!(AgentInstanceId::parse(&first.to_string()), Some(first));
    }

    #[test]
    fn one_thousand_allocations_are_unique() {
        let ids = (0..1_000)
            .map(|_| AgentInstanceId::alloc().to_string())
            .collect::<HashSet<_>>();

        assert_eq!(ids.len(), 1_000);
    }
}
