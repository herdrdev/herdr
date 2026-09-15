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

/// Integration target accepted by the CLI. Letta is deliberately kept out of
/// the frozen client endpoint `IntegrationTarget` enum and is handled as an
/// experimental CLI-only target until the agent registry replaces it.
enum IntegrationCommandTarget {
    Builtin(IntegrationTarget),
    Letta,
}

fn parse_integration_target(
    args: &[String],
    action: &str,
) -> std::io::Result<Option<IntegrationCommandTarget>> {
    let Some(target) = args.first().map(|arg| arg.as_str()) else {
        eprintln!(
            "usage: herdr integration {action} <pi|omp|claude|codex|copilot|devin|droid|kimi|opencode|kilo|hermes|qodercli|qwen|letta|cursor|mastracode|grok|muse>"
        );
        return Ok(None);
    };
    if args.len() != 1 {
        eprintln!(
            "usage: herdr integration {action} <pi|omp|claude|codex|copilot|devin|droid|kimi|opencode|kilo|hermes|qodercli|qwen|letta|cursor|mastracode|grok|muse>"
        );
        return Ok(None);
    }

    let parsed = match target {
        "pi" => IntegrationCommandTarget::Builtin(IntegrationTarget::Pi),
        "omp" => IntegrationCommandTarget::Builtin(IntegrationTarget::Omp),
        "claude" => IntegrationCommandTarget::Builtin(IntegrationTarget::Claude),
        "codex" => IntegrationCommandTarget::Builtin(IntegrationTarget::Codex),
        "copilot" => IntegrationCommandTarget::Builtin(IntegrationTarget::Copilot),
        "devin" => IntegrationCommandTarget::Builtin(IntegrationTarget::Devin),
        "droid" => IntegrationCommandTarget::Builtin(IntegrationTarget::Droid),
        "kimi" => IntegrationCommandTarget::Builtin(IntegrationTarget::Kimi),
        "opencode" => IntegrationCommandTarget::Builtin(IntegrationTarget::Opencode),
        "kilo" => IntegrationCommandTarget::Builtin(IntegrationTarget::Kilo),
        "hermes" => IntegrationCommandTarget::Builtin(IntegrationTarget::Hermes),
        "qodercli" => IntegrationCommandTarget::Builtin(IntegrationTarget::Qodercli),
        "qwen" => IntegrationCommandTarget::Builtin(IntegrationTarget::Qwen),
        "letta" => IntegrationCommandTarget::Letta,
        "cursor" => IntegrationCommandTarget::Builtin(IntegrationTarget::Cursor),
        "mastracode" => IntegrationCommandTarget::Builtin(IntegrationTarget::Mastracode),
        "antigravity-cli" | "antigravity_cli" => {
            IntegrationCommandTarget::Builtin(IntegrationTarget::AntigravityCli)
        }
        "grok" => IntegrationCommandTarget::Builtin(IntegrationTarget::Grok),
        "muse" => IntegrationCommandTarget::Builtin(IntegrationTarget::Muse),
        _ => {
            eprintln!("unknown integration target: {target}");
            eprintln!(
                "currently supported: pi, omp, claude, codex, copilot, devin, droid, kimi, opencode, kilo, hermes, qodercli, qwen, letta, cursor, mastracode, antigravity-cli, grok, muse"
            );
            return Ok(None);
        }
    };

    Ok(Some(parsed))
}

fn print_integration_help() {
    eprintln!("herdr integration commands:");
    eprintln!("  herdr integration install pi");
    eprintln!("  herdr integration install omp");
    eprintln!("  herdr integration install claude");
    eprintln!("  herdr integration install codex");
    eprintln!("  herdr integration install copilot");
    eprintln!("  herdr integration install devin");
    eprintln!("  herdr integration install droid");
    eprintln!("  herdr integration install kimi");
    eprintln!("  herdr integration install opencode");
    eprintln!("  herdr integration install kilo");
    eprintln!("  herdr integration install hermes");
    eprintln!("  herdr integration install qodercli");
    eprintln!("  herdr integration install qwen");
    eprintln!("  herdr integration install letta");
    eprintln!("  herdr integration install cursor");
    eprintln!("  herdr integration install mastracode");
    eprintln!("  herdr integration install antigravity-cli");
    eprintln!("  herdr integration install grok");
    eprintln!("  herdr integration install muse");
    eprintln!("  herdr integration uninstall pi");
    eprintln!("  herdr integration uninstall omp");
    eprintln!("  herdr integration uninstall claude");
    eprintln!("  herdr integration uninstall codex");
    eprintln!("  herdr integration uninstall copilot");
    eprintln!("  herdr integration uninstall devin");
    eprintln!("  herdr integration uninstall droid");
    eprintln!("  herdr integration uninstall kimi");
    eprintln!("  herdr integration uninstall opencode");
    eprintln!("  herdr integration uninstall kilo");
    eprintln!("  herdr integration uninstall hermes");
    eprintln!("  herdr integration uninstall qodercli");
    eprintln!("  herdr integration uninstall qwen");
    eprintln!("  herdr integration uninstall letta");
    eprintln!("  herdr integration uninstall cursor");
    eprintln!("  herdr integration uninstall mastracode");
    eprintln!("  herdr integration uninstall antigravity-cli");
    eprintln!("  herdr integration uninstall grok");
    eprintln!("  herdr integration uninstall muse");
    eprintln!("  herdr integration status [--outdated-only]");
}
