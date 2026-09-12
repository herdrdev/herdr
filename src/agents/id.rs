//! Stable canonical agent identity, independent of registry membership.

use std::fmt;

/// A validated canonical ID (`[a-z][a-z0-9-]{0,63}`), stored inline.
///
/// Parsing an ID does not establish registry membership or grant capabilities.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AgentId {
    bytes: [u8; 64],
    len: u8,
}

impl AgentId {
    pub fn parse(value: &str) -> Result<Self, String> {
        if !Self::valid(value) {
            return Err("agent ID must match [a-z][a-z0-9-]{0,63}".to_string());
        }
        Ok(Self::from_canonical(value))
    }

    pub fn as_str(&self) -> &str {
        // Both constructors validate ASCII and zero-fill unused bytes.
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).expect("validated ASCII agent ID")
    }

    const fn valid(value: &str) -> bool {
        let bytes = value.as_bytes();
        if bytes.is_empty() || bytes.len() > 64 || !bytes[0].is_ascii_lowercase() {
            return false;
        }
        let mut index = 1;
        while index < bytes.len() {
            let byte = bytes[index];
            if !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && byte != b'-' {
                return false;
            }
            index += 1;
        }
        true
    }

    const fn from_canonical(value: &str) -> Self {
        assert!(Self::valid(value), "invalid canonical agent ID");
        let mut bytes = [0; 64];
        let mut index = 0;
        while index < value.len() {
            bytes[index] = value.as_bytes()[index];
            index += 1;
        }
        Self {
            bytes,
            len: value.len() as u8,
        }
    }
}

// Compatibility spellings, not a closed set of supported identities.
#[allow(non_upper_case_globals)]
impl AgentId {
    pub const Pi: Self = Self::from_canonical("pi");
    pub const Claude: Self = Self::from_canonical("claude");
    pub const Codex: Self = Self::from_canonical("codex");
    pub const Gemini: Self = Self::from_canonical("gemini");
    pub const Cursor: Self = Self::from_canonical("cursor");
    pub const Devin: Self = Self::from_canonical("devin");
    pub const Antigravity: Self = Self::from_canonical("agy");
    pub const Cline: Self = Self::from_canonical("cline");
    pub const Omp: Self = Self::from_canonical("omp");
    pub const Mastracode: Self = Self::from_canonical("mastracode");
    pub const OpenCode: Self = Self::from_canonical("opencode");
    pub const GithubCopilot: Self = Self::from_canonical("copilot");
    pub const Kimi: Self = Self::from_canonical("kimi");
    pub const Kiro: Self = Self::from_canonical("kiro");
    pub const Droid: Self = Self::from_canonical("droid");
    pub const Amp: Self = Self::from_canonical("amp");
    pub const Grok: Self = Self::from_canonical("grok");
    pub const Hermes: Self = Self::from_canonical("hermes");
    pub const Kilo: Self = Self::from_canonical("kilo");
    pub const Qodercli: Self = Self::from_canonical("qodercli");
    pub const Qwen: Self = Self::from_canonical("qwen");
    pub const Maki: Self = Self::from_canonical("maki");
    pub const Muse: Self = Self::from_canonical("muse");

    /// Legacy built-in identities, for compatibility and tests only.
    /// Never use this list to decide whether an ID is valid or registered.
    pub const ALL: [Self; 23] = [
        Self::Pi,
        Self::Claude,
        Self::Codex,
        Self::Gemini,
        Self::Cursor,
        Self::Devin,
        Self::Antigravity,
        Self::Cline,
        Self::Omp,
        Self::Mastracode,
        Self::OpenCode,
        Self::GithubCopilot,
        Self::Kimi,
        Self::Kiro,
        Self::Droid,
        Self::Amp,
        Self::Grok,
        Self::Hermes,
        Self::Kilo,
        Self::Qodercli,
        Self::Qwen,
        Self::Maki,
        Self::Muse,
    ];
}

impl fmt::Debug for AgentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("AgentId")
            .field(&self.as_str())
            .finish()
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::AgentId;
    use std::collections::{BTreeSet, HashMap};

    #[test]
    fn validates_canonical_syntax_and_bounds_without_truncating() {
        for invalid in [
            "", "Pi", " pi", "pi ", "1agent", "-agent", "a_b", "a.b", "a/b", "a\0b", "é", "aé",
            "a\nb",
        ] {
            assert!(AgentId::parse(invalid).is_err(), "{invalid:?}");
        }
        assert_eq!(AgentId::parse("a").unwrap().as_str(), "a");
        assert_eq!(AgentId::parse("a-0").unwrap().as_str(), "a-0");
        let maximum = "a".repeat(64);
        assert_eq!(AgentId::parse(&maximum).unwrap().as_str(), maximum);
        assert!(AgentId::parse(&"a".repeat(65)).is_err());
    }

    #[test]
    fn identity_is_copyable_and_has_value_equality_hash_and_order() {
        assert_eq!(std::mem::size_of::<AgentId>(), 65);
        let original = AgentId::parse("future-agent").unwrap();
        let copy = original;
        assert_eq!(original, copy);
        assert_eq!(original, AgentId::parse("future-agent").unwrap());
        assert_ne!(original, AgentId::parse("future-agent2").unwrap());
        let map = HashMap::from([(original, 42)]);
        assert_eq!(map.get(&AgentId::parse("future-agent").unwrap()), Some(&42));
        let sorted: BTreeSet<_> = ["aa", "a0", "a", "a-", "b"]
            .map(|id| AgentId::parse(id).unwrap())
            .into_iter()
            .collect();
        assert_eq!(
            sorted.iter().map(AgentId::as_str).collect::<Vec<_>>(),
            ["a", "a-", "a0", "aa", "b"]
        );
    }

    #[test]
    fn accepts_new_ids_without_registry_membership() {
        let id = AgentId::parse("future-agent-42").unwrap();
        assert!(!AgentId::ALL.contains(&id));
        assert_eq!(id.to_string(), "future-agent-42");
        assert_eq!(format!("{id:?}"), "AgentId(\"future-agent-42\")");
    }

    #[test]
    fn legacy_constants_are_canonical_values() {
        for id in AgentId::ALL {
            assert_eq!(AgentId::parse(id.as_str()).unwrap(), id);
        }
        assert_eq!(AgentId::Antigravity.as_str(), "agy");
        assert_eq!(AgentId::GithubCopilot.as_str(), "copilot");
        assert_eq!(AgentId::OpenCode.as_str(), "opencode");
    }
}
