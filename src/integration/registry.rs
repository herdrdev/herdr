use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::agents::integration::{IntegrationAdapter, IntegrationProfile};

pub(super) fn registered_integration_profile(
    target: crate::api::schema::IntegrationTarget,
) -> io::Result<Arc<IntegrationProfile>> {
    crate::agents::registry()
        .profile_by_integration_target(target)
        .and_then(|profile| profile.integration())
        .cloned()
        .ok_or_else(|| io::Error::other("integration target has no active registry profile"))
}

pub(crate) fn integration_target_label(target: crate::api::schema::IntegrationTarget) -> String {
    registered_integration_profile(target)
        .map(|profile| profile.cli_label().to_owned())
        .unwrap_or_else(|_| "unknown".to_owned())
}

#[cfg(test)]
pub(crate) fn integration_target_command(target: crate::api::schema::IntegrationTarget) -> String {
    registered_integration_profile(target)
        .ok()
        .and_then(|profile| profile.command_names().first().cloned())
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn integration_target_command_names(
    target: crate::api::schema::IntegrationTarget,
) -> Vec<String> {
    registered_integration_profile(target)
        .map(|profile| profile.command_names().to_vec())
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn integration_target_supported(target: crate::api::schema::IntegrationTarget) -> bool {
    registered_integration_profile(target).is_ok_and(|profile| profile.supported())
}

#[cfg(test)]
pub(crate) fn integration_target_available(target: crate::api::schema::IntegrationTarget) -> bool {
    registered_integration_profile(target).is_ok_and(|profile| integration_available(&profile))
}

fn integration_available(profile: &IntegrationProfile) -> bool {
    profile.supported()
        && (profile
            .command_names()
            .iter()
            .any(|command| command_available(command))
            || profile.adapter().install_layout_available())
}

pub(crate) fn command_available(command: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        command_path_candidates(&dir, command)
            .into_iter()
            .any(|path| executable_file_exists(&path))
    })
}

pub(crate) fn command_path_candidates(dir: &Path, command: &str) -> Vec<PathBuf> {
    let base = dir.join(command);

    #[cfg(not(windows))]
    {
        vec![base]
    }

    #[cfg(windows)]
    {
        if Path::new(command).extension().is_some() {
            return vec![base];
        }

        let mut candidates = vec![base];
        for extension in [".exe", ".cmd", ".bat", ".ps1"] {
            candidates.push(dir.join(format!("{command}{extension}")));
        }
        candidates
    }
}

