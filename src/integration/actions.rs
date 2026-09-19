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

/// Experimental Letta install that bypasses the frozen client endpoint
/// `IntegrationTarget` enum. Fold into the agent registry when it lands.
pub(crate) fn install_experimental_letta() -> io::Result<Vec<String>> {
    let result = super::targets::install_letta().map(|installed| {
        vec![
            format!(
                "installed letta integration hook to {}",
                installed.hook_path.display()
            ),
            format!(
                "ensured letta settings at {}",
                installed.settings_path.display()
            ),
        ]
    });
    let outcome = if result.is_ok() { "ok" } else { "error" };
    crate::logging::integration_action("install", "letta", outcome);
    result
}

/// Experimental Letta uninstall counterpart.
pub(crate) fn uninstall_experimental_letta() -> io::Result<Vec<String>> {
    let result = super::targets::uninstall_letta().map(|result| {
        let mut messages = Vec::new();
        if result.removed_hook_file {
            messages.push(format!(
                "removed letta hook at {}",
                result.hook_path.display()
            ));
        } else {
            messages.push(format!(
                "no letta hook found at {}",
                result.hook_path.display()
            ));
        }
        if result.updated_settings {
            messages.push(format!(
                "removed herdr letta hook entry from {}",
                result.settings_path.display()
            ));
        } else {
            messages.push(format!(
                "no herdr letta hook entry found in {}",
                result.settings_path.display()
            ));
        }
        messages
    });
    let outcome = if result.is_ok() { "ok" } else { "error" };
    crate::logging::integration_action("uninstall", "letta", outcome);
    result
}

fn install_target_inner(
    profile: &crate::agents::integration::IntegrationProfile,
) -> io::Result<Vec<String>> {
    let adapter = profile.adapter();

    if !profile.supported() {
        return Err(io::Error::other(format!(
            "{} integration is not supported on this platform",
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
