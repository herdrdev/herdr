use std::fs;
use std::io;
use std::path::PathBuf;

use crate::agents::integration::IntegrationAdapter;
use crate::integration::codex_dir;
use crate::integration::executable_file_exists;
use crate::integration::{install_codex, uninstall_codex};

pub(super) const HOOK_INSTALL_NAME_UNIX: &str = "herdr-agent-state.sh";
pub(super) const HOOK_INSTALL_NAME_WINDOWS: &str = "herdr-agent-state.ps1";
pub(crate) const HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    HOOK_INSTALL_NAME_WINDOWS
} else {
    HOOK_INSTALL_NAME_UNIX
};
pub(crate) const HOOK_ASSET: &str = if cfg!(windows) {
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/vendor/agent-registry/agents/codex/assets/herdr-agent-state.ps1"
    ))
} else {
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/vendor/agent-registry/agents/codex/assets/herdr-agent-state.sh"
    ))
};

pub(super) const ADAPTER: IntegrationAdapter =
    IntegrationAdapter::new(install_adapter, uninstall_adapter, integration_path)
        .with_install_layout_probe(standalone_binary_available);

fn install_adapter() -> io::Result<Vec<String>> {
    let installed = install_codex()?;
    Ok(vec![
        format!(
            "installed codex integration hook to {}",
            installed.hook_path.display()
        ),
        format!("ensured codex hooks at {}", installed.hooks_path.display()),
        format!(
            "ensured codex config at {}",
            installed.config_path.display()
        ),
    ])
}

fn uninstall_adapter() -> io::Result<Vec<String>> {
    let result = uninstall_codex()?;
    let mut messages = Vec::new();
    if result.removed_hook_file {
        messages.push(format!(
            "removed codex hook at {}",
            result.hook_path.display()
        ));
    } else {
        messages.push(format!(
            "no codex hook found at {}",
            result.hook_path.display()
        ));
    }
    if result.updated_hooks {
        messages.push(format!(
            "removed herdr codex hook entries from {}",
            result.hooks_path.display()
        ));
    } else {
        messages.push(format!(
            "no herdr codex hook entries found in {}",
            result.hooks_path.display()
        ));
    }
    messages.push(format!(
        "left codex config unchanged at {}",
        result.config_path.display()
    ));
    Ok(messages)
}

fn integration_path() -> io::Result<PathBuf> {
    codex_dir().map(|dir| dir.join(HOOK_INSTALL_NAME))
}

fn standalone_binary_available() -> bool {
    let Ok(releases_dir) =
        codex_dir().map(|dir| dir.join("packages").join("standalone").join("releases"))
    else {
        return false;
    };
    let Ok(entries) = fs::read_dir(releases_dir) else {
        return false;
    };

    entries
        .filter_map(Result::ok)
        .any(|entry| executable_file_exists(&entry.path().join("bin").join(executable_name())))
}

pub(crate) fn executable_name() -> &'static str {
    if cfg!(windows) {
        "codex.exe"
    } else {
        "codex"
    }
}
