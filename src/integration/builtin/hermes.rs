use std::io;
use std::path::PathBuf;

use crate::agents::integration::IntegrationAdapter;
use crate::integration::hermes_plugin_dir;
#[cfg(windows)]
use crate::integration::{executable_file_exists, hermes_dir};
use crate::integration::{install_hermes, uninstall_hermes};

pub(crate) const PLUGIN_INSTALL_NAME: &str = "herdr-agent-state";
pub(crate) const PLUGIN_MANIFEST_INSTALL_NAME: &str = "plugin.yaml";
pub(crate) const PLUGIN_INIT_INSTALL_NAME: &str = "__init__.py";
pub(crate) const PLUGIN_MANIFEST_ASSET: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/agent-registry/agents/hermes/assets/plugin.yaml"
));
pub(crate) const PLUGIN_INIT_ASSET: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/agent-registry/agents/hermes/assets/__init__.py"
));

pub(super) const ADAPTER: IntegrationAdapter =
    IntegrationAdapter::new(install_adapter, uninstall_adapter, integration_path)
        .with_install_layout_probe(install_layout_available);

fn install_adapter() -> io::Result<Vec<String>> {
    let installed = install_hermes()?;
    Ok(vec![
        format!(
            "installed hermes integration plugin to {}",
            installed.plugin_dir.display()
        ),
        format!(
            "enabled hermes plugin in {}",
            installed.config_path.display()
        ),
    ])
}

fn uninstall_adapter() -> io::Result<Vec<String>> {
    let result = uninstall_hermes()?;
    let mut messages = Vec::new();
    if result.removed_plugin_dir {
        messages.push(format!(
            "removed hermes integration plugin at {}",
            result.plugin_dir.display()
        ));
    } else {
        messages.push(format!(
            "no hermes integration plugin found at {}",
            result.plugin_dir.display()
        ));
    }
    if result.updated_config {
        messages.push(format!(
            "disabled hermes plugin in {}",
            result.config_path.display()
        ));
    } else {
        messages.push(format!(
            "no hermes plugin entry found in {}",
            result.config_path.display()
        ));
    }
    Ok(messages)
}

fn integration_path() -> io::Result<PathBuf> {
    hermes_plugin_dir().map(|dir| dir.join(PLUGIN_INIT_INSTALL_NAME))
}

pub(crate) fn install_layout_available() -> bool {
    #[cfg(windows)]
    {
        let Ok(dir) = hermes_dir() else {
            return false;
        };
        [
            dir.join("hermes.exe"),
            dir.join("bin").join("hermes.exe"),
            dir.join("Scripts").join("hermes.exe"),
        ]
        .into_iter()
        .any(|path| executable_file_exists(&path))
    }

    #[cfg(not(windows))]
    {
        false
    }
}
