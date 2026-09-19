use std::io;
use std::path::PathBuf;

use crate::agents::integration::{IntegrationAdapter, IntegrationProfile};
use crate::integration::omp_extension_dir;
use crate::integration::{install_omp, uninstall_omp};

pub(crate) const EXTENSION_INSTALL_NAME: &str = "herdr-omp-agent-state.ts";
#[cfg(test)]
pub(crate) const EXTENSION_ASSET: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/agent-registry/agents/omp/assets/herdr-agent-state.ts"
));

pub(super) const ADAPTER: IntegrationAdapter =
    IntegrationAdapter::new(install_adapter, uninstall_adapter, integration_path);

fn install_adapter(profile: &IntegrationProfile) -> io::Result<Vec<String>> {
    let installed = install_omp(profile)?;
    let mut messages = Vec::new();
    if installed.removed_legacy_pi_extension {
        messages.push(format!(
            "removed legacy pi integration from omp extension directory at {}",
            installed
                .extension_path
                .with_file_name(crate::integration::builtin::pi::EXTENSION_INSTALL_NAME)
                .display()
        ));
    }
    messages.push(format!(
        "installed omp integration to {}",
        installed.extension_path.display()
    ));
    Ok(messages)
}

fn uninstall_adapter() -> io::Result<Vec<String>> {
    let result = uninstall_omp()?;
    Ok(if result.removed_extension {
        vec![format!(
            "removed omp integration extension at {}",
            result.extension_path.display()
        )]
    } else {
        vec![format!(
            "no omp integration extension found at {}",
            result.extension_path.display()
        )]
    })
}

fn integration_path() -> io::Result<PathBuf> {
    omp_extension_dir().map(|dir| dir.join(EXTENSION_INSTALL_NAME))
}
