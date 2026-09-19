//! Trusted built-in installers and validators for registry integration profiles.

mod contract;

pub(crate) use contract::validate_package;

pub(crate) mod agy;
pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod copilot;
pub(crate) mod cursor;
pub(crate) mod devin;
pub(crate) mod droid;
pub(crate) mod grok;
pub(crate) mod hermes;
pub(crate) mod kilo;
pub(crate) mod kimi;
pub(crate) mod mastracode;
pub(crate) mod omp;
pub(crate) mod opencode;
pub(crate) mod pi;
pub(crate) mod qodercli;
pub(crate) mod qwen;

use crate::agents::integration::IntegrationAdapter;
use crate::api::schema::IntegrationTarget;
use crate::detect::Agent;

#[cfg(test)]
pub(crate) fn adapter(agent: Agent) -> Option<IntegrationAdapter> {
    binding(agent).map(|(_, adapter)| adapter)
}

/// Bind registry metadata to a trusted target and its installer implementation.
pub(crate) fn binding(agent: Agent) -> Option<(IntegrationTarget, IntegrationAdapter)> {
    match agent {
        Agent::Antigravity => Some((IntegrationTarget::AntigravityCli, agy::ADAPTER)),
        Agent::Claude => Some((IntegrationTarget::Claude, claude::ADAPTER)),
        Agent::Codex => Some((IntegrationTarget::Codex, codex::ADAPTER)),
        Agent::GithubCopilot => Some((IntegrationTarget::Copilot, copilot::ADAPTER)),
        Agent::Cursor => Some((IntegrationTarget::Cursor, cursor::ADAPTER)),
        Agent::Devin => Some((IntegrationTarget::Devin, devin::ADAPTER)),
        Agent::Droid => Some((IntegrationTarget::Droid, droid::ADAPTER)),
        Agent::Grok => Some((IntegrationTarget::Grok, grok::ADAPTER)),
        Agent::Hermes => Some((IntegrationTarget::Hermes, hermes::ADAPTER)),
        Agent::Kilo => Some((IntegrationTarget::Kilo, kilo::ADAPTER)),
        Agent::Kimi => Some((IntegrationTarget::Kimi, kimi::ADAPTER)),
        Agent::Mastracode => Some((IntegrationTarget::Mastracode, mastracode::ADAPTER)),
        Agent::Omp => Some((IntegrationTarget::Omp, omp::ADAPTER)),
        Agent::OpenCode => Some((IntegrationTarget::Opencode, opencode::ADAPTER)),
        Agent::Pi => Some((IntegrationTarget::Pi, pi::ADAPTER)),
        Agent::Qodercli => Some((IntegrationTarget::Qodercli, qodercli::ADAPTER)),
        Agent::Qwen => Some((IntegrationTarget::Qwen, qwen::ADAPTER)),
        _ => None,
    }
}