pub(crate) fn executable_file_exists(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

pub(crate) fn installed_integration_statuses() -> Vec<super::IntegrationStatus> {
    integration_specs()
        .filter(|profile| profile.supported())
        .filter_map(|profile| {
            let path = profile.adapter().primary_installed_artifact_path().ok()?;
            Some(integration_status_with_adapter(
                profile.target(),
                path,
                profile.expected_version(),
                Some(profile.adapter()),
            ))
        })
        .collect()
}

pub(crate) fn integration_recommendations() -> Vec<super::IntegrationRecommendation> {
    let snapshot = crate::agents::registry();
    integration_recommendations_with_registry(&snapshot)
}

/// Probe recommendations against one pinned registry, including its labels and
/// adapters. Callers tracking publication generations must use this same snapshot.
pub(crate) fn integration_recommendations_with_registry(
    registry: &crate::agents::AgentRegistry,
) -> Vec<super::IntegrationRecommendation> {
    registry
        .integration_capable_profiles()
        .filter_map(|profile| profile.integration())
        .filter(|profile| profile.supported())
        .filter_map(|profile| {
            let path = profile.adapter().primary_installed_artifact_path().ok()?;
            let status = integration_status_with_adapter(
                profile.target(),
                path.clone(),
                profile.expected_version(),
                Some(profile.adapter()),
            );
            Some(super::IntegrationRecommendation {
                target: profile.target(),
                label: profile.cli_label().to_owned(),
                command: profile.command_names().first().cloned().unwrap_or_default(),
                available: integration_available(profile)
                    || status.state != super::IntegrationStatusKind::NotInstalled,
                path,
                state: status.state,
            })
        })
        .collect()
}

pub(crate) fn outdated_installed_integrations() -> Vec<super::IntegrationStatus> {
    installed_integration_statuses()
        .into_iter()
        .filter(|status| status.state == super::IntegrationStatusKind::Outdated)
        .collect()
}

fn integration_specs() -> impl Iterator<Item = Arc<IntegrationProfile>> {
    crate::agents::registry()
        .integration_capable_profiles()
        .filter_map(|profile| profile.integration().cloned())
        .collect::<Vec<_>>()
        .into_iter()
}

pub(crate) fn integration_update_instructions(
    targets: &[crate::api::schema::IntegrationTarget],
) -> String {
    let registry = crate::agents::registry();
    let commands: Vec<String> = targets
        .iter()
        .map(|target| {
            let label = registry
                .profile_by_integration_target(*target)
                .and_then(|profile| profile.integration())
                .map(|profile| profile.cli_label())
                .unwrap_or("unknown");
            format!("`herdr integration install {label}`")
        })
        .collect();

    match commands.as_slice() {
        [] => String::new(),
        [command] => format!("run {command}"),
        [rest @ .., last] => format!("run {} and {last}", rest.join(", ")),
    }
}

pub(crate) fn print_outdated_update_notice() -> bool {
    let outdated = outdated_installed_integrations();
    if outdated.is_empty() {
        return false;
    }

    let targets = outdated
        .iter()
        .map(|integration| integration.target)
        .collect::<Vec<_>>();
    eprintln!(
        "installed herdr integrations need updating; {}.",
        integration_update_instructions(&targets).replace('`', "")
    );
    true
}

#[cfg(test)]
pub(crate) fn integration_status_at(
    target: crate::api::schema::IntegrationTarget,
    path: PathBuf,
    expected_version: u32,
) -> super::IntegrationStatus {
    let adapter = registered_integration_profile(target)
        .ok()
        .map(|profile| profile.adapter());
    integration_status_with_adapter(target, path, expected_version, adapter)
}

fn integration_status_with_adapter(
    target: crate::api::schema::IntegrationTarget,
    path: PathBuf,
    expected_version: u32,
    adapter: Option<IntegrationAdapter>,
) -> super::IntegrationStatus {
    if !path.is_file() {
        return super::IntegrationStatus {
            target,
            path,
            state: super::IntegrationStatusKind::NotInstalled,
            installed_version: None,
            expected_version,
        };
    }

    let installed_version = fs::read_to_string(&path)
        .ok()
        .and_then(|content| parse_integration_version(&content));
    let mut state = if installed_version.is_some_and(|version| version >= expected_version) {
        super::IntegrationStatusKind::Current
    } else {
        super::IntegrationStatusKind::Outdated
    };

    // Some integrations need companion config or artifacts in addition to the
    // primary versioned artifact. A current primary artifact with invalid
    // companions is nonfunctional, so report it as outdated and let reinstall
    // repair the complete integration.
    if state == super::IntegrationStatusKind::Current
        && !adapter
            .is_some_and(|adapter| adapter.current_install_extra_is_valid(&path, expected_version))
    {
        state = super::IntegrationStatusKind::Outdated;
    }

    super::IntegrationStatus {
        target,
        path,
        state,
        installed_version,
        expected_version,
    }
}

pub(crate) fn parse_integration_version(content: &str) -> Option<u32> {
    content.lines().find_map(|line| {
        let marker_line = line
            .trim()
            .trim_start_matches('/')
            .trim_start_matches('#')
            .trim();
        marker_line
            .strip_prefix(super::INTEGRATION_VERSION_MARKER)?
            .trim()
            .parse()
            .ok()
    })
}
