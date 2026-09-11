//! Stage-one compatibility boundary: metadata cannot redirect fixed installers.
//! Only installer assumptions live here, not CLI metadata, versions, or asset bytes.

use crate::agents::source::Package;

use super::{
    agy, claude, codex, copilot, cursor, devin, droid, grok, hermes, kilo, kimi, mastracode, omp,
    opencode, pi, qodercli, qwen,
};

#[derive(Clone, Copy)]
struct AssetContract {
    path: &'static str,
    install_name: &'static str,
    platform: &'static str,
    role: &'static str,
}

impl AssetContract {
    const fn new(
        path: &'static str,
        install_name: &'static str,
        platform: &'static str,
        role: &'static str,
    ) -> Self {
        Self {
            path,
            install_name,
            platform,
            role,
        }
    }
}

// The source paths match the fixed include_str! paths in the hook adapters;
// installed names come from the same constants used by those installers.
fn shell_hook(
    unix_install_name: &'static str,
    windows_install_name: &'static str,
    session: bool,
) -> Vec<AssetContract> {
    let (unix_path, windows_path) = if session {
        (
            "assets/herdr-agent-session.sh",
            "assets/herdr-agent-session.ps1",
        )
    } else {
        (
            "assets/herdr-agent-state.sh",
            "assets/herdr-agent-state.ps1",
        )
    };
    vec![
        AssetContract::new(unix_path, unix_install_name, "unix", "reporter"),
        AssetContract::new(windows_path, windows_install_name, "windows", "reporter"),
    ]
}

/// Validate both platforms without reading files, comparing bytes, or running probes.
/// Unknown packages remain inert; source validation owns their structural validity.
pub(crate) fn validate_package(package: &Package) -> Result<(), String> {
    let Some(integration) = &package.integration else {
        return Ok(());
    };
    macro_rules! hook {
        ($module:ident) => {
            shell_hook(
                $module::HOOK_INSTALL_NAME_UNIX,
                $module::HOOK_INSTALL_NAME_WINDOWS,
                false,
            )
        };
    }
    let expected = match package.identity.id.as_str() {
        "agy" => hook!(agy),
        "claude" => hook!(claude),
        "codex" => hook!(codex),
        "copilot" => hook!(copilot),
        "cursor" => hook!(cursor),
        "devin" => hook!(devin),
        "droid" => hook!(droid),
        "grok" => hook!(grok),
        "kimi" => hook!(kimi),
        "mastracode" => hook!(mastracode),
        "qodercli" => hook!(qodercli),
        "qwen" => shell_hook(
            qwen::HOOK_INSTALL_NAME_UNIX,
            qwen::HOOK_INSTALL_NAME_WINDOWS,
            true,
        ),
        "pi" => vec![AssetContract::new(
            "assets/herdr-agent-state.ts",
            pi::EXTENSION_INSTALL_NAME,
            "all",
            "reporter",
        )],
        "omp" => vec![AssetContract::new(
            "assets/herdr-agent-state.ts",
            omp::EXTENSION_INSTALL_NAME,
            "all",
            "reporter",
        )],
        "kilo" => vec![AssetContract::new(
            "assets/herdr-agent-state.js",
            kilo::PLUGIN_INSTALL_NAME,
            "all",
            "reporter",
        )],
        "opencode" => vec![
            AssetContract::new(
                "assets/herdr-agent-state.js",
                opencode::PLUGIN_INSTALL_NAME,
                "all",
                "reporter",
            ),
            AssetContract::new(
                "assets/herdr-tui-session.js",
                opencode::TUI_PLUGIN_INSTALL_NAME,
                "all",
                "tui",
            ),
        ],
        "hermes" => vec![
            AssetContract::new(
                "assets/__init__.py",
                hermes::PLUGIN_INIT_INSTALL_NAME,
                "all",
                "reporter",
            ),
            AssetContract::new(
                "assets/plugin.yaml",
                hermes::PLUGIN_MANIFEST_INSTALL_NAME,
                "all",
                "manifest",
            ),
        ],
        _ => return Ok(()),
    };
    let error = |detail: &str| {
        format!(
            "{}: built-in integration asset contract mismatch: {detail}",
            package.identity.id
        )
    };
    if !integration.supported.unix || !integration.supported.windows {
        return Err(error("fixed assets require both unix and windows support"));
    }
    if integration.assets.len() != expected.len() {
        return Err(error("unexpected asset count"));
    }
    // Exact coverage (rather than zip/order comparison) rejects missing, extra,
    // duplicate, and remapped assets while allowing metadata entry reordering.
    for contract in expected {
        let count = integration
            .assets
            .iter()
            .filter(|asset| {
                asset.path == contract.path
                    && asset.install_name == contract.install_name
                    && asset.platform == contract.platform
                    && asset.role == contract.role
            })
            .count();
        if count != 1 {
            return Err(error(&format!(
                "expected {} installed as {} with role {} on {}",
                contract.path, contract.install_name, contract.role, contract.platform
            )));
        }
    }
    Ok(())
}
