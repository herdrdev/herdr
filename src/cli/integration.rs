use crate::api::schema::IntegrationTarget;

pub(super) fn run_integration_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_integration_help();
        return Ok(2);
    };

    match subcommand {
        "install" => integration_install(&args[1..]),
        "uninstall" => integration_uninstall(&args[1..]),
        "status" => integration_status(&args[1..]),
        "help" | "--help" | "-h" => {
            print_integration_help();
            Ok(0)
        }
        _ => {
            print_integration_help();
            Ok(2)
        }
    }
}

fn integration_status(args: &[String]) -> std::io::Result<i32> {
    let outdated_only = match args {
        [] => false,
        [flag] if flag == "--outdated-only" => true,
        _ => {
            eprintln!("usage: herdr integration status [--outdated-only]");
            return Ok(2);
        }
    };

    if outdated_only {
        crate::integration::print_outdated_update_notice();
        return Ok(0);
    }

    for status in crate::integration::installed_integration_statuses() {
        let target = crate::integration::integration_target_label(status.target);
        let state = describe_integration_state(
            status.state,
            status.installed_version,
            status.expected_version,
        );
        println!("{target}: {state} ({})", status.path.display());
    }

    if let Some(status) = crate::integration::experimental_letta_integration_status() {
        let state = describe_integration_state(
            status.state,
            status.installed_version,
            status.expected_version,
        );
        println!(
            "{} (experimental): {state} ({})",
            status.label,
            status.path.display()
        );
    }

    Ok(0)
}

fn describe_integration_state(
    state: crate::integration::IntegrationStatusKind,
    installed_version: Option<u32>,
    expected_version: u32,
) -> String {
    let version = match installed_version {
        Some(version) => format!("v{version}"),
        None => "legacy".to_string(),
    };
    match state {
        crate::integration::IntegrationStatusKind::NotInstalled => "not installed".to_string(),
        crate::integration::IntegrationStatusKind::Current => format!("current ({version})"),
        crate::integration::IntegrationStatusKind::Outdated
            if installed_version.is_some_and(|installed| installed >= expected_version) =>
        {
            format!("needs repair ({version})")
        }
        crate::integration::IntegrationStatusKind::Outdated => {
            format!("outdated ({version} < v{expected_version})")
        }
    }
}

fn integration_install(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = parse_integration_target(args, "install")? else {
        return Ok(2);
    };

    let installed = match target {
        IntegrationCommandTarget::Builtin(target) => crate::integration::install_target(target),
        IntegrationCommandTarget::Letta => crate::integration::install_experimental_letta(),
    };
    match installed {
        Ok(messages) => {
            print_integration_messages(messages);
            Ok(0)
        }
        Err(err) => {
            eprintln!("{err}");
            Ok(1)
        }
    }
}

fn integration_uninstall(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = parse_integration_target(args, "uninstall")? else {
        return Ok(2);
    };

    let removed = match target {
        IntegrationCommandTarget::Builtin(target) => crate::integration::uninstall_target(target),
        IntegrationCommandTarget::Letta => crate::integration::uninstall_experimental_letta(),
    };
    match removed {
        Ok(messages) => {
            print_integration_messages(messages);
            Ok(0)
        }
        Err(err) => {
            eprintln!("{err}");
            Ok(1)
        }
    }
}

fn print_integration_messages(messages: Vec<String>) {
    for message in messages {
        println!("{message}");
    }
}

// Letta retains master's CLI-only installer without extending the frozen endpoint enum.
#[derive(Debug, PartialEq, Eq)]
enum IntegrationCommandTarget {
    Builtin(IntegrationTarget),
    Letta,
}

fn integration_target_labels() -> Vec<String> {
    crate::agents::registry()
        .integration_capable_profiles()
        .map(|profile| {
            profile
                .integration()
                .expect("integration-capable profile must contain metadata")
                .cli_label()
                .to_owned()
        })
        .chain(std::iter::once("letta".to_owned()))
        .collect()
}

fn print_integration_usage(action: &str) {
    eprintln!(
        "usage: herdr integration {action} <{}>",
        integration_target_labels().join("|")
    );
}

fn parse_integration_target(
    args: &[String],
    action: &str,
) -> std::io::Result<Option<IntegrationCommandTarget>> {
    let Some(target) = args.first().map(|arg| arg.as_str()) else {
        print_integration_usage(action);
        return Ok(None);
    };
    if args.len() != 1 {
        print_integration_usage(action);
        return Ok(None);
    }

    if target == "letta" {
        return Ok(Some(IntegrationCommandTarget::Letta));
    }
    let Some(parsed) = crate::agents::registry()
        .profile_by_integration_cli_name(target)
        .and_then(|profile| profile.integration())
        .map(|integration| integration.target())
    else {
        eprintln!("unknown integration target: {target}");
        eprintln!(
            "currently supported: {}",
            integration_target_labels().join(", ")
        );
        return Ok(None);
    };

    Ok(Some(IntegrationCommandTarget::Builtin(parsed)))
}

fn print_integration_help() {
    eprintln!("herdr integration commands:");
    for action in ["install", "uninstall"] {
        for label in integration_target_labels() {
            eprintln!("  herdr integration {action} {label}");
        }
    }
    eprintln!("  herdr integration status [--outdated-only]");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integration_cli_labels_and_aliases_route_through_registry() {
        for profile in crate::agents::registry().integration_capable_profiles() {
            let integration = profile.integration().expect("integration metadata");
            for name in std::iter::once(integration.cli_label())
                .chain(integration.cli_aliases().iter().map(String::as_str))
            {
                assert_eq!(
                    parse_integration_target(&[name.to_string()], "install").unwrap(),
                    Some(IntegrationCommandTarget::Builtin(integration.target()))
                );
            }
        }
    }

    #[test]
    fn integration_cli_parsing_remains_exact() {
        for rejected in ["agy", "antigravity", "Antigravity-cli", " kilo "] {
            assert_eq!(
                parse_integration_target(&[rejected.to_string()], "install").unwrap(),
                None
            );
        }
    }
}
