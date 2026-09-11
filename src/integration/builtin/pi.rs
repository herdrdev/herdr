use std::io;
use std::path::PathBuf;

use crate::agents::integration::IntegrationAdapter;
use crate::integration::pi_extension_dir;
use crate::integration::{install_pi, uninstall_pi};

pub(crate) const EXTENSION_INSTALL_NAME: &str = "herdr-agent-state.ts";
pub(crate) const EXTENSION_ASSET: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/agent-registry/agents/pi/assets/herdr-agent-state.ts"
));

pub(super) const ADAPTER: IntegrationAdapter =
    IntegrationAdapter::new(install_adapter, uninstall_adapter, integration_path);

fn install_adapter() -> io::Result<Vec<String>> {
    let path = install_pi()?;
    Ok(vec![format!(
        "installed pi integration to {}",
        path.display()
    )])
}

fn uninstall_adapter() -> io::Result<Vec<String>> {
    let result = uninstall_pi()?;
    Ok(if result.removed_extension {
        vec![format!(
            "removed pi integration extension at {}",
            result.extension_path.display()
        )]
    } else {
        vec![format!(
            "no pi integration extension found at {}",
            result.extension_path.display()
        )]
    })
}

fn integration_path() -> io::Result<PathBuf> {
    pi_extension_dir().map(|dir| dir.join(EXTENSION_INSTALL_NAME))
}
