use std::io;

use super::registry::registered_integration_profile;
use super::version::enforce_agent_version;

pub(crate) fn install_target(
    target: crate::api::schema::IntegrationTarget,
) -> io::Result<Vec<String>> {
    let profile = registered_integration_profile(target);
    let result = match &profile {
        Ok(profile) => install_target_inner(profile),
        Err(error) => Err(io::Error::other(error.to_string())),
    };
    let outcome = if result.is_ok() { "ok" } else { "error" };
    let label = profile
        .as_ref()
        .map(|profile| profile.cli_label())
        .unwrap_or("unknown");
    crate::logging::integration_action("install", label, outcome);
    result
}

fn install_target_inner(
    profile: &crate::agents::integration::IntegrationProfile,
) -> io::Result<Vec<String>> {
    let adapter = profile.adapter();

    if !profile.supported() {
        return Err(io::Error::other(format!(
            "{} integration is not supported on Windows",
            profile.cli_label()
        )));
    }

    let version_warning = match adapter.agent_version_requirement() {
        Some(requirement) => enforce_agent_version(requirement)?,
        None => None,
    };

    let mut messages = adapter.install(profile)?;
    if let Some(warning) = version_warning {
        messages.push(warning);
    }

    Ok(messages)
}

pub(crate) fn uninstall_target(
    target: crate::api::schema::IntegrationTarget,
) -> io::Result<Vec<String>> {
    let profile = registered_integration_profile(target)?;
    let messages = profile.adapter().uninstall()?;

    crate::logging::integration_action("uninstall", profile.cli_label(), "ok");
    Ok(messages)
}
