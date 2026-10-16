//! Common OpenSSH control socket options used by remote transports.
use std::path::Path;
use std::process::Command;

pub(crate) fn apply(command: &mut Command, control_path: &Path) {
    apply_with_persist(command, control_path, "yes");
}

pub(crate) fn apply_with_persist(command: &mut Command, control_path: &Path, persist: &str) {
    command
        .arg("-S")
        .arg(control_path)
        .args(["-o", "ControlMaster=auto", "-o"])
        .arg(format!("ControlPersist={persist}"));
}
