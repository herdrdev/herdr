use std::io;
use std::path::PathBuf;

use crate::agents::integration::{IntegrationAdapter, IntegrationProfile};
use crate::integration::kilo_dir;
use crate::integration::{install_kilo, uninstall_kilo};

pub(crate) const PLUGIN_INSTALL_NAME: &str = "herdr-agent-state.js";
#[cfg(test)]
pub(crate) const PLUGIN_ASSET: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/agent-registry/agents/kilo/assets/herdr-agent-state.js"
));

pub(super) const ADAPTER: IntegrationAdapter =
    IntegrationAdapter::new(install_adapter, uninstall_adapter, integration_path);

fn install_adapter(profile: &IntegrationProfile) -> io::Result<Vec<String>> {
    let installed = install_kilo(profile)?;
    Ok(vec![format!(
        "installed kilo integration plugin to {}",
        installed.plugin_path.display()
    )])
}

fn uninstall_adapter() -> io::Result<Vec<String>> {
    let result = uninstall_kilo()?;
    Ok(if result.removed_plugin {
        vec![format!(
            "removed kilo integration plugin at {}",
            result.plugin_path.display()
        )]
    } else {
        vec![format!(
            "no kilo integration plugin found at {}",
            result.plugin_path.display()
        )]
    })
}

fn integration_path() -> io::Result<PathBuf> {
    kilo_dir().map(|dir| dir.join("plugin").join(PLUGIN_INSTALL_NAME))
}
