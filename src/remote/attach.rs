//! Remote thin-client launcher over SSH command stdio.

use super::{args::*, process::wait_with_output_timeout, restart_policy::*, shell_quote};
use base64::Engine as _;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, IsTerminal, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use interprocess::local_socket::traits::Listener as _;
#[cfg(all(test, unix))]
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::ListenerNonblockingMode;
use interprocess::TryClone as _;
use serde::Deserialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const BRIDGE_ACCEPT_POLL: Duration = Duration::from_millis(50);
const BRIDGE_IO_POLL: Duration = Duration::from_millis(1);
const BRIDGE_SOCKET_PERMISSION_MODE: u32 = 0o600;
const REMOTE_SERVER_SHUTDOWN_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);
const NONINTERACTIVE_SSH_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const NONINTERACTIVE_SSH_STDERR_LIMIT: usize = 16 * 1024;
const BRIDGE_FAILURE_REPORT_TIMEOUT: Duration = Duration::from_secs(1);
const REMOTE_SERVER_SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(100);
const CURRENT_PROTOCOL: u32 = crate::protocol::PROTOCOL_VERSION;
const STABLE_UPDATE_MANIFEST_URL: &str = "https://herdr.dev/latest.json";
const PREVIEW_UPDATE_MANIFEST_URL: &str = "https://herdr.dev/preview.json";
const REMOTE_BINARY_ENV_VAR: &str = "HERDR_REMOTE_BINARY";
const REMOTE_OUTPUT_READY_MARKER: &str = "herdr-remote-output-ready:1";
const SSH_CONTROL_SOCKET_NAME: &str = "ctl";
pub(crate) fn run_remote(remote: RemoteLaunch) -> io::Result<()> {
    let session_name = crate::session::active_name()
        .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_string());
    let local_socket = local_forward_socket_path(&remote.target, &session_name);
    let program = std::env::args()
        .next()
        .unwrap_or_else(|| "herdr".to_string());
    let reattach_command = reattach_command(
        &program,
        &remote.target,
        &session_name,
        remote.keybindings,
        remote.live_handoff,
    );
    let manage_ssh_config = crate::config::Config::load()
        .config
        .remote
        .manage_ssh_config;
    let require_surface_interest = crate::client::endpoint::EndpointCatalog::load()
        .map(|catalog| catalog.contains_enabled_target_session(&remote.target, &session_name))
        .unwrap_or(false);
    let remote_ssh = RemoteSsh::new(
        remote.target.clone(),
        manage_ssh_config,
        session_name.clone(),
    );
    let prepared_remote =
        prepare_remote_herdr(&remote_ssh, remote.live_handoff, require_surface_interest)?;
    ensure_remote_server_ready(
        &remote_ssh,
        &prepared_remote.remote_herdr,
        prepared_remote.stop_after_install_approved,
        remote.live_handoff,
        require_surface_interest,
    )?;

    let _bridge = SshStdioBridge::start(
        remote.target,
        prepared_remote.remote_herdr,
        local_socket.clone(),
        session_name,
        remote_ssh.options(),
        false,
    )?;

    run_client_process(&local_socket, &reattach_command, remote.keybindings)
}

pub(crate) fn prepare_saved_ssh(target: &str, session_name: &str) -> io::Result<()> {
    super::validate_remote_target(target)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    crate::session::validate_name(session_name)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let manage_ssh_config = crate::config::Config::load()
        .config
        .remote
        .manage_ssh_config;
    let ssh = RemoteSsh::new(
        target.to_owned(),
        manage_ssh_config,
        session_name.to_owned(),
    );
    let prepared = prepare_remote_herdr(&ssh, false, true)?;
    ensure_remote_server_ready(
        &ssh,
        &prepared.remote_herdr,
        prepared.stop_after_install_approved,
        false,
        true,
    )?;

    // The bridge already owns daemon startup. EOF closes only this temporary attachment,
    // leaving the named server running even when no local TUI is open yet.
    let command = prepared
        .remote_herdr
        .executable
        .saved_bridge_command(session_name);
    let output = ssh.shell_output(&prepared.remote_herdr.platform, &command)?;
    if !output.status.success() {
        return Err(command_failed("remote server startup failed", &output));
    }
    match remote_server_status(&ssh, &prepared.remote_herdr, true)? {
        RemoteServerStatus::Running {
            endpoint_protocol_generation,
            surface_interest,
            health_check,
            detached_server_daemon,
            ..
        } if remote_server_restart_reason(
            endpoint_protocol_generation,
            detached_server_daemon,
            true,
            surface_interest,
            health_check,
        )
        .is_none() =>
        {
            Ok(())
        }
        _ => Err(io::Error::other(
            "remote server is not ready for saved machines",
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemotePlatform {
    os: &'static str,
    arch: &'static str,
}

impl RemotePlatform {
    fn from_uname(os: &str, arch: &str) -> Option<Self> {
        let os = match os.trim() {
            "Linux" => "linux",
            "Darwin" => "macos",
            _ => return None,
        };
        let arch = match arch.trim() {
            "x86_64" | "amd64" => "x86_64",
            "aarch64" | "arm64" => "aarch64",
            _ => return None,
        };
        Some(Self { os, arch })
    }

    fn local() -> Self {
        let os = if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(target_os = "windows") {
            "windows"
        } else {
            "unknown"
        };

        let arch = if cfg!(target_arch = "x86_64") {
            "x86_64"
        } else if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "unknown"
        };

        Self { os, arch }
    }

    fn asset_key(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }

    fn is_windows(&self) -> bool {
        self.os == "windows"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteExecutable {
    PosixShellPath(String),
    WindowsPath(String),
}

impl RemoteExecutable {
    fn display(&self) -> &str {
        match self {
            Self::PosixShellPath(path) | Self::WindowsPath(path) => path,
        }
    }

    fn command(&self, args: &[&str]) -> String {
        match self {
            Self::PosixShellPath(path) => {
                let mut command = path.clone();
                for arg in args {
                    command.push(' ');
                    command.push_str(&shell_quote(arg));
                }
                command
            }
            Self::WindowsPath(path) => windows_powershell_script_command(
                &windows_powershell_application_script(path, args),
            ),
        }
    }

    fn session_command(&self, session_name: &str, args: &[&str]) -> String {
        self.command(&Self::session_args(session_name, args))
    }

    fn session_args<'a>(session_name: &'a str, args: &[&'a str]) -> Vec<&'a str> {
        let mut session_args = Vec::with_capacity(args.len() + 2);
        if session_name != crate::session::DEFAULT_SESSION_NAME {
            session_args.extend(["--session", session_name]);
        }
        session_args.extend_from_slice(args);
        session_args
    }

    fn exists_command(&self) -> String {
        match self {
            Self::PosixShellPath(path) => format!("test -x {path}"),
            Self::WindowsPath(path) => windows_powershell_script_command(&format!(
                "if ($null -ne (Get-Command {} -CommandType Application -ErrorAction SilentlyContinue)) {{ exit 0 }}; exit 1",
                crate::platform::quote_powershell_arg(path)
            )),
        }
    }

    fn status_client_command(&self) -> String {
        match self {
            Self::PosixShellPath(path) => format!(
                "test -x {path} && {}",
                self.command(&["status", "client", "--json"])
            ),
            Self::WindowsPath(_) => self.command(&["status", "client", "--json"]),
        }
    }

    fn bridge_command(&self, session_name: &str) -> String {
        let args = Self::session_args(session_name, &["remote-client-bridge"]);
        match self {
            Self::PosixShellPath(_) => {
                posix_remote_output_command(&format!("exec {}", self.command(&args)))
            }
            Self::WindowsPath(path) => {
                windows_powershell_streaming_application_command(path, &args)
            }
        }
    }

    fn saved_bridge_command(&self, session_name: &str) -> String {
        let args = Self::session_args(session_name, &["remote-client-bridge"]);
        match self {
            Self::PosixShellPath(_) => format!("exec {} </dev/null", self.command(&args)),
            Self::WindowsPath(path) => {
                windows_powershell_streaming_application_command(path, &args)
            }
        }
    }

    fn live_handoff_command(&self, session_name: &str, protocol: u32, version: &str) -> String {
        let protocol = protocol.to_string();
        match self {
            Self::PosixShellPath(path) => format!(
                "{} --import-exe {path} --expected-protocol {} --expected-version {}",
                self.session_command(session_name, &["server", "live-handoff"]),
                shell_quote(&protocol),
                shell_quote(version),
            ),
            Self::WindowsPath(path) => self.session_command(
                session_name,
                &[
                    "server",
                    "live-handoff",
                    "--import-exe",
                    path,
                    "--expected-protocol",
                    &protocol,
                    "--expected-version",
                    version,
                ],
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct RemoteHerdr {
    install_suffix: String,
    executable: RemoteExecutable,
    platform: RemotePlatform,
}

impl RemoteHerdr {
    fn for_platform(platform: RemotePlatform) -> Self {
        let (install_suffix, executable) = if platform.is_windows() {
            (
                String::new(),
                RemoteExecutable::WindowsPath("herdr.exe".to_string()),
            )
        } else {
            let install_suffix = ".local/bin/herdr".to_string();
            let shell_path = format!("\"$HOME/{install_suffix}\"");
            (install_suffix, RemoteExecutable::PosixShellPath(shell_path))
        };
        Self {
            install_suffix,
            executable,
            platform,
        }
    }

    fn with_shell_path(mut self, shell_path: String) -> Self {
        self.executable = RemoteExecutable::PosixShellPath(shell_path);
        self
    }
}

fn windows_powershell_application_script(path: &str, args: &[&str]) -> String {
    let mut script = format!("& {}", crate::platform::quote_powershell_arg(path));
    for arg in args {
        script.push(' ');
        script.push_str(&crate::platform::quote_powershell_arg(arg));
    }
    script.push_str("; exit $LASTEXITCODE");
    script
}

fn windows_powershell_streaming_application_command(path: &str, args: &[&str]) -> String {
    let command_line = args
        .iter()
        .map(|arg| crate::platform::quote_windows_command_line_arg(arg))
        .collect::<Vec<_>>()
        .join(" ");
    windows_powershell_script_command(&format!(
        "$process = Start-Process -FilePath {} -ArgumentList {} -NoNewWindow -Wait -PassThru -ErrorAction Stop; exit $process.ExitCode",
        crate::platform::quote_powershell_arg(path),
        crate::platform::quote_powershell_arg(&command_line),
    ))
}

fn posix_remote_output_command(command: &str) -> String {
    format!("printf '\n%s\n' '{REMOTE_OUTPUT_READY_MARKER}'\n{command}")
}

fn windows_powershell_script_command(script: &str) -> String {
    let script = format!(
        "[Console]::Out.WriteLine(); [Console]::Out.WriteLine('{REMOTE_OUTPUT_READY_MARKER}'); [Console]::Out.Flush(); {script}"
    );
    let utf16 = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    format!("powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {encoded}")
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RemoteAssetRef {
    Url(String),
    Object { url: String, sha256: Option<String> },
}

impl RemoteAssetRef {
    fn url(&self) -> &str {
        match self {
            Self::Url(url) => url,
            Self::Object { url, .. } => url,
        }
    }

    fn sha256(&self) -> Option<&str> {
        match self {
            Self::Url(_) => None,
            Self::Object { sha256, .. } => {
                sha256.as_deref().filter(|value| !value.trim().is_empty())
            }
        }
    }
}

#[derive(Deserialize)]
struct RemoteUpdateManifest {
    version: String,
    protocol: Option<u32>,
    assets: BTreeMap<String, RemoteAssetRef>,
    #[serde(default)]
    sha256: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_remote_manifest_releases")]
    releases: BTreeMap<String, RemoteReleaseMetadata>,
}

#[derive(Deserialize)]
struct RemoteReleaseMetadata {
    protocol: Option<u32>,
    #[serde(default)]
    assets: BTreeMap<String, RemoteAssetRef>,
    #[serde(default)]
    sha256: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct RemotePreviewManifest {
    build_id: String,
    protocol: u32,
    assets: BTreeMap<String, RemoteAssetRef>,
    #[serde(default)]
    builds: BTreeMap<String, RemotePreviewBuildMetadata>,
}

#[derive(Deserialize)]
struct RemotePreviewBuildMetadata {
    protocol: u32,
    assets: BTreeMap<String, RemoteAssetRef>,
}

fn deserialize_remote_manifest_releases<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, RemoteReleaseMetadata>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::Object(object)) => object
            .into_iter()
            .filter_map(|(version, release)| {
                serde_json::from_value::<RemoteReleaseMetadata>(release)
                    .ok()
                    .map(|metadata| (version, metadata))
            })
            .collect(),
        _ => BTreeMap::new(),
    })
}

impl RemoteUpdateManifest {
    fn release_for_version(&self, version: &str) -> Option<RemoteManifestReleaseRef<'_>> {
        if self.version.trim_start_matches('v') == version {
            return Some(RemoteManifestReleaseRef {
                protocol: self.protocol,
                assets: &self.assets,
                sha256: &self.sha256,
            });
        }

        self.releases.get(version).and_then(|release| {
            (!release.assets.is_empty()).then_some(RemoteManifestReleaseRef {
                protocol: release.protocol,
                assets: &release.assets,
                sha256: &release.sha256,
            })
        })
    }
}

#[derive(Clone, Copy)]
struct RemoteManifestReleaseRef<'a> {
    protocol: Option<u32>,
    assets: &'a BTreeMap<String, RemoteAssetRef>,
    sha256: &'a BTreeMap<String, String>,
}

fn current_version() -> String {
    crate::build_info::version()
}

fn current_channel() -> &'static str {
    crate::build_info::channel()
}

struct InstallSource {
    path: PathBuf,
    temporary_dir: Option<PathBuf>,
}

struct RemoteReleaseAsset {
    url: String,
    sha256: Option<String>,
}

pub(super) struct PreparedRemoteHerdr {
    pub(super) remote_herdr: RemoteHerdr,
    stop_after_install_approved: bool,
}

#[derive(Clone)]
pub(super) struct ManagedSshOptions {
    config_path: PathBuf,
    control_path: Option<PathBuf>,
}

struct ManagedSshConfig {
    options: ManagedSshOptions,
}

impl Drop for ManagedSshConfig {
    fn drop(&mut self) {
        if let Some(dir) = self.options.config_path.parent() {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

pub(super) struct RemoteSsh {
    target: String,
    session_name: String,
    managed_config: Option<ManagedSshConfig>,
    noninteractive: bool,
}

impl RemoteSsh {
    fn new(target: String, manage_ssh_config: bool, session_name: String) -> Self {
        let managed_config = if manage_ssh_config {
            write_managed_ssh_config()
                .inspect_err(|err| {
                    tracing::debug!(%err, "could not write managed ssh config; using plain ssh");
                })
                .ok()
        } else {
            None
        };

        Self {
            target,
            session_name,
            managed_config,
            noninteractive: false,
        }
    }

    pub(super) fn new_noninteractive(target: String) -> Self {
        Self {
            target,
            session_name: crate::session::DEFAULT_SESSION_NAME.into(),
            managed_config: None,
            noninteractive: true,
        }
    }

    fn target(&self) -> &str {
        &self.target
    }

    fn destination(&self) -> String {
        format!("{} (session {})", self.target, self.session_name)
    }

    pub(super) fn options(&self) -> Option<&ManagedSshOptions> {
        self.managed_config.as_ref().map(|config| &config.options)
    }

    fn command(&self) -> Command {
        let mut command = self.base_command();
        if self.noninteractive {
            apply_noninteractive_ssh_options(&mut command);
        }
        command.arg("-T").arg(&self.target);
        command
    }

    fn base_command(&self) -> Command {
        let mut command = Command::new("ssh");
        apply_managed_ssh_options(&mut command, self.options());
        command
    }

    fn sh_output(&self, script: &str) -> io::Result<Output> {
        let script = posix_remote_output_command(script);
        let mut child = self
            .command()
            .arg("/bin/sh -s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let write_result = if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(script.as_bytes())
        } else {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ssh bootstrap stdin missing",
            ))
        };
        let output = if self.noninteractive {
            wait_with_output_timeout(child, NONINTERACTIVE_SSH_COMMAND_TIMEOUT)?
        } else {
            child.wait_with_output()?
        };
        write_result?;
        normalize_remote_output(output)
    }

    fn framed_user_shell_output(&self, remote_command: &str) -> io::Result<Output> {
        let mut command = self.command();
        command
            // Windows OpenSSH can still read the console with stdin redirected to NUL.
            .arg("-n")
            .arg(remote_command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = if self.noninteractive {
            wait_with_output_timeout(command.spawn()?, NONINTERACTIVE_SSH_COMMAND_TIMEOUT)
        } else {
            command.output()
        }?;
        normalize_remote_output(output)
    }

    fn posix_user_shell_output(&self, remote_command: &str) -> io::Result<Output> {
        self.framed_user_shell_output(&posix_remote_output_command(remote_command))
    }

    fn shell_output(&self, platform: &RemotePlatform, remote_command: &str) -> io::Result<Output> {
        if platform.is_windows() {
            self.framed_user_shell_output(remote_command)
        } else {
            self.sh_output(remote_command)
        }
    }

    fn install_herdr(&self, remote_herdr: &RemoteHerdr, source_path: &Path) -> io::Result<()> {
        let output = self.sh_output(&remote_install_prepare_script(remote_herdr))?;
        if !output.status.success() {
            return Err(command_failed("remote install preparation failed", &output));
        }
        let (tmp_path, dest_path) = parse_remote_install_paths(&output.stdout)?;

        let mut child = self
            .command()
            .arg(remote_install_stream_command(&tmp_path))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|err| {
                io::Error::new(err.kind(), format!("failed to start ssh install: {err}"))
            })?;

        let mut source = File::open(source_path)?;
        let copy_result = if let Some(mut stdin) = child.stdin.take() {
            io::copy(&mut source, &mut stdin).map(|_| ())
        } else {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ssh install stdin missing",
            ))
        };
        let status = child.wait()?;
        copy_result?;

        if status.success() {
            let output = self.sh_output(&remote_install_commit_script(&tmp_path, &dest_path))?;
            if output.status.success() {
                Ok(())
            } else {
                Err(command_failed("remote install commit failed", &output))
            }
        } else {
            Err(io::Error::other(format!(
                "remote install exited with {status}"
            )))
        }
    }
}

fn normalize_remote_output(mut output: Output) -> io::Result<Output> {
    normalize_remote_stdout(&mut output.stdout, output.status.success())?;
    Ok(output)
}

fn normalize_remote_stdout(stdout: &mut Vec<u8>, command_succeeded: bool) -> io::Result<()> {
    let consumed = {
        let mut reader = io::Cursor::new(stdout.as_slice());
        match discard_remote_output_preamble(&mut reader) {
            Ok(()) => reader.position() as usize,
            Err(_) if !command_succeeded => return Ok(()),
            Err(err) => return Err(err),
        }
    };
    stdout.drain(..consumed);
    Ok(())
}

fn remote_install_prepare_script(remote_herdr: &RemoteHerdr) -> String {
    format!(
        r#"set -eu
dest="$HOME/{install_suffix}"
dir="${{dest%/*}}"
mkdir -p "$dir"
tmp="${{dest}}.tmp.$$"
printf '%s\0%s\0' "$tmp" "$dest"
"#,
        install_suffix = remote_herdr.install_suffix
    )
}

fn parse_remote_install_paths(stdout: &[u8]) -> io::Result<(String, String)> {
    let mut parts = stdout.split(|byte| *byte == 0);
    let tmp_path = parts.next().unwrap_or_default();
    let dest_path = parts.next().unwrap_or_default();
    if tmp_path.is_empty() || dest_path.is_empty() {
        return Err(io::Error::other(
            "remote install preparation did not return destination paths",
        ));
    }
    let tmp_path = String::from_utf8(tmp_path.to_vec()).map_err(|err| {
        io::Error::other(format!(
            "remote install temporary path is not valid UTF-8: {err}"
        ))
    })?;
    let dest_path = String::from_utf8(dest_path.to_vec()).map_err(|err| {
        io::Error::other(format!(
            "remote install destination path is not valid UTF-8: {err}"
        ))
    })?;
    Ok((tmp_path, dest_path))
}

fn remote_install_stream_command(tmp_path: &str) -> String {
    format!("tee {}", shell_quote(tmp_path))
}

fn remote_install_commit_script(tmp_path: &str, dest_path: &str) -> String {
    format!(
        "set -eu\nchmod 755 {tmp_path}\nmv {tmp_path} {dest_path}\n",
        tmp_path = shell_quote(tmp_path),
        dest_path = shell_quote(dest_path)
    )
}

impl Drop for RemoteSsh {
    fn drop(&mut self) {
        let Some(_options) = self
            .managed_config
            .as_ref()
            .map(|config| &config.options)
            .filter(|options| options.control_path.is_some())
        else {
            return;
        };

        let _ = self
            .base_command()
            .arg("-O")
            .arg("exit")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg(&self.target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn apply_noninteractive_ssh_options(command: &mut Command) {
    command
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("NumberOfPasswordPrompts=0")
        .arg("-o")
        .arg("StrictHostKeyChecking=yes")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg("-o")
        .arg("ConnectionAttempts=1")
        .arg("-o")
        .arg("ServerAliveInterval=15")
        .arg("-o")
        .arg("ServerAliveCountMax=4");
}

fn apply_managed_ssh_options(command: &mut Command, options: Option<&ManagedSshOptions>) {
    let Some(options) = options else {
        return;
    };

    command.arg("-F").arg(&options.config_path);
    if let Some(control_path) = &options.control_path {
        command
            .arg("-S")
            .arg(control_path)
            .arg("-o")
            .arg("ControlMaster=auto")
            .arg("-o")
            .arg("ControlPersist=yes");
    }
}

impl InstallSource {
    fn persistent(path: PathBuf) -> Self {
        Self {
            path,
            temporary_dir: None,
        }
    }

    fn temporary(path: PathBuf, temporary_dir: PathBuf) -> Self {
        Self {
            path,
            temporary_dir: Some(temporary_dir),
        }
    }

    fn cleanup(&self) {
        if let Some(dir) = &self.temporary_dir {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

pub(super) fn prepare_remote_herdr(
    ssh: &RemoteSsh,
    live_handoff_enabled: bool,
    require_surface_interest: bool,
) -> io::Result<PreparedRemoteHerdr> {
    let platform = detect_remote_platform(ssh)?;
    let remote_herdr = RemoteHerdr::for_platform(platform);
    if remote_herdr.platform.is_windows() {
        return prepare_windows_remote_herdr(ssh, remote_herdr, require_surface_interest);
    }
    let override_binary = remote_binary_override_path()?;
    let remote_binary_candidates = remote_binary_candidates(ssh, &remote_herdr)?;

    if override_binary.is_none() {
        for candidate in &remote_binary_candidates {
            if remote_binary_supports_endpoint_requirement(ssh, candidate, require_surface_interest)
                .unwrap_or(false)
            {
                return Ok(PreparedRemoteHerdr {
                    remote_herdr: candidate.clone(),
                    stop_after_install_approved: false,
                });
            }
        }
        if remote_binary_supports_endpoint_requirement(
            ssh,
            &remote_herdr,
            require_surface_interest,
        )? {
            return Ok(PreparedRemoteHerdr {
                remote_herdr,
                stop_after_install_approved: false,
            });
        }
    }

    let mut stop_after_install_approved = false;
    if let Some(status_probe_herdr) = remote_binary_candidates.first().or_else(|| {
        remote_binary_exists(ssh, &remote_herdr)
            .ok()
            .and_then(|exists| exists.then_some(&remote_herdr))
    }) {
        stop_after_install_approved = confirm_remote_install_with_running_server(
            ssh,
            status_probe_herdr,
            live_handoff_enabled,
            require_surface_interest,
        )?;
    }
    if !stop_after_install_approved {
        confirm_remote_install(
            &ssh.destination(),
            &remote_herdr,
            &install_source_description(&remote_herdr.platform, override_binary.as_deref()),
        )?;
    }
    let source = resolve_install_source(&remote_herdr.platform, override_binary)?;
    let install_result = ssh.install_herdr(&remote_herdr, &source.path);
    source.cleanup();
    install_result?;

    if !remote_binary_supports_endpoint_requirement(ssh, &remote_herdr, require_surface_interest)? {
        return Err(io::Error::other(format!(
            "installed remote herdr at {}, but it does not support saved SSH endpoint federation",
            remote_herdr.executable.display()
        )));
    }
    warn_if_remote_bin_not_on_path(ssh)?;

    Ok(PreparedRemoteHerdr {
        remote_herdr,
        stop_after_install_approved,
    })
}

pub(super) fn find_installed_remote_herdr(ssh: &RemoteSsh) -> io::Result<RemoteHerdr> {
    let platform = detect_remote_platform(ssh)?;
    let remote_herdr = RemoteHerdr::for_platform(platform);
    if remote_herdr.platform.is_windows() {
        return prepare_windows_remote_herdr(ssh, remote_herdr, true)
            .map(|prepared| prepared.remote_herdr);
    }
    let candidates = remote_binary_candidates(ssh, &remote_herdr)?;
    for candidate in candidates {
        if remote_binary_supports_endpoint_requirement(ssh, &candidate, true)? {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "matching Herdr is not ready on {}; run `herdr --remote {}` interactively to install or update it",
            ssh.target(),
            ssh.target()
        ),
    ))
}

fn prepare_windows_remote_herdr(
    ssh: &RemoteSsh,
    remote_herdr: RemoteHerdr,
    require_surface_interest: bool,
) -> io::Result<PreparedRemoteHerdr> {
    if !remote_binary_exists(ssh, &remote_herdr)? {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "herdr.exe is not installed or is not on PATH on Windows host {}; install a compatible Windows package with remote host support and retry",
                ssh.target()
            ),
        ));
    }
    if !remote_binary_supports_endpoint_requirement(ssh, &remote_herdr, require_surface_interest)? {
        return Err(io::Error::other(format!(
            "herdr.exe on Windows host {} does not support saved SSH endpoint federation; install a compatible Windows package with remote host support and retry",
            ssh.target()
        )));
    }

    Ok(PreparedRemoteHerdr {
        remote_herdr,
        stop_after_install_approved: false,
    })
}

fn detect_remote_platform(ssh: &RemoteSsh) -> io::Result<RemotePlatform> {
    let output = ssh.sh_output("uname -s\nuname -m\n")?;
    let mut windows_uname_hint = false;
    let posix_error = if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut lines = stdout.lines();
        let os = lines.next().unwrap_or_default();
        let arch = lines.next().unwrap_or_default();
        if let Some(platform) = RemotePlatform::from_uname(os, arch) {
            return Ok(platform);
        }
        windows_uname_hint = looks_like_windows_uname(os);
        io::Error::other(format!(
            "unsupported remote platform: {} {}",
            os.trim(),
            arch.trim()
        ))
    } else {
        command_failed("remote platform detection failed", &output)
    };

    if !should_try_windows_probe(
        output.status.success(),
        output.status.code(),
        windows_uname_hint,
    ) {
        return Err(posix_error);
    }
    let windows_output = match ssh.framed_user_shell_output(&windows_platform_probe_command()) {
        Ok(output) if output.status.success() => output,
        _ => return Err(posix_error),
    };
    match parse_windows_platform_probe(&String::from_utf8_lossy(&windows_output.stdout)) {
        Ok(Some(platform)) => Ok(platform),
        Ok(None) => Err(posix_error),
        Err(message) => Err(io::Error::other(message)),
    }
}

fn should_try_windows_probe(
    success: bool,
    exit_code: Option<i32>,
    windows_uname_hint: bool,
) -> bool {
    (success && windows_uname_hint) || (!success && exit_code.is_some_and(|code| code != 255))
}

fn looks_like_windows_uname(os: &str) -> bool {
    let os = os.trim().to_ascii_uppercase();
    ["MINGW", "MSYS", "CYGWIN"]
        .iter()
        .any(|prefix| os.starts_with(prefix))
}

fn windows_platform_probe_command() -> String {
    windows_powershell_script_command(
        "$arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }; [Console]::Out.WriteLine('herdr-windows:' + $arch); exit 0",
    )
}

fn parse_windows_platform_probe(stdout: &str) -> Result<Option<RemotePlatform>, String> {
    let Some(arch) = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("herdr-windows:"))
    else {
        return Ok(None);
    };
    match arch.trim().to_ascii_uppercase().as_str() {
        "AMD64" | "X86_64" => Ok(Some(RemotePlatform {
            os: "windows",
            arch: "x86_64",
        })),
        arch => Err(format!("unsupported remote platform: Windows {arch}")),
    }
}

fn remote_binary_candidates(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<Vec<RemoteHerdr>> {
    let mut candidates = Vec::new();

    if let Some(path_candidate) = remote_binary_on_path_any(ssh, remote_herdr)? {
        push_if_new_remote_binary_candidate(&mut candidates, path_candidate);
    }

    let output = ssh.sh_output(&known_remote_binary_candidate_script(
        &remote_herdr.platform,
    ))?;
    if !output.status.success() {
        return Err(command_failed("remote binary discovery failed", &output));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for candidate in remote_herdrs_from_path_discovery(remote_herdr, &stdout) {
        push_if_new_remote_binary_candidate(&mut candidates, candidate);
    }

    Ok(candidates)
}

fn push_if_new_remote_binary_candidate(candidates: &mut Vec<RemoteHerdr>, candidate: RemoteHerdr) {
    if !candidates
        .iter()
        .any(|existing| existing.executable == candidate.executable)
    {
        candidates.push(candidate);
    }
}

fn known_remote_binary_candidate_script(platform: &RemotePlatform) -> String {
    let mut script = String::from(
        r#"home=${HOME:-}
user=${USER:-}
version="#,
    );
    script.push_str(&shell_quote(&current_version()));
    script.push_str(
        r#"
emit() {
    path=$1
    if [ -n "$path" ] && [ -x "$path" ]; then
        printf '%s\n' "$path"
    fi
}
if [ -n "$home" ]; then
    emit "$home/.local/bin/herdr"
fi
"#,
    );
    if platform.os == "macos" {
        script.push_str(
            r#"    emit "/opt/homebrew/bin/herdr"
    emit "/usr/local/bin/herdr"
"#,
        );
    } else if platform.os == "linux" {
        script.push_str(
            r#"    emit "/home/linuxbrew/.linuxbrew/bin/herdr"
"#,
        );
    }
    script.push_str(
        r#"if [ -n "$home" ]; then
    emit "$home/.local/share/mise/installs/herdr/$version/bin/herdr"
    emit "$home/.local/share/mise/installs/herdr/$version/herdr"
    emit "$home/.local/share/mise/installs/github-ogulcancelik-herdr/$version/herdr"
    emit "$home/.nix-profile/bin/herdr"
fi
if [ -n "$user" ]; then
    emit "/etc/profiles/per-user/$user/bin/herdr"
fi
emit "/nix/var/nix/profiles/default/bin/herdr"
emit "/run/current-system/sw/bin/herdr"
"#,
    );

    script
}

fn remote_binary_on_path_any(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<Option<RemoteHerdr>> {
    let output = ssh.posix_user_shell_output("command -v herdr")?;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(candidate) = remote_herdr_from_path_discovery(remote_herdr, &stdout) {
            return Ok(Some(candidate));
        }
    }

    // Non-POSIX login shells such as xonsh reject `command -v`; retry through
    // /bin/sh while retaining the login-shell probe for shell-initialized PATHs.
    let output = ssh.sh_output("command -v herdr\n")?;
    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(remote_herdr_from_path_discovery(remote_herdr, &stdout))
}

fn remote_herdrs_from_path_discovery(remote_herdr: &RemoteHerdr, stdout: &str) -> Vec<RemoteHerdr> {
    stdout
        .lines()
        .filter_map(|path| remote_herdr_from_path(remote_herdr, path))
        .collect()
}

fn remote_herdr_from_path_discovery(
    remote_herdr: &RemoteHerdr,
    stdout: &str,
) -> Option<RemoteHerdr> {
    stdout
        .lines()
        .find_map(|path| remote_herdr_from_path(remote_herdr, path))
}

fn remote_herdr_from_path(remote_herdr: &RemoteHerdr, path: &str) -> Option<RemoteHerdr> {
    let path = path.trim();
    if !path.starts_with('/') {
        return None;
    }
    if is_mise_shim_path(path) {
        return None;
    }
    Some(remote_herdr.clone().with_shell_path(shell_quote(path)))
}

fn is_mise_shim_path(path: &str) -> bool {
    path.ends_with("/mise/shims/herdr")
}

fn remote_client_status(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<Option<RemoteClientStatusJson>> {
    let command = remote_herdr.executable.status_client_command();
    let output = ssh.shell_output(&remote_herdr.platform, &command)?;
    if !output.status.success() {
        if output.status.code() == Some(255) {
            return Err(command_failed("remote SSH connection failed", &output));
        }
        return Ok(None);
    }
    Ok(parse_client_status_json(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn remote_binary_supports_endpoint_requirement(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    require_surface_interest: bool,
) -> io::Result<bool> {
    Ok(remote_client_status(ssh, remote_herdr)?
        .is_some_and(|status| status.supports_endpoint_requirement(require_surface_interest)))
}

fn remote_binary_exists(ssh: &RemoteSsh, remote_herdr: &RemoteHerdr) -> io::Result<bool> {
    let command = remote_herdr.executable.exists_command();
    Ok(ssh
        .shell_output(&remote_herdr.platform, &command)?
        .status
        .success())
}

fn remote_binary_override_path() -> io::Result<Option<PathBuf>> {
    let Some(value) = std::env::var_os(REMOTE_BINARY_ENV_VAR) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{REMOTE_BINARY_ENV_VAR} must not be empty"),
        ));
    }

    let path = PathBuf::from(value);
    let metadata = fs::metadata(&path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "failed to inspect {REMOTE_BINARY_ENV_VAR} path {}: {err}",
                path.display()
            ),
        )
    })?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{REMOTE_BINARY_ENV_VAR} path is not a file: {}",
                path.display()
            ),
        ));
    }

    Ok(Some(path))
}

fn install_source_description(platform: &RemotePlatform, override_binary: Option<&Path>) -> String {
    install_source_description_for(
        platform,
        override_binary,
        local_binary_can_seed_remote(platform),
    )
}

fn install_source_description_for(
    platform: &RemotePlatform,
    override_binary: Option<&Path>,
    local_binary_can_seed_remote: bool,
) -> String {
    if let Some(path) = override_binary {
        return format!("{REMOTE_BINARY_ENV_VAR} ({})", path.display());
    }

    if local_binary_can_seed_remote {
        "the current local herdr binary".to_string()
    } else {
        format!(
            "the {} {} asset for {}",
            current_version(),
            current_channel(),
            platform.asset_key()
        )
    }
}

fn resolve_install_source(
    platform: &RemotePlatform,
    override_binary: Option<PathBuf>,
) -> io::Result<InstallSource> {
    if let Some(path) = override_binary {
        return Ok(InstallSource::persistent(path));
    }

    if *platform == RemotePlatform::local() {
        let path = std::env::current_exe()?;
        if !crate::update::is_package_manager_managed_exe_path(&path) {
            return Ok(InstallSource::persistent(path));
        }
    }

    download_release_asset(platform)
}

fn local_binary_can_seed_remote(platform: &RemotePlatform) -> bool {
    if *platform != RemotePlatform::local() {
        return false;
    }

    std::env::current_exe()
        .map(|path| !crate::update::is_package_manager_managed_exe_path(&path))
        .unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteServerStatus {
    Running {
        version: Option<String>,
        endpoint_protocol_generation: Option<u32>,
        surface_interest: bool,
        health_check: bool,
        live_handoff: bool,
        detached_server_daemon: bool,
    },
    NotRunning,
}

impl RemoteServerStatus {
    fn with_endpoint_negotiation(
        mut self,
        negotiation: &crate::client::endpoint::EndpointNegotiation,
    ) -> Self {
        if let Self::Running {
            surface_interest,
            health_check,
            ..
        } = &mut self
        {
            *surface_interest = negotiation.supports_surface_interest();
            *health_check = negotiation.supports_health_check();
        }
        self
    }
}

fn ensure_remote_server_ready(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    stop_after_install_approved: bool,
    live_handoff_enabled: bool,
    require_surface_interest: bool,
) -> io::Result<()> {
    let status = remote_server_status(ssh, remote_herdr, require_surface_interest)?;
    let RemoteServerStatus::Running {
        version,
        endpoint_protocol_generation,
        surface_interest,
        health_check,
        live_handoff,
        detached_server_daemon,
    } = status
    else {
        return Ok(());
    };

    let Some(reason) = remote_server_restart_reason(
        endpoint_protocol_generation,
        detached_server_daemon,
        require_surface_interest,
        surface_interest,
        health_check,
    ) else {
        return Ok(());
    };

    if live_handoff_enabled && live_handoff {
        match live_handoff_remote_server(ssh, remote_herdr) {
            Ok(()) => return Ok(()),
            Err(err) => {
                eprintln!("remote live handoff failed: {err}");
                eprintln!("falling back to remote server restart.");
            }
        }
    }

    if stop_after_install_approved {
        stop_remote_server(ssh, remote_herdr)?;
        return Ok(());
    }

    if confirm_remote_server_stop(&ssh.destination(), version.as_deref(), reason)? {
        stop_remote_server(ssh, remote_herdr)?;
    }
    Ok(())
}

fn confirm_remote_install_with_running_server(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    live_handoff_enabled: bool,
    require_surface_interest: bool,
) -> io::Result<bool> {
    let target = ssh.destination();
    let status = match remote_server_status(ssh, remote_herdr, require_surface_interest) {
        Ok(status) => status,
        Err(err) => {
            if !io::stdin().is_terminal() {
                return Err(io::Error::other(format!(
                    "could not inspect the running remote herdr server on {target} before installing: {err}; run from an interactive terminal to approve updating the remote binary"
                )));
            }
            eprintln!(
                "could not inspect the running remote herdr server on {target} before installing: {err}"
            );
            eprint!("continue installing the remote herdr binary? [y/N] ");
            io::stderr().flush()?;

            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            let answer = answer.trim().to_ascii_lowercase();
            if answer != "y" && answer != "yes" {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "remote herdr install cancelled",
                ));
            }
            return Ok(false);
        }
    };
    let RemoteServerStatus::Running {
        version,
        endpoint_protocol_generation,
        surface_interest,
        health_check,
        live_handoff,
        detached_server_daemon,
    } = &status
    else {
        return Ok(false);
    };
    let plan = remote_install_running_server_plan(
        *endpoint_protocol_generation,
        *detached_server_daemon,
        *surface_interest,
        *health_check,
        *live_handoff,
        live_handoff_enabled,
        require_surface_interest,
    );

    if plan == RemoteInstallRunningServerPlan::KeepRunning {
        if io::stdin().is_terminal() {
            eprintln!("remote herdr server on {target} is already compatible:");
            eprintln!("  server: v{}", version_label(version.as_deref()));
            eprintln!(
                "Herdr will install {} without stopping the running remote server.",
                current_version()
            );
        }
        return Ok(false);
    }

    if !io::stdin().is_terminal() {
        match plan {
            RemoteInstallRunningServerPlan::LiveHandoff => return Ok(false),
            RemoteInstallRunningServerPlan::StopRequired(_) => {
                return Err(io::Error::other(format!(
                    "remote herdr server on {target} is running v{}; run from an interactive terminal to approve stopping it for the update",
                    version_label(version.as_deref())
                )));
            }
            RemoteInstallRunningServerPlan::KeepRunning => return Ok(false),
        }
    }

    if plan == RemoteInstallRunningServerPlan::LiveHandoff {
        eprintln!("remote herdr server on {target} is currently running:");
        eprintln!("  server: v{}", version_label(version.as_deref()));
        eprintln!(
            "Herdr will install {} and hand off live pane processes to the prepared server.",
            current_version()
        );
        return Ok(false);
    }

    eprintln!("remote herdr server on {target} is currently running:");
    eprintln!("  server: v{}", version_label(version.as_deref()));
    eprintln!(
        "To complete the remote update, Herdr must stop the running remote server after installing."
    );
    eprintln!("This stops active remote pane processes, including shells, agents, dev servers, and tests.");
    eprintln!();
    eprint!(
        "Install {} and stop the remote server now? [y/N] ",
        current_version()
    );
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    if answer != "y" && answer != "yes" {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote herdr install cancelled",
        ));
    }

    Ok(true)
}

fn remote_server_status(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    require_surface_interest: bool,
) -> io::Result<RemoteServerStatus> {
    let command = remote_herdr
        .executable
        .session_command(&ssh.session_name, &["status", "server", "--json"]);
    let output = ssh.shell_output(&remote_herdr.platform, &command)?;
    if !output.status.success() {
        return Err(command_failed("remote server status failed", &output));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let status = parse_remote_server_status_json(stdout.trim())?;
    if require_surface_interest
        && matches!(
            status,
            RemoteServerStatus::Running {
                endpoint_protocol_generation: Some(
                    crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION
                ),
                surface_interest: true,
                health_check: true,
                ..
            }
        )
    {
        // Older status helpers omit newer capabilities. Ask the live endpoint rather than
        // assuming that the installed binary and the running daemon support the same features.
        let negotiation = probe_remote_endpoint(ssh, remote_herdr)?;
        return Ok(status.with_endpoint_negotiation(&negotiation));
    }
    Ok(status)
}

fn probe_remote_endpoint(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<crate::client::endpoint::EndpointNegotiation> {
    let path = local_forward_socket_path(ssh.target(), &ssh.session_name);
    let bridge = SshStdioBridge::start(
        ssh.target.clone(),
        remote_herdr.clone(),
        path.clone(),
        ssh.session_name.clone(),
        None,
        true,
    )?;
    let mut stream = crate::ipc::connect_local_stream(&path)?;
    // Use the saved client's noninteractive path. This metadata-only attachment never
    // acquires a surface or sends pane input.
    match crate::client::probe_endpoint_negotiation(&mut stream) {
        Ok(negotiation) => Ok(negotiation),
        Err(probe_error) => Err(bridge.reported_failure().unwrap_or(probe_error)),
    }
}

#[derive(Debug, Deserialize)]
struct RemoteClientStatusJson {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    protocol: Option<u32>,
    #[serde(default)]
    endpoint_protocol_generation: Option<u32>,
    #[serde(default)]
    endpoint_capabilities: Vec<String>,
}

impl RemoteClientStatusJson {
    fn supports_endpoint_requirement(&self, require_surface_interest: bool) -> bool {
        self.endpoint_protocol_generation
            == Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION)
            && (!require_surface_interest
                || [
                    crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY,
                    crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY,
                    crate::protocol::endpoint::HEALTH_CHECK_CAPABILITY,
                ]
                .iter()
                .all(|required| {
                    self.endpoint_capabilities
                        .iter()
                        .any(|capability| capability == required)
                }))
    }
}

#[derive(Debug, Deserialize)]
struct RemoteServerStatusJson {
    running: bool,
    version: Option<String>,
    capabilities: Option<RemoteServerCapabilitiesJson>,
}

#[derive(Debug, Deserialize)]
struct RemoteServerCapabilitiesJson {
    live_handoff: bool,
    #[serde(default)]
    detached_server_daemon: bool,
    #[serde(default)]
    endpoint_protocol_generation: Option<u32>,
    #[serde(default)]
    surface_interest: bool,
    #[serde(default)]
    health_check: bool,
}

fn parse_client_status_json(status: &str) -> Option<RemoteClientStatusJson> {
    status
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<RemoteClientStatusJson>(line).ok())
        .find(|status| {
            status.version.is_some()
                || status.protocol.is_some()
                || status.endpoint_protocol_generation.is_some()
                || !status.endpoint_capabilities.is_empty()
        })
}

fn parse_remote_server_status_json(status: &str) -> io::Result<RemoteServerStatus> {
    let parsed: RemoteServerStatusJson = serde_json::from_str(status).map_err(|err| {
        io::Error::other(format!(
            "could not parse remote server status JSON from `{status}`: {err}"
        ))
    })?;
    if !parsed.running {
        return Ok(RemoteServerStatus::NotRunning);
    }

    let capabilities = parsed.capabilities;

    Ok(RemoteServerStatus::Running {
        version: parsed.version,
        endpoint_protocol_generation: capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.endpoint_protocol_generation),
        surface_interest: capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.surface_interest),
        health_check: capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.health_check),
        live_handoff: capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.live_handoff),
        detached_server_daemon: capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.detached_server_daemon),
    })
}

fn confirm_remote_server_stop(
    target: &str,
    version: Option<&str>,
    reason: RemoteServerRestartReason,
) -> io::Result<bool> {
    let required_upgrade = matches!(
        reason,
        RemoteServerRestartReason::EndpointProtocol
            | RemoteServerRestartReason::SurfaceInterest
            | RemoteServerRestartReason::HealthCheck
    );
    if !io::stdin().is_terminal() {
        if required_upgrade {
            return Err(io::Error::other(format!(
                "remote herdr server on {target} needs one final update before this client can attach; run from an interactive terminal to approve updating it"
            )));
        }

        eprintln!(
            "remote herdr server on {target} is still running v{}; it will use {} after it restarts.",
            version_label(version),
            current_version()
        );
        return Ok(false);
    }

    eprintln!("remote herdr server on {target} is currently running:");
    eprintln!("  server: v{}", version_label(version));
    eprintln!("  prepared binary: {}", current_version());
    eprintln!();

    match reason {
        RemoteServerRestartReason::EndpointProtocol => {
            eprintln!(
                "the remote server predates Herdr's stable endpoint protocol and must update before this client can attach."
            );
        }
        RemoteServerRestartReason::SurfaceInterest => {
            eprintln!(
                "the remote server must restart before it can join saved SSH endpoint federation."
            );
        }
        RemoteServerRestartReason::HealthCheck => {
            eprintln!("the remote server must restart to enable saved SSH endpoint health checks.");
        }
        RemoteServerRestartReason::DaemonDetach => {
            eprintln!(
                "the remote server was started by a herdr build that may not survive SSH connection loss. restart it so network drops disconnect only this client."
            );
        }
    }

    eprintln!("This stops active remote pane processes, including shells, agents, dev servers, and tests.");
    let prompt = if required_upgrade {
        "stop and update the remote server, then continue attaching? [y/N] "
    } else {
        "restart the remote server now? [y/N] "
    };
    eprint!("{prompt}");
    io::stderr().flush()?;

    if read_remote_confirmation(&mut io::stdin().lock(), false)? {
        return Ok(true);
    }
    if required_upgrade {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote herdr server stop cancelled",
        ));
    }

    Ok(false)
}

fn live_handoff_remote_server(ssh: &RemoteSsh, remote_herdr: &RemoteHerdr) -> io::Result<()> {
    let status = remote_client_status(ssh, remote_herdr)?.ok_or_else(|| {
        io::Error::other("could not inspect the prepared remote herdr binary before live handoff")
    })?;
    let protocol = status.protocol.ok_or_else(|| {
        io::Error::other("prepared remote herdr did not report its private protocol")
    })?;
    let version = status
        .version
        .filter(|version| !version.is_empty())
        .ok_or_else(|| io::Error::other("prepared remote herdr did not report its version"))?;
    let command =
        remote_herdr
            .executable
            .live_handoff_command(&ssh.session_name, protocol, &version);
    let output = ssh.shell_output(&remote_herdr.platform, &command)?;
    if !output.status.success() {
        return Err(command_failed("remote server live handoff failed", &output));
    }

    eprintln!(
        "handed off the remote herdr server on {}; reconnecting to the prepared server.",
        ssh.target()
    );
    Ok(())
}

fn stop_remote_server(ssh: &RemoteSsh, remote_herdr: &RemoteHerdr) -> io::Result<()> {
    let command = remote_herdr
        .executable
        .session_command(&ssh.session_name, &["server", "stop"]);
    let output = ssh.shell_output(&remote_herdr.platform, &command)?;
    if !output.status.success() {
        return Err(command_failed("remote server stop failed", &output));
    }

    wait_for_remote_server_shutdown(ssh, remote_herdr)?;
    eprintln!(
        "stopped the remote herdr server on {}; it will restart when the remote client bridge attaches.",
        ssh.target()
    );
    Ok(())
}

fn wait_for_remote_server_shutdown(ssh: &RemoteSsh, remote_herdr: &RemoteHerdr) -> io::Result<()> {
    let deadline = Instant::now() + REMOTE_SERVER_SHUTDOWN_CONFIRM_TIMEOUT;
    loop {
        if remote_server_status(ssh, remote_herdr, false)? == RemoteServerStatus::NotRunning {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "shutdown was requested, but the old remote herdr server on {target} is still responding after {} seconds",
                    REMOTE_SERVER_SHUTDOWN_CONFIRM_TIMEOUT.as_secs(),
                    target = ssh.target()
                ),
            ));
        }
        thread::sleep(REMOTE_SERVER_SHUTDOWN_POLL_INTERVAL);
    }
}

fn version_label(version: Option<&str>) -> &str {
    version.unwrap_or("unknown")
}

fn warn_if_remote_bin_not_on_path(ssh: &RemoteSsh) -> io::Result<()> {
    let output = ssh.posix_user_shell_output("command -v herdr")?;
    if output.status.success()
        && remote_shell_resolves_managed_install(&String::from_utf8_lossy(&output.stdout))
    {
        return Ok(());
    }

    eprintln!(
        "herdr: installed remote binary to ~/.local/bin/herdr, but the remote shell does not resolve `herdr` to that path"
    );
    Ok(())
}

fn remote_shell_resolves_managed_install(stdout: &str) -> bool {
    stdout
        .lines()
        .next()
        .map(str::trim)
        .is_some_and(|path| path.ends_with("/.local/bin/herdr"))
}

fn download_release_asset(platform: &RemotePlatform) -> io::Result<InstallSource> {
    let asset_key = platform.asset_key();
    let asset = remote_release_asset(&asset_key)?;

    let dir = private_download_dir(&asset_key)?;
    let path = dir.join("herdr.tmp");
    let status = crate::noninteractive_process::curl_command()
        .args(["-sfL", "--max-time", "120", "-o"])
        .arg(&path)
        .arg(&asset.url)
        .status()
        .map_err(|err| io::Error::new(err.kind(), format!("download failed: {err}")))?;
    if !status.success() {
        let _ = fs::remove_dir_all(&dir);
        return Err(io::Error::other("download failed"));
    }
    if let Some(expected) = &asset.sha256 {
        if let Err(err) = crate::checksum::verify_sha256(&path, expected) {
            let _ = fs::remove_dir_all(&dir);
            return Err(io::Error::new(
                err.kind(),
                format!("downloaded remote asset checksum verification failed: {err}"),
            ));
        }
    }

    Ok(InstallSource::temporary(path, dir))
}

fn fetch_remote_manifest(url: &str) -> io::Result<Vec<u8>> {
    let output = crate::noninteractive_process::curl_command()
        .args([
            "-sfL",
            "--retry",
            "3",
            "--connect-timeout",
            "10",
            "--max-time",
            "20",
            url,
        ])
        .output()
        .map_err(|err| io::Error::new(err.kind(), format!("curl failed: {err}")))?;
    if !output.status.success() {
        return Err(command_failed("failed to fetch update manifest", &output));
    }
    Ok(output.stdout)
}

fn remote_asset_info(asset: &RemoteAssetRef) -> RemoteReleaseAsset {
    RemoteReleaseAsset {
        url: asset.url().to_string(),
        sha256: asset.sha256().map(str::to_string),
    }
}

fn preview_assets_for_build<'a>(
    manifest: &'a RemotePreviewManifest,
    build_id: &str,
) -> io::Result<(u32, &'a BTreeMap<String, RemoteAssetRef>)> {
    if manifest.build_id == build_id {
        return Ok((manifest.protocol, &manifest.assets));
    }
    let build = manifest.builds.get(build_id).ok_or_else(|| {
        io::Error::other(format!(
            "preview manifest no longer includes build {build_id}; run `herdr update` locally or set {REMOTE_BINARY_ENV_VAR}=target/release/herdr"
        ))
    })?;
    Ok((build.protocol, &build.assets))
}

fn remote_release_asset(asset_key: &str) -> io::Result<RemoteReleaseAsset> {
    if crate::build_info::is_preview() {
        let build_id = crate::build_info::build_id().ok_or_else(|| {
            io::Error::other("preview client has no build id; set HERDR_REMOTE_BINARY or install Herdr on the remote manually")
        })?;
        let manifest_bytes = fetch_remote_manifest(PREVIEW_UPDATE_MANIFEST_URL)?;
        let manifest: RemotePreviewManifest =
            serde_json::from_slice(&manifest_bytes).map_err(|err| {
                io::Error::other(format!("failed to parse preview manifest JSON: {err}"))
            })?;
        let (protocol, assets) = preview_assets_for_build(&manifest, build_id)?;
        if protocol != CURRENT_PROTOCOL {
            return Err(io::Error::other(format!(
                "preview manifest has build {build_id} protocol {protocol}, but this client needs protocol {CURRENT_PROTOCOL}; set {REMOTE_BINARY_ENV_VAR}=target/release/herdr or install a matching Herdr on the remote host manually"
            )));
        }
        return assets.get(asset_key).map(remote_asset_info).ok_or_else(|| {
            io::Error::other(format!(
                "no {asset_key} binary in the preview manifest for build {build_id}"
            ))
        });
    }

    let current_version = current_version();
    let manifest_bytes = fetch_remote_manifest(STABLE_UPDATE_MANIFEST_URL)?;
    let manifest: RemoteUpdateManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|err| io::Error::other(format!("failed to parse update manifest JSON: {err}")))?;
    let release = manifest.release_for_version(&current_version).ok_or_else(|| {
        io::Error::other(format!(
            "release manifest does not include herdr {current_version}; build herdr for {} or install it there manually",
            asset_key
        ))
    })?;
    if let Some(protocol) = release.protocol {
        if protocol != CURRENT_PROTOCOL {
            return Err(io::Error::other(format!(
                "release manifest has herdr {current_version} protocol {protocol}, but this client needs protocol {CURRENT_PROTOCOL}; set {REMOTE_BINARY_ENV_VAR}=target/release/herdr or install a matching herdr on the remote host manually"
            )));
        }
    }
    let asset = release.assets.get(asset_key).ok_or_else(|| {
        io::Error::other(format!(
            "no {asset_key} binary in the release manifest for herdr {current_version}"
        ))
    })?;
    let mut asset = remote_asset_info(asset);
    asset.sha256 = asset
        .sha256
        .or_else(|| release.sha256.get(asset_key).cloned());
    if asset.sha256.is_none() {
        return Err(io::Error::other(format!(
            "release manifest asset {asset_key} is missing a SHA-256 checksum"
        )));
    }
    Ok(asset)
}

fn private_download_dir(asset_key: &str) -> io::Result<PathBuf> {
    let base = crate::platform::remote_private_temp_base();
    fs::create_dir_all(&base)?;
    for attempt in 0..100 {
        let dir = base.join(format!(
            "herdr-remote-{}-{}-{attempt}",
            std::process::id(),
            asset_key
        ));
        match crate::platform::create_remote_private_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "failed to create private herdr remote download directory",
    ))
}

fn read_remote_confirmation(reader: &mut impl io::BufRead, default: bool) -> io::Result<bool> {
    let mut answer = String::new();
    if reader.read_line(&mut answer)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote setup cancelled",
        ));
    }
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        "" => Ok(default),
        _ => Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote setup cancelled: expected yes or no",
        )),
    }
}

fn confirm_remote_install(
    target: &str,
    remote_herdr: &RemoteHerdr,
    source_description: &str,
) -> io::Result<()> {
    if !io::stdin().is_terminal() {
        return Err(io::Error::other(format!(
            "matching remote herdr {} is not installed at {}; run from an interactive terminal to approve installation",
            current_version(),
            remote_herdr.executable.display()
        )));
    }

    eprintln!(
        "matching herdr {} is not installed on {target} for {}.",
        current_version(),
        remote_herdr.platform.asset_key()
    );
    eprint!(
        "Install {} to {}? [Y/n] ",
        source_description,
        remote_herdr.executable.display()
    );
    io::stderr().flush()?;

    if !read_remote_confirmation(&mut io::stdin().lock(), true)? {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote herdr installation cancelled",
        ));
    }

    Ok(())
}

fn reattach_command(
    program: &str,
    target: &str,
    session_name: &str,
    keybindings: RemoteKeybindings,
    live_handoff: bool,
) -> String {
    let program = crate::platform::remote_reattach_program(program);
    let target = crate::platform::remote_reattach_argument(target);
    let mut command = format!("{program} --remote {target}");
    if keybindings != RemoteKeybindings::Local {
        command.push_str(" --remote-keybindings ");
        command.push_str(keybindings.as_str());
    }
    if live_handoff {
        command.push_str(" --handoff");
    }
    if session_name != crate::session::DEFAULT_SESSION_NAME {
        command.push_str(" --session ");
        command.push_str(&crate::platform::remote_reattach_argument(session_name));
    }
    command
}

fn command_failed(context: &str, output: &Output) -> io::Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        io::Error::other(format!("{context}: {}", output.status))
    } else {
        io::Error::other(format!("{context}: {stderr}"))
    }
}

pub(super) struct SshStdioBridge {
    local_socket: PathBuf,
    socket_identity: crate::ipc::SocketFileIdentity,
    should_stop: Arc<AtomicBool>,
    failure_rx: mpsc::Receiver<io::Error>,
    thread: Option<JoinHandle<()>>,
}

impl SshStdioBridge {
    pub(super) fn start(
        target: String,
        remote_herdr: RemoteHerdr,
        local_socket: PathBuf,
        session_name: String,
        ssh_options: Option<&ManagedSshOptions>,
        noninteractive: bool,
    ) -> io::Result<Self> {
        crate::ipc::prepare_socket_path(&local_socket, |path| {
            format!("remote bridge is already listening at {}", path.display())
        })?;
        let listener = crate::ipc::bind_private_local_listener(&local_socket)?;
        let socket_identity = crate::ipc::socket_file_identity(&local_socket)?;
        if let Err(err) =
            crate::ipc::restrict_socket_permissions(&local_socket, BRIDGE_SOCKET_PERMISSION_MODE)
        {
            let _ = crate::ipc::remove_socket_file_if_owned(&local_socket, &socket_identity);
            return Err(err);
        }
        if let Err(err) = listener.set_nonblocking(ListenerNonblockingMode::Accept) {
            let _ = crate::ipc::remove_socket_file_if_owned(&local_socket, &socket_identity);
            return Err(err);
        }

        let should_stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&should_stop);
        let thread_ssh_options = ssh_options.cloned();
        let (failure_tx, failure_rx) = mpsc::sync_channel(1);
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok(stream) => {
                        let stream = match prepare_remote_bridge_stream(stream) {
                            Ok(stream) => stream,
                            Err(err) => {
                                tracing::error!(
                                    error = %err,
                                    "remote bridge failed to prepare client socket"
                                );
                                continue;
                            }
                        };
                        if let Err(err) = bridge_connection(
                            stream,
                            &target,
                            &remote_herdr,
                            &session_name,
                            thread_ssh_options.as_ref(),
                            noninteractive,
                            &thread_stop,
                        ) {
                            let _ =
                                failure_tx.try_send(io::Error::new(err.kind(), err.to_string()));
                            if noninteractive {
                                tracing::warn!(error = %err, "saved SSH endpoint bridge failed");
                            } else {
                                eprintln!("herdr: remote bridge failed: {err}");
                            }
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(BRIDGE_ACCEPT_POLL);
                    }
                    Err(err) => {
                        if noninteractive {
                            tracing::warn!(error = %err, "saved SSH endpoint listener failed");
                        } else {
                            eprintln!("herdr: remote bridge listener failed: {err}");
                        }
                        break;
                    }
                }
            }
        });

        Ok(Self {
            local_socket,
            socket_identity,
            should_stop,
            failure_rx,
            thread: Some(thread),
        })
    }

    fn reported_failure(&self) -> Option<io::Error> {
        self.failure_rx
            .recv_timeout(BRIDGE_FAILURE_REPORT_TIMEOUT)
            .ok()
    }
}

fn prepare_remote_bridge_stream(
    mut stream: crate::ipc::LocalStream,
) -> io::Result<crate::ipc::LocalStream> {
    crate::ipc::set_local_stream_polling(&mut stream, false)?;
    Ok(stream)
}

impl Drop for SshStdioBridge {
    fn drop(&mut self) {
        self.should_stop.store(true, Ordering::Release);
        #[cfg(unix)]
        let _ = crate::ipc::remove_socket_file_if_owned(&self.local_socket, &self.socket_identity);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        #[cfg(windows)]
        let _ = crate::ipc::remove_socket_file_if_owned(&self.local_socket, &self.socket_identity);
    }
}

fn ssh_config_quote(path: &str) -> String {
    format!("\"{path}\"")
}

fn ssh_config_include_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '\\' {
        ssh_config_quote(&path.replace('\\', "/"))
    } else {
        ssh_config_quote(&path)
    }
}

/// Builds a temporary ssh config that includes the user's settings first, so
/// OpenSSH's first-value-wins behavior preserves explicit user keepalives.
fn write_managed_ssh_config() -> io::Result<ManagedSshConfig> {
    let paths = crate::platform::remote_ssh_config_paths();
    let dir = crate::platform::create_remote_ssh_config_dir(SSH_CONTROL_SOCKET_NAME)?;
    let path = dir.join("config");
    let control_path = paths
        .multiplexing
        .then(|| dir.join(SSH_CONTROL_SOCKET_NAME));

    let mut contents = String::new();
    if let Some(user_config) = paths.user_config.filter(|path| path.is_file()) {
        contents.push_str(&format!(
            "Include {}\n",
            ssh_config_include_path(&user_config)
        ));
    }
    if let Some(system_config) = paths.system_config.filter(|path| path.is_file()) {
        contents.push_str(&format!(
            "Include {}\n",
            ssh_config_include_path(&system_config)
        ));
    }
    contents.push_str("Host *\n");
    contents.push_str("  ServerAliveInterval 15\n");
    contents.push_str("  ServerAliveCountMax 4\n");

    let write_result = (|| {
        let mut file = crate::platform::create_remote_ssh_config_file(&path)?;
        file.write_all(contents.as_bytes())
    })();
    if let Err(err) = write_result {
        let _ = fs::remove_dir_all(&dir);
        return Err(err);
    }
    Ok(ManagedSshConfig {
        options: ManagedSshOptions {
            config_path: path,
            control_path,
        },
    })
}

struct BridgeUploadStop {
    stopped: AtomicBool,
    wake: crate::platform::RemoteBridgeWake,
}

impl BridgeUploadStop {
    fn new() -> io::Result<Self> {
        Ok(Self {
            stopped: AtomicBool::new(false),
            wake: crate::platform::RemoteBridgeWake::new()?,
        })
    }

    fn cancel(&self) {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            if let Err(error) = self.wake.cancel() {
                tracing::debug!(%error, "remote bridge read cancellation failed");
            }
        }
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
}

#[cfg(all(test, unix))]
pub(crate) fn bridge_upload_cancellation_for_test(
    stream: crate::ipc::LocalStream,
    mut writer: impl io::Write + Send + 'static,
) -> impl FnOnce() {
    stream.set_nonblocking(true).unwrap();
    let stop = Arc::new(BridgeUploadStop::new().unwrap());
    let worker_stop = Arc::clone(&stop);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        let closed = AtomicBool::new(false);
        let result = copy_local_stream_to_writer(
            stream,
            &mut writer,
            &worker_stop,
            &AtomicBool::new(false),
            &closed,
        );
        done_tx
            .send((result, closed.load(Ordering::Acquire)))
            .unwrap();
    });
    move || {
        stop.cancel();
        let (result, closed) = done_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        worker.join().unwrap();
        result.unwrap();
        assert!(!closed, "upload cancellation must not report peer EOF");
    }
}

fn bridge_connection(
    mut stream: crate::ipc::LocalStream,
    target: &str,
    remote_herdr: &RemoteHerdr,
    session_name: &str,
    ssh_options: Option<&ManagedSshOptions>,
    noninteractive: bool,
    bridge_stop: &Arc<AtomicBool>,
) -> io::Result<()> {
    let upload_stop = Arc::new(BridgeUploadStop::new()?);
    let mut command = Command::new("ssh");
    apply_managed_ssh_options(&mut command, ssh_options);
    if noninteractive {
        apply_noninteractive_ssh_options(&mut command);
    }
    command
        .arg("-T")
        .arg(target)
        .arg(remote_herdr.executable.bridge_command(session_name))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(if noninteractive {
            Stdio::piped()
        } else {
            Stdio::inherit()
        });

    let mut child = command
        .spawn()
        .map_err(|err| io::Error::new(err.kind(), format!("failed to start ssh bridge: {err}")))?;
    let mut child_stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => return terminate_bridge_child(child, "ssh bridge stdin missing"),
    };
    let child_stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => return terminate_bridge_child(child, "ssh bridge stdout missing"),
    };
    let stderr_reader = if noninteractive {
        let Some(child_stderr) = child.stderr.take() else {
            return terminate_bridge_child(child, "ssh bridge stderr missing");
        };
        Some(thread::spawn(move || capture_ssh_stderr(child_stderr)))
    } else {
        None
    };
    let stream_to_child = match stream.try_clone() {
        Ok(stream) => stream,
        Err(err) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(err);
        }
    };
    if let Err(err) = crate::ipc::set_local_stream_polling(&mut stream, true) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(err);
    }
    let mut child_to_stream = stream;

    let connection_stop = Arc::new(AtomicBool::new(false));
    let upload_failed = Arc::new(AtomicBool::new(false));
    let download_done = Arc::new(AtomicBool::new(false));
    let client_closed = Arc::new(AtomicBool::new(false));
    let upload_cancel = Arc::clone(&upload_stop);
    let upload_bridge_stop = Arc::clone(bridge_stop);
    let upload_failed_worker = Arc::clone(&upload_failed);
    let upload_client_closed = Arc::clone(&client_closed);
    let upload = thread::spawn(move || {
        let result = copy_local_stream_to_writer(
            stream_to_child,
            &mut child_stdin,
            &upload_cancel,
            &upload_bridge_stop,
            &upload_client_closed,
        );
        upload_failed_worker.store(result.is_err(), Ordering::Release);
        result
    });
    let download_stop = Arc::clone(&connection_stop);
    let download_bridge_stop = Arc::clone(bridge_stop);
    let download_done_worker = Arc::clone(&download_done);
    let download_upload_stop = Arc::clone(&upload_stop);
    let download = thread::spawn(move || {
        let mut child_stdout = io::BufReader::new(child_stdout);
        let result = discard_remote_output_preamble(&mut child_stdout).and_then(|()| {
            copy_reader_to_local_stream(
                &mut child_stdout,
                &mut child_to_stream,
                &download_stop,
                &download_bridge_stop,
            )
        });
        download_done_worker.store(true, Ordering::Release);
        download_upload_stop.cancel();
        result
    });

    let mut stopped_at = None;
    let (status_result, child_exited) = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                upload_stop.cancel();
                break (Ok(status), true);
            }
            Ok(None) => {}
            Err(err) => {
                connection_stop.store(true, Ordering::Release);
                upload_stop.cancel();
                let _ = child.kill();
                let _ = child.wait();
                break (Err(err), false);
            }
        }
        if bridge_stop.load(Ordering::Acquire) {
            connection_stop.store(true, Ordering::Release);
            upload_stop.cancel();
            let _ = child.kill();
            break (child.wait(), false);
        }
        if client_closed.load(Ordering::Acquire)
            || upload_failed.load(Ordering::Acquire)
            || download_done.load(Ordering::Acquire)
        {
            upload_stop.cancel();
            let stopped_at = stopped_at.get_or_insert_with(Instant::now);
            if stopped_at.elapsed() >= Duration::from_millis(250) {
                connection_stop.store(true, Ordering::Release);
                let _ = child.kill();
                break (child.wait(), false);
            }
        }
        thread::sleep(BRIDGE_ACCEPT_POLL);
    };
    upload_stop.cancel();
    if !child_exited {
        connection_stop.store(true, Ordering::Release);
    }
    let upload_result = upload
        .join()
        .map_err(|_| io::Error::other("remote bridge upload worker panicked"))?;
    let download_result = download
        .join()
        .map_err(|_| io::Error::other("remote bridge download worker panicked"))?;
    let stderr = match stderr_reader {
        Some(reader) => reader
            .join()
            .map_err(|_| io::Error::other("SSH stderr reader panicked"))??,
        None => Vec::new(),
    };
    let status = status_result?;

    let stopping = bridge_stop.load(Ordering::Acquire);
    let client_closed = client_closed.load(Ordering::Acquire);
    if child_exited && !status.success() && !stopping && !client_closed {
        return Err(ssh_bridge_exit_error(status, &stderr));
    }
    if !stopping && !client_closed {
        upload_result.map_err(|err| {
            io::Error::new(err.kind(), format!("remote bridge upload failed: {err}"))
        })?;
        download_result.map_err(|err| {
            io::Error::new(err.kind(), format!("remote bridge download failed: {err}"))
        })?;
    }

    if status.success() || stopping || client_closed {
        Ok(())
    } else {
        Err(ssh_bridge_exit_error(status, &stderr))
    }
}

fn ssh_bridge_exit_error(status: std::process::ExitStatus, stderr: &[u8]) -> io::Error {
    let stderr = String::from_utf8_lossy(stderr);
    let stderr = stderr.trim();
    let message = if stderr.is_empty() {
        format!("ssh bridge exited with {status}")
    } else {
        format!("remote SSH connection failed: {stderr}")
    };
    io::Error::new(io::ErrorKind::ConnectionAborted, message)
}

fn capture_ssh_stderr(mut stderr: impl io::Read) -> io::Result<Vec<u8>> {
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 4 * 1024];
    loop {
        let read = stderr.read(&mut buffer)?;
        if read == 0 {
            return Ok(captured);
        }
        let remaining = NONINTERACTIVE_SSH_STDERR_LIMIT.saturating_sub(captured.len());
        captured.extend_from_slice(&buffer[..read.min(remaining)]);
    }
}

fn discard_remote_output_preamble(reader: &mut impl io::BufRead) -> io::Result<()> {
    let marker = REMOTE_OUTPUT_READY_MARKER.as_bytes();
    let mut matched = 0;
    let mut matching = true;
    loop {
        let (consumed, ready) = {
            let buffer = reader.fill_buf()?;
            if buffer.is_empty() {
                if matching && matched == marker.len() {
                    return Ok(());
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "remote command exited before producing its output marker",
                ));
            }

            let mut consumed = 0;
            let mut ready = false;
            for &byte in buffer {
                consumed += 1;
                if byte == b'\n' {
                    if matching && matched == marker.len() {
                        ready = true;
                        break;
                    }
                    matched = 0;
                    matching = true;
                } else if matching && matched < marker.len() && byte == marker[matched] {
                    matched += 1;
                } else if matching && (matched != marker.len() || byte != b'\r') {
                    matching = false;
                }
            }
            (consumed, ready)
        };
        reader.consume(consumed);
        if ready {
            return Ok(());
        }
    }
}

fn terminate_bridge_child(mut child: std::process::Child, message: &'static str) -> io::Result<()> {
    let _ = child.kill();
    let _ = child.wait();
    Err(io::Error::new(io::ErrorKind::BrokenPipe, message))
}

fn copy_reader_to_local_stream<R: io::Read>(
    reader: &mut R,
    stream: &mut crate::ipc::LocalStream,
    connection_stop: &AtomicBool,
    bridge_stop: &AtomicBool,
) -> io::Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0;

    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => return Ok(total),
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        let mut written = 0;
        while written < read {
            if connection_stop.load(Ordering::Acquire) || bridge_stop.load(Ordering::Acquire) {
                return Ok(total);
            }
            let chunk_len = (read - written).min(4 * 1024);
            match stream.write(&buffer[written..written + chunk_len]) {
                Ok(0) => thread::sleep(BRIDGE_IO_POLL),
                Ok(count) => written += count,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(BRIDGE_IO_POLL);
                }
                Err(err) => return Err(err),
            }
        }
        stream.flush()?;
        total += read as u64;
    }
}

fn copy_local_stream_to_writer<W: io::Write>(
    mut stream: crate::ipc::LocalStream,
    writer: &mut W,
    connection_stop: &BridgeUploadStop,
    bridge_stop: &AtomicBool,
    client_closed: &AtomicBool,
) -> io::Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0;

    while !connection_stop.is_stopped() && !bridge_stop.load(Ordering::Acquire) {
        #[cfg(all(test, unix))]
        tests::UPLOAD_READ_ATTEMPTS.with(|attempts| {
            if let Some(attempts) = attempts.borrow().as_ref() {
                attempts.fetch_add(1, Ordering::Relaxed);
            }
        });
        match crate::ipc::poll_local_stream_read_count(&mut stream, &mut buffer)? {
            crate::ipc::LocalStreamReadCount::Data(read) => {
                writer.write_all(&buffer[..read])?;
                writer.flush()?;
                total += read as u64;
            }
            crate::ipc::LocalStreamReadCount::Pending => {
                connection_stop.wake.wait(&stream)?;
            }
            crate::ipc::LocalStreamReadCount::Closed => {
                client_closed.store(true, Ordering::Release);
                break;
            }
        }
    }

    Ok(total)
}

fn run_client_process(
    local_socket: &Path,
    reattach_command: &str,
    keybindings: RemoteKeybindings,
) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let status = Command::new(exe)
        .arg("client")
        .env(
            crate::server::socket_paths::CLIENT_SOCKET_PATH_ENV_VAR,
            local_socket,
        )
        .env(REATTACH_COMMAND_ENV_VAR, reattach_command)
        .env(REMOTE_KEYBINDINGS_ENV_VAR, keybindings.as_str())
        .env_remove(crate::api::SOCKET_PATH_ENV_VAR)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            format!("remote client exited with {status}"),
        ))
    }
}

fn local_forward_socket_path(target: &str, session_name: &str) -> PathBuf {
    let pid = std::process::id();
    let target_clean = sanitize_path_component(target);
    let session_clean = sanitize_path_component(session_name);
    let readable_name = format!("herdr-remote-{pid}-{target_clean}-{session_clean}.sock");
    let target_prefix: String = target_clean.chars().take(8).collect();
    let hash = short_socket_hash(target, session_name);
    let short_name = format!("herdr-r-{pid}-{target_prefix}-{hash}.sock");
    crate::platform::remote_bridge_endpoint_path(&readable_name, &short_name)
}

#[cfg(all(test, unix))]
fn fits_unix_socket_path(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;

    path.as_os_str().as_bytes().len() <= 103
}

fn short_socket_hash(target: &str, session: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    target.hash(&mut hasher);
    0u8.hash(&mut hasher);
    session.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn sanitize_path_component(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect();

    sanitized.trim_matches('-').chars().take(32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    thread_local! {
        pub(super) static UPLOAD_READ_ATTEMPTS: std::cell::RefCell<Option<Arc<std::sync::atomic::AtomicUsize>>> = const { std::cell::RefCell::new(None) };
    }

    #[cfg(unix)]
    fn upload_test_streams(name: &str) -> (crate::ipc::LocalStream, crate::ipc::LocalStream) {
        let socket = local_forward_socket_path(name, "upload-test");
        let listener = crate::ipc::bind_private_local_listener(&socket).unwrap();
        let client = crate::ipc::connect_local_stream(&socket).unwrap();
        let server = listener.accept().unwrap();
        server.set_nonblocking(true).unwrap();
        drop(listener);
        std::fs::remove_file(socket).unwrap();
        (client, server)
    }

    #[cfg(unix)]
    #[test]
    fn bridge_upload_idle_waits_without_repeated_reads_and_cancels() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::mpsc;

        let (mut client, stream) = upload_test_streams("idle");
        let attempts = Arc::new(AtomicUsize::new(0));
        let worker_attempts = Arc::clone(&attempts);
        let stop = Arc::new(BridgeUploadStop::new().unwrap());
        let worker_stop = Arc::clone(&stop);
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            UPLOAD_READ_ATTEMPTS.with(|slot| *slot.borrow_mut() = Some(worker_attempts));
            let mut output = Vec::new();
            let closed = AtomicBool::new(false);
            let result = copy_local_stream_to_writer(
                stream,
                &mut output,
                &worker_stop,
                &AtomicBool::new(false),
                &closed,
            );
            done_tx
                .send((result, output, closed.load(Ordering::Acquire)))
                .unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while attempts.load(Ordering::Relaxed) == 0 {
            assert!(Instant::now() < deadline, "upload worker did not start");
            thread::sleep(Duration::from_millis(1));
        }
        thread::sleep(Duration::from_millis(100));
        let idle_reads = attempts.load(Ordering::Relaxed);
        client.write_all(b"pane input").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while attempts.load(Ordering::Relaxed) < idle_reads + 2 {
            assert!(
                Instant::now() < deadline,
                "input did not wake the upload worker"
            );
            thread::sleep(Duration::from_millis(1));
        }
        thread::sleep(Duration::from_millis(100));
        let reads_after_input = attempts.load(Ordering::Relaxed);
        stop.cancel();
        let (result, output, closed) = done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        assert_eq!(result.unwrap(), 10);
        assert_eq!(output, b"pane input");
        assert!(!closed, "cancellation is not a peer disconnect");
        assert_eq!(idle_reads, 1, "idle forwarding must wait, not retry reads");
        assert_eq!(
            reads_after_input, 3,
            "forwarding must sleep again after input"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bridge_upload_cancel_before_wait_preserves_download() {
        use std::io::Read as _;

        let (mut client, stream) = upload_test_streams("cancel-before-wait");
        let mut download = stream.try_clone().unwrap();
        let stop = BridgeUploadStop::new().unwrap();
        stop.cancel();
        stop.cancel();
        let closed = AtomicBool::new(false);
        let count = copy_local_stream_to_writer(
            stream,
            &mut Vec::new(),
            &stop,
            &AtomicBool::new(false),
            &closed,
        )
        .unwrap();
        assert_eq!(count, 0);
        assert!(!closed.load(Ordering::Acquire));
        download.write_all(b"final frame").unwrap();
        let mut output = [0; 11];
        client.read_exact(&mut output).unwrap();
        assert_eq!(&output, b"final frame");
    }

    #[cfg(unix)]
    #[test]
    fn bridge_upload_cancel_between_stop_check_and_wait_is_retained() {
        let (_client, stream) = upload_test_streams("cancel-before-poll");
        let stop = BridgeUploadStop::new().unwrap();
        assert!(!stop.is_stopped());
        stop.cancel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            done_tx.send(stop.wake.wait(&stream)).unwrap();
        });
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn bridge_upload_drains_input_before_peer_eof() {
        let (mut client, stream) = upload_test_streams("drain");
        let payload = vec![b'x'; 1024 * 1024];
        let expected = payload.clone();
        let worker = thread::spawn(move || {
            let stop = BridgeUploadStop::new().unwrap();
            let mut output = Vec::new();
            let closed = AtomicBool::new(false);
            let count = copy_local_stream_to_writer(
                stream,
                &mut output,
                &stop,
                &AtomicBool::new(false),
                &closed,
            )
            .unwrap();
            assert!(closed.load(Ordering::Acquire));
            assert_eq!(count, output.len() as u64);
            output
        });
        client.write_all(&payload).unwrap();
        drop(client);
        assert_eq!(worker.join().unwrap(), expected);
    }

    #[cfg(unix)]
    #[test]
    fn bridge_socket_is_user_only() {
        use std::os::unix::fs::PermissionsExt;

        let socket = std::env::temp_dir().join(format!(
            "herdr-bridge-permissions-test-{}.sock",
            std::process::id()
        ));
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let bridge = SshStdioBridge::start(
            "example".to_string(),
            remote_herdr,
            socket.clone(),
            "default".to_string(),
            None,
            false,
        )
        .expect("start bridge listener");

        let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, BRIDGE_SOCKET_PERMISSION_MODE);

        drop(bridge);
        let _ = std::fs::remove_file(socket);
    }

    #[cfg(unix)]
    #[test]
    fn accepted_bridge_stream_is_reset_to_blocking() {
        use std::os::fd::AsRawFd as _;

        fn is_nonblocking(stream: &crate::ipc::LocalStream) -> bool {
            let fd = match stream {
                crate::ipc::LocalStream::UdSocket(stream) => stream.inner().as_raw_fd(),
            };
            // SAFETY: F_GETFL only reads flags from the live descriptor owned by `stream`.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            assert!(flags >= 0, "fcntl(F_GETFL): {}", io::Error::last_os_error());
            flags & libc::O_NONBLOCK != 0
        }

        let socket = std::env::temp_dir().join(format!(
            "herdr-bridge-blocking-test-{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&socket);
        let listener = crate::ipc::bind_private_local_listener(&socket).expect("bind listener");
        let client = crate::ipc::connect_local_stream(&socket).expect("connect client");
        let mut server = listener.accept().expect("accept client");

        crate::ipc::set_local_stream_polling(&mut server, true)
            .expect("force the macOS accepted-stream state");
        assert!(is_nonblocking(&server));
        let server = prepare_remote_bridge_stream(server).expect("prepare bridge stream");
        assert!(!is_nonblocking(&server));

        drop(server);
        drop(client);
        drop(listener);
        let _ = std::fs::remove_file(socket);
    }

    #[cfg(windows)]
    #[test]
    fn bridge_stream_delivers_large_frame_before_delayed_reply() {
        use std::io::Read as _;
        use std::sync::mpsc;
        use std::time::{SystemTime, UNIX_EPOCH};

        fn read_frame(stream: &mut crate::ipc::LocalStream) -> Vec<u8> {
            let mut length = [0; 4];
            stream.read_exact(&mut length).expect("read frame length");
            let length = usize::try_from(u32::from_le_bytes(length)).expect("frame length fits");
            let mut body = vec![0; length];
            stream.read_exact(&mut body).expect("read frame body");
            body
        }

        fn write_frame(stream: &mut crate::ipc::LocalStream, body: &[u8]) {
            let length = u32::try_from(body.len()).expect("frame length fits");
            stream
                .write_all(&length.to_le_bytes())
                .expect("write frame length");
            stream.write_all(body).expect("write frame body");
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let socket = std::env::temp_dir().join(format!(
            "herdr-bridge-large-frame-{}-{nonce}.sock",
            std::process::id()
        ));
        let listener = crate::ipc::bind_private_local_listener(&socket).expect("bind listener");
        let (large_received_tx, large_received_rx) = mpsc::channel();
        let client_socket = socket.clone();
        let client = thread::spawn(move || {
            let mut stream =
                crate::ipc::connect_local_stream(&client_socket).expect("connect client");
            assert_eq!(read_frame(&mut stream), b"welcome");
            assert_eq!(read_frame(&mut stream), vec![b'x'; 16 * 1024]);
            large_received_tx.send(()).expect("signal large frame");
            assert_eq!(read_frame(&mut stream), b"pong");
        });
        let mut server = prepare_remote_bridge_stream(listener.accept().expect("accept client"))
            .expect("prepare bridge stream");

        crate::ipc::set_local_stream_polling(&mut server, true)
            .expect("enable bridge read polling");
        write_frame(&mut server, b"welcome");
        write_frame(&mut server, &vec![b'x'; 16 * 1024]);
        large_received_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("client received large frame");
        write_frame(&mut server, b"pong");

        client.join().expect("client thread");
        drop(server);
        drop(listener);
        let _ = std::fs::remove_file(socket);
    }

    #[test]
    fn bridge_drop_while_waiting_for_client_is_bounded() {
        let socket = local_forward_socket_path("drop-test", "default");
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let bridge = SshStdioBridge::start(
            "example".to_string(),
            remote_herdr,
            socket.clone(),
            "default".to_string(),
            None,
            false,
        )
        .expect("start bridge listener");
        let started = Instant::now();

        drop(bridge);

        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!socket.exists());
    }

    #[cfg(unix)]
    #[test]
    fn managed_ssh_config_includes_user_config_then_fallback() {
        use std::os::unix::fs::PermissionsExt;

        let managed_config = write_managed_ssh_config().expect("write managed config");
        let path = managed_config.options.config_path.clone();
        let control_path = managed_config
            .options
            .control_path
            .clone()
            .expect("Unix managed config has a control path");
        let contents = std::fs::read_to_string(&path).expect("read keepalive config");

        // herdr's fallback transport settings are present...
        assert!(
            contents.contains("Host *"),
            "config should add a Host * fallback block: {contents}"
        );
        assert!(
            contents.contains("ServerAliveInterval 15"),
            "config should set the keepalive interval: {contents}"
        );
        assert!(
            contents.contains("ServerAliveCountMax 4"),
            "config should set the keepalive count: {contents}"
        );
        assert!(!contents.contains("ControlMaster"));
        assert!(!contents.contains("ControlPersist"));
        assert!(!contents.contains("ControlPath"));
        // ...and any user config is Included (quoted) BEFORE it so
        // first-value-wins keeps the user's own settings.
        if let Some(home) = std::env::var_os("HOME") {
            let user_config = PathBuf::from(home).join(".ssh").join("config");
            if user_config.is_file() {
                let include = format!(
                    "Include {}",
                    ssh_config_quote(&user_config.to_string_lossy())
                );
                let include_at = contents.find(&include).expect("user config Included");
                let fallback_at = contents.find("Host *").expect("fallback present");
                assert!(
                    include_at < fallback_at,
                    "user config must be Included before herdr's fallback: {contents}"
                );
            }
        }

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, BRIDGE_SOCKET_PERMISSION_MODE,
            "keepalive config must be user-only"
        );
        // The config lives in a private 0700 dir, not a predictable temp path.
        let dir = path.parent().expect("config has a parent dir");
        let dir_mode = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "ssh config dir must be user-only");
        assert!(
            fits_unix_socket_path(&control_path),
            "control socket path must fit portable Unix socket limits"
        );

        drop(managed_config);
    }

    #[test]
    fn ssh_config_quote_wraps_path_with_spaces() {
        assert_eq!(
            ssh_config_quote("/home/a b/.ssh/config"),
            "\"/home/a b/.ssh/config\""
        );
    }

    #[cfg(unix)]
    #[test]
    fn remote_ssh_command_uses_managed_config_when_present() {
        let managed_config = write_managed_ssh_config().expect("write managed config");
        let config_path = managed_config.options.config_path.clone();
        let control_path = managed_config
            .options
            .control_path
            .clone()
            .expect("Unix managed config has a control path");
        let ssh = RemoteSsh {
            target: "example".to_string(),
            session_name: crate::session::DEFAULT_SESSION_NAME.into(),
            managed_config: Some(managed_config),
            noninteractive: false,
        };

        let command = ssh.command();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            args,
            vec![
                "-F".to_string(),
                config_path.to_string_lossy().into_owned(),
                "-S".to_string(),
                control_path.to_string_lossy().into_owned(),
                "-o".to_string(),
                "ControlMaster=auto".to_string(),
                "-o".to_string(),
                "ControlPersist=yes".to_string(),
                "-T".to_string(),
                "example".to_string(),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_managed_ssh_config_uses_keepalives_without_control_socket() {
        let managed_config = write_managed_ssh_config().expect("write managed config");
        let config_path = managed_config.options.config_path.clone();
        assert!(managed_config.options.control_path.is_none());
        let contents = std::fs::read_to_string(&config_path).expect("read managed config");
        assert!(contents.contains("ServerAliveInterval 15"));
        assert!(contents.contains("ServerAliveCountMax 4"));

        let ssh = RemoteSsh {
            target: "example".to_string(),
            session_name: crate::session::DEFAULT_SESSION_NAME.into(),
            managed_config: Some(managed_config),
            noninteractive: false,
        };
        let args = ssh
            .command()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                "-F".to_string(),
                config_path.to_string_lossy().into_owned(),
                "-T".to_string(),
                "example".to_string(),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_ssh_config_include_uses_forward_slashes() {
        assert_eq!(
            ssh_config_include_path(Path::new(r"C:\Users\A B\.ssh\config")),
            r#""C:/Users/A B/.ssh/config""#
        );
    }

    #[test]
    fn noninteractive_ssh_stderr_capture_is_bounded() {
        let stderr = vec![b'x'; NONINTERACTIVE_SSH_STDERR_LIMIT + 4096];
        let captured = capture_ssh_stderr(stderr.as_slice()).expect("capture stderr");
        assert_eq!(captured.len(), NONINTERACTIVE_SSH_STDERR_LIMIT);
    }

    #[test]
    fn noninteractive_ssh_command_cannot_prompt_or_accept_unknown_hosts() {
        let ssh = RemoteSsh::new_noninteractive("example".into());
        let args = ssh
            .command()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        for required in [
            "BatchMode=yes",
            "NumberOfPasswordPrompts=0",
            "StrictHostKeyChecking=yes",
            "ConnectTimeout=10",
            "ConnectionAttempts=1",
            "ServerAliveInterval=15",
            "ServerAliveCountMax=4",
        ] {
            assert!(args.iter().any(|arg| arg == required), "missing {required}");
        }
        assert!(!args.iter().any(|arg| arg == "-F"));
        assert!(ssh.options().is_none());
    }

    #[test]
    fn remote_setup_approval_requires_input_and_rejects_unrecognized_answers() {
        for default in [false, true] {
            for input in ["", "maybe\n"] {
                assert_eq!(
                    read_remote_confirmation(&mut input.as_bytes(), default)
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::Interrupted
                );
            }
            assert_eq!(
                read_remote_confirmation(&mut "\n".as_bytes(), default).unwrap(),
                default
            );
            assert!(read_remote_confirmation(&mut "YES\n".as_bytes(), default).unwrap());
            assert!(!read_remote_confirmation(&mut "no\n".as_bytes(), default).unwrap());
        }
    }

    #[test]
    fn saved_machine_compatibility_uses_capabilities_not_release_or_private_protocol() {
        let mut status = RemoteClientStatusJson {
            version: Some("0.1.0".into()),
            protocol: Some(1),
            endpoint_protocol_generation: Some(
                crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION,
            ),
            endpoint_capabilities: vec![
                crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY.into(),
                crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY.into(),
                crate::protocol::endpoint::HEALTH_CHECK_CAPABILITY.into(),
            ],
        };
        assert!(status.supports_endpoint_requirement(true));
        for index in 0..status.endpoint_capabilities.len() {
            let removed = status.endpoint_capabilities.remove(index);
            assert!(!status.supports_endpoint_requirement(true));
            assert!(status.supports_endpoint_requirement(false));
            status.endpoint_capabilities.insert(index, removed);
        }
        status.endpoint_protocol_generation = None;
        assert!(!status.supports_endpoint_requirement(true));
    }

    #[test]
    fn saved_machine_server_commands_are_scoped_to_the_explicit_session() {
        let herdr =
            RemoteHerdr::for_platform(RemotePlatform::from_uname("Linux", "x86_64").unwrap());
        for (args, command) in [
            (&["status", "server", "--json"][..], "status server --json"),
            (&["server", "stop"][..], "server stop"),
            (&["remote-client-bridge"][..], "remote-client-bridge"),
        ] {
            assert_eq!(
                herdr.executable.session_command("agents", args),
                format!("{} --session agents {command}", herdr.executable.display())
            );
            assert_eq!(
                herdr
                    .executable
                    .session_command(crate::session::DEFAULT_SESSION_NAME, args),
                format!("{} {command}", herdr.executable.display())
            );
        }
        assert!(herdr
            .executable
            .live_handoff_command("agents", 19, "0.7.9")
            .starts_with(&format!(
                "{} --session agents server live-handoff",
                herdr.executable.display()
            )));
    }

    #[test]
    fn remote_ssh_command_is_plain_without_managed_config() {
        let ssh = RemoteSsh {
            target: "example".to_string(),
            session_name: crate::session::DEFAULT_SESSION_NAME.into(),
            managed_config: None,
            noninteractive: false,
        };

        let command = ssh.command();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(args, vec!["-T".to_string(), "example".to_string()]);
    }

    #[test]
    fn remote_install_stream_command_avoids_shell_c_wrapper() {
        let command = remote_install_stream_command("/home/a b/.local/bin/herdr.tmp.123");

        assert_eq!(command, "tee '/home/a b/.local/bin/herdr.tmp.123'");
    }

    #[test]
    fn remote_install_prepare_and_commit_scripts_quote_paths() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let prepare = remote_install_prepare_script(&remote_herdr);

        assert!(prepare.contains("mkdir -p \"$dir\""));
        assert!(prepare.contains("printf '%s\\0%s\\0' \"$tmp\" \"$dest\""));
        assert_eq!(
            parse_remote_install_paths(b"/home/a b/herdr.tmp.42\0/home/a b/herdr\0").unwrap(),
            (
                "/home/a b/herdr.tmp.42".to_string(),
                "/home/a b/herdr".to_string()
            )
        );
        assert_eq!(
            parse_remote_install_paths(b"/home/a b\n/herdr.tmp.42\0/home/a b\n/herdr\0").unwrap(),
            (
                "/home/a b\n/herdr.tmp.42".to_string(),
                "/home/a b\n/herdr".to_string()
            )
        );
        assert_eq!(
            remote_install_commit_script("/home/a b/herdr.tmp.42", "/home/a b/herdr"),
            "set -eu\nchmod 755 '/home/a b/herdr.tmp.42'\nmv '/home/a b/herdr.tmp.42' '/home/a b/herdr'\n"
        );
    }

    #[test]
    fn extract_remote_args_removes_space_form() {
        let args = vec![
            "herdr".into(),
            "--remote".into(),
            "dev".into(),
            "--help".into(),
        ];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr", "--help"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "dev");
        assert_eq!(remote.keybindings, RemoteKeybindings::Local);
    }

    #[test]
    fn extract_remote_args_removes_equals_form() {
        let args = vec!["herdr".into(), "--remote=user@host".into()];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "user@host");
        assert_eq!(remote.keybindings, RemoteKeybindings::Local);
    }

    #[test]
    fn extract_remote_args_accepts_remote_keybindings_server() {
        let args = vec![
            "herdr".into(),
            "--remote".into(),
            "dev".into(),
            "--remote-keybindings=server".into(),
        ];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "dev");
        assert_eq!(remote.keybindings, RemoteKeybindings::Server);
    }

    #[test]
    fn extract_remote_args_accepts_remote_keybindings_space_form() {
        let args = vec![
            "herdr".into(),
            "--remote=dev".into(),
            "--remote-keybindings".into(),
            "server".into(),
        ];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr"]);
        assert_eq!(remote.unwrap().keybindings, RemoteKeybindings::Server);
    }

    #[test]
    fn extract_remote_args_accepts_explicit_handoff() {
        let args = vec!["herdr".into(), "--remote=dev".into(), "--handoff".into()];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, vec!["herdr"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "dev");
        assert!(remote.live_handoff);
    }

    #[test]
    fn extract_remote_args_preserves_child_remote_options_after_separator() {
        let args = vec![
            "herdr".into(),
            "agent".into(),
            "start".into(),
            "repro".into(),
            "--".into(),
            "child".into(),
            "--remote".into(),
            "dev".into(),
            "--remote-keybindings=server".into(),
            "--handoff".into(),
        ];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, args);
        assert!(remote.is_none());
    }

    #[test]
    fn extract_remote_args_preserves_handoff_without_remote() {
        let args = vec!["herdr".into(), "update".into(), "--handoff".into()];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, args);
        assert!(remote.is_none());
    }

    #[test]
    fn extract_remote_args_rejects_remote_keybindings_without_remote() {
        let args = vec!["herdr".into(), "--remote-keybindings=server".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote-keybindings requires --remote");
    }

    #[test]
    fn extract_remote_args_rejects_duplicate_remote_keybindings() {
        let args = vec![
            "herdr".into(),
            "--remote=dev".into(),
            "--remote-keybindings=local".into(),
            "--remote-keybindings=server".into(),
        ];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote-keybindings can only be specified once");
    }

    #[test]
    fn extract_remote_args_requires_value() {
        let args = vec!["herdr".into(), "--remote".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "missing value for --remote");
    }

    #[test]
    fn extract_remote_args_rejects_empty_value() {
        let args = vec!["herdr".into(), "--remote=".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "missing value for --remote");
    }

    #[test]
    fn extract_remote_args_rejects_duplicate_values() {
        let args = vec![
            "herdr".into(),
            "--remote=dev".into(),
            "--remote=prod".into(),
        ];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote can only be specified once");
    }

    #[test]
    fn extract_remote_args_rejects_option_like_target() {
        let args = vec!["herdr".into(), "--remote".into(), "-oProxyCommand=x".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote target must not start with '-'");
    }

    #[test]
    fn sanitize_path_component_removes_shell_sensitive_chars() {
        assert_eq!(sanitize_path_component("user@host:22"), "user-host-22");
    }

    #[test]
    fn remote_platform_maps_uname_values() {
        assert_eq!(
            RemotePlatform::from_uname("Linux", "amd64")
                .unwrap()
                .asset_key(),
            "linux-x86_64"
        );
        assert_eq!(
            RemotePlatform::from_uname("Darwin", "arm64")
                .unwrap()
                .asset_key(),
            "macos-aarch64"
        );
        assert!(RemotePlatform::from_uname("FreeBSD", "x86_64").is_none());
    }

    #[test]
    fn windows_platform_probe_accepts_only_x86_64() {
        assert_eq!(
            parse_windows_platform_probe("profile noise\r\nherdr-windows:AMD64\r\n").unwrap(),
            Some(RemotePlatform {
                os: "windows",
                arch: "x86_64",
            })
        );
        assert_eq!(parse_windows_platform_probe("other output").unwrap(), None);
        assert_eq!(
            parse_windows_platform_probe("herdr-windows:ARM64").unwrap_err(),
            "unsupported remote platform: Windows ARM64"
        );
    }

    #[test]
    fn windows_probe_preserves_transport_and_unsupported_unix_failures() {
        let cases = [
            (true, Some(0), false, false),
            (true, Some(0), true, true),
            (false, Some(255), false, false),
            (false, None, false, false),
            (false, Some(1), false, true),
        ];
        for (success, exit_code, windows_uname_hint, expected) in cases {
            assert_eq!(
                should_try_windows_probe(success, exit_code, windows_uname_hint),
                expected,
                "success={success}, exit_code={exit_code:?}, windows_uname_hint={windows_uname_hint}"
            );
        }

        for (os, expected) in [
            ("MINGW64_NT-10.0-26100", true),
            ("MSYS_NT-10.0", true),
            ("CYGWIN_NT-10.0", true),
            ("FreeBSD", false),
        ] {
            assert_eq!(looks_like_windows_uname(os), expected, "{os}");
        }
    }

    #[test]
    fn windows_remote_commands_use_one_encoded_powershell_grammar() {
        fn decode(command: &str) -> String {
            let encoded = command
                .strip_prefix("powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand ")
                .expect("encoded PowerShell command");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .expect("base64");
            let utf16 = bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect::<Vec<_>>();
            String::from_utf16(&utf16).expect("UTF-16LE")
        }

        let executable = RemoteExecutable::WindowsPath("herdr.exe".to_string());
        let commands = [
            (
                "platform probe",
                windows_platform_probe_command(),
                "$arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }; [Console]::Out.WriteLine('herdr-windows:' + $arch); exit 0",
            ),
            (
                "PATH lookup",
                executable.exists_command(),
                "if ($null -ne (Get-Command herdr.exe -CommandType Application -ErrorAction SilentlyContinue)) { exit 0 }; exit 1",
            ),
            (
                "client status",
                executable.status_client_command(),
                "& herdr.exe status client '--json'; exit $LASTEXITCODE",
            ),
            (
                "named server status",
                executable.session_command("agents", &["status", "server", "--json"]),
                "& herdr.exe '--session' agents status server '--json'; exit $LASTEXITCODE",
            ),
            (
                "server stop",
                executable.session_command("agents", &["server", "stop"]),
                "& herdr.exe '--session' agents server stop; exit $LASTEXITCODE",
            ),
            (
                "direct bridge",
                executable.bridge_command("agents"),
                "$process = Start-Process -FilePath herdr.exe -ArgumentList '--session agents remote-client-bridge' -NoNewWindow -Wait -PassThru -ErrorAction Stop; exit $process.ExitCode",
            ),
            (
                "saved bridge with closed stdin",
                executable.saved_bridge_command("agents"),
                "$process = Start-Process -FilePath herdr.exe -ArgumentList '--session agents remote-client-bridge' -NoNewWindow -Wait -PassThru -ErrorAction Stop; exit $process.ExitCode",
            ),
        ];

        let preamble = format!(
            "[Console]::Out.WriteLine(); [Console]::Out.WriteLine('{REMOTE_OUTPUT_READY_MARKER}'); [Console]::Out.Flush(); "
        );
        for (label, command, expected) in commands {
            let script = decode(&command);
            let script = script
                .strip_prefix(&preamble)
                .unwrap_or_else(|| panic!("{label} lacks shared output marker: {script}"));
            assert_eq!(script, expected, "{label}");
        }
    }

    #[test]
    fn remote_output_framing_discards_any_banner_and_preserves_binary() {
        let payload = [0, 1, 2, 0xff, b'\n'];
        let mut input = vec![b'x'; 4 * 1024 * 1024];
        input.extend_from_slice(b"\r\nherdr-remote-output-ready:1\r\n");
        input.extend_from_slice(&payload);
        let mut reader = io::BufReader::with_capacity(17, io::Cursor::new(input));

        discard_remote_output_preamble(&mut reader).unwrap();
        let mut output = Vec::new();
        io::Read::read_to_end(&mut reader, &mut output).unwrap();
        assert_eq!(output, payload);

        let mut missing = b"profile output without marker".to_vec();
        assert!(normalize_remote_stdout(&mut missing, true).is_err());
        normalize_remote_stdout(&mut missing, false).unwrap();
        assert_eq!(missing, b"profile output without marker");

        let mut platform = b"profile output\nherdr-remote-output-ready:1\nLinux\nx86_64\n".to_vec();
        normalize_remote_stdout(&mut platform, true).unwrap();
        let platform = String::from_utf8(platform).unwrap();
        let mut lines = platform.lines();
        assert_eq!(
            RemotePlatform::from_uname(lines.next().unwrap(), lines.next().unwrap()),
            Some(RemotePlatform {
                os: "linux",
                arch: "x86_64"
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn reattach_command_includes_remote_and_session() {
        assert_eq!(
            reattach_command(
                "target/release/herdr",
                "user@host",
                "work",
                RemoteKeybindings::Local,
                false,
            ),
            "target/release/herdr --remote user@host --session work"
        );
        assert_eq!(
            reattach_command(
                "herdr",
                "host name",
                crate::session::DEFAULT_SESSION_NAME,
                RemoteKeybindings::Local,
                false,
            ),
            "herdr --remote 'host name'"
        );
        assert_eq!(
            reattach_command(
                "herdr",
                "host",
                crate::session::DEFAULT_SESSION_NAME,
                RemoteKeybindings::Server,
                false,
            ),
            "herdr --remote host --remote-keybindings server"
        );
        assert_eq!(
            reattach_command(
                "herdr",
                "host",
                crate::session::DEFAULT_SESSION_NAME,
                RemoteKeybindings::Local,
                true,
            ),
            "herdr --remote host --handoff"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_reattach_command_uses_current_executable() {
        let executable = std::env::current_exe().expect("current test executable");
        assert_eq!(
            reattach_command(
                r"C:\Program Files\Herdr\herdr.exe",
                "host'name",
                "work'name",
                RemoteKeybindings::Local,
                false,
            ),
            format!(
                "& '{}' --remote 'host''name' --session 'work''name'",
                executable.display().to_string().replace('\'', "''")
            )
        );
    }

    #[test]
    fn remote_bridge_command_uses_installed_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        assert_eq!(
            remote_herdr
                .executable
                .bridge_command(crate::session::DEFAULT_SESSION_NAME),
            "printf '\n%s\n' 'herdr-remote-output-ready:1'\nexec \"$HOME/.local/bin/herdr\" remote-client-bridge"
        );
        assert_eq!(
            remote_herdr.executable.saved_bridge_command("agents"),
            "exec \"$HOME/.local/bin/herdr\" --session agents remote-client-bridge </dev/null"
        );
    }

    #[test]
    fn remote_path_discovery_uses_path_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr = remote_herdr_from_path_discovery(&remote_herdr, "/usr/bin/herdr\n")
            .expect("path binary");

        assert_eq!(
            remote_herdr
                .executable
                .bridge_command(crate::session::DEFAULT_SESSION_NAME),
            "printf '\n%s\n' 'herdr-remote-output-ready:1'\nexec /usr/bin/herdr remote-client-bridge"
        );
    }

    #[test]
    fn remote_path_discovery_quotes_discovered_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr =
            remote_herdr_from_path_discovery(&remote_herdr, "/opt/herdr bin/herdr\n")
                .expect("path binary");

        assert_eq!(
            remote_herdr
                .executable
                .bridge_command(crate::session::DEFAULT_SESSION_NAME),
            "printf '\n%s\n' 'herdr-remote-output-ready:1'\nexec '/opt/herdr bin/herdr' remote-client-bridge"
        );
    }

    #[test]
    fn remote_path_discovery_uses_macos_path_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "macos",
            arch: "aarch64",
        });
        let remote_herdr =
            remote_herdr_from_path_discovery(&remote_herdr, "/opt/homebrew/bin/herdr\n")
                .expect("path binary");

        assert_eq!(
            remote_herdr
                .executable
                .bridge_command(crate::session::DEFAULT_SESSION_NAME),
            "printf '\n%s\n' 'herdr-remote-output-ready:1'\nexec /opt/homebrew/bin/herdr remote-client-bridge"
        );
        assert_eq!(remote_herdr.platform.asset_key(), "macos-aarch64");
    }

    #[test]
    fn remote_path_discovery_reads_multiple_absolute_paths() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let candidates = remote_herdrs_from_path_discovery(
            &remote_herdr,
            "/usr/bin/herdr\nbin/herdr\n /opt/herdr bin/herdr\n",
        );

        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates[0].executable,
            RemoteExecutable::PosixShellPath("/usr/bin/herdr".to_string())
        );
        assert_eq!(
            candidates[1].executable,
            RemoteExecutable::PosixShellPath("'/opt/herdr bin/herdr'".to_string())
        );
    }

    #[test]
    fn remote_path_discovery_ignores_mise_shims() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let candidates = remote_herdrs_from_path_discovery(
            &remote_herdr,
            "/home/can/.local/share/mise/shims/herdr\n/home/can/.local/share/mise/installs/herdr/0.7.1/bin/herdr\n",
        );

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].executable,
            RemoteExecutable::PosixShellPath(
                "/home/can/.local/share/mise/installs/herdr/0.7.1/bin/herdr".to_string()
            )
        );
    }

    #[test]
    fn known_remote_binary_candidate_script_includes_mise_and_nix_paths() {
        let script = known_remote_binary_candidate_script(&RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });

        assert!(script.contains("emit \"$home/.local/bin/herdr\""));
        assert!(!script.contains("mise/shims/herdr"));
        assert!(script.contains(&format!("version={}", shell_quote(&current_version()))));
        assert!(
            script.contains("emit \"$home/.local/share/mise/installs/herdr/$version/bin/herdr\"")
        );
        assert!(script.contains("emit \"$home/.local/share/mise/installs/herdr/$version/herdr\""));
        assert!(script.contains(
            "emit \"$home/.local/share/mise/installs/github-ogulcancelik-herdr/$version/herdr\""
        ));
        assert!(script.contains("emit \"$home/.nix-profile/bin/herdr\""));
        assert!(script.contains("emit \"/etc/profiles/per-user/$user/bin/herdr\""));
        assert!(script.contains("emit \"/run/current-system/sw/bin/herdr\""));
        assert!(script.contains("emit \"/home/linuxbrew/.linuxbrew/bin/herdr\""));
        assert!(!script.contains("emit \"/opt/homebrew/bin/herdr\""));
    }

    #[test]
    fn known_remote_binary_candidate_script_includes_macos_homebrew_paths() {
        let script = known_remote_binary_candidate_script(&RemotePlatform {
            os: "macos",
            arch: "aarch64",
        });

        assert!(script.contains("emit \"/opt/homebrew/bin/herdr\""));
        assert!(script.contains("emit \"/usr/local/bin/herdr\""));
        assert!(!script.contains("emit \"/home/linuxbrew/.linuxbrew/bin/herdr\""));
    }

    #[test]
    fn remote_path_discovery_quotes_single_quotes_in_discovered_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr =
            remote_herdr_from_path_discovery(&remote_herdr, "/opt/herdr's/bin/herdr\n")
                .expect("path binary");

        assert_eq!(
            remote_herdr
                .executable
                .bridge_command(crate::session::DEFAULT_SESSION_NAME),
            "printf '\n%s\n' 'herdr-remote-output-ready:1'\nexec '/opt/herdr'\\''s/bin/herdr' remote-client-bridge"
        );
    }

    #[test]
    fn remote_path_discovery_ignores_relative_paths() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr = remote_herdr_from_path_discovery(&remote_herdr, "bin/herdr\n");

        assert!(remote_herdr.is_none());
    }

    #[test]
    fn remote_path_discovery_ignores_empty_output() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr = remote_herdr_from_path_discovery(&remote_herdr, "\n");

        assert!(remote_herdr.is_none());
    }

    #[test]
    fn remote_shell_path_warning_accepts_managed_install() {
        assert!(remote_shell_resolves_managed_install(
            "/home/can/.local/bin/herdr\n"
        ));
        assert!(remote_shell_resolves_managed_install(
            "/Users/can/.local/bin/herdr\n"
        ));
        assert!(!remote_shell_resolves_managed_install(
            "/usr/local/bin/herdr\n"
        ));
        assert!(!remote_shell_resolves_managed_install(""));
    }

    #[test]
    fn parse_client_status_json_reads_last_json_record() {
        let status = parse_client_status_json(
            "wrapper output\n{\"version\":\"0.8.0\",\"protocol\":20,\"endpoint_protocol_generation\":1,\"endpoint_capabilities\":[\"surface_interest\",\"health_check\"]}\n{\"wrapper\":true}\n",
        )
        .unwrap();
        assert_eq!(status.version.as_deref(), Some("0.8.0"));
        assert_eq!(status.protocol, Some(20));
        assert_eq!(status.endpoint_protocol_generation, Some(1));
        assert_eq!(
            status.endpoint_capabilities,
            vec!["surface_interest", "health_check"]
        );
        assert!(
            parse_client_status_json(r#"{"endpoint_protocol_generation":"unknown"}"#).is_none()
        );
    }

    #[test]
    fn saved_machine_setup_handoffs_old_server_missing_presentation_fence() {
        // Captured from Rohan after installing a new binary while the old daemon stayed alive.
        let installed = parse_client_status_json(
            r#"{"version":"0.8.2","protocol":22,"endpoint_protocol_generation":1,"endpoint_capabilities":["surface_interest","presentation_effects_fence","health_check"]}"#,
        )
        .unwrap();
        let running_binary = parse_client_status_json(
            r#"{"version":"0.8.2","protocol":22,"endpoint_protocol_generation":1,"endpoint_capabilities":["surface_interest","health_check"]}"#,
        )
        .unwrap();
        assert!(installed.supports_endpoint_requirement(true));
        assert!(!running_binary.supports_endpoint_requirement(true));
        for (live_capabilities, expected) in [
            (
                running_binary.endpoint_capabilities,
                RemoteInstallRunningServerPlan::LiveHandoff,
            ),
            (
                installed.endpoint_capabilities,
                RemoteInstallRunningServerPlan::KeepRunning,
            ),
        ] {
            let live_negotiation = crate::client::endpoint::EndpointNegotiation::new(
                vec!["client_shell.surface.set".into()],
                live_capabilities,
            );
            let RemoteServerStatus::Running {
                endpoint_protocol_generation,
                surface_interest,
                health_check,
                live_handoff,
                detached_server_daemon,
                ..
            } = parse_remote_server_status_json(
                r#"{"status":"running","running":true,"version":"0.8.2","protocol":22,"capabilities":{"live_handoff":true,"detached_server_daemon":true,"endpoint_protocol_generation":1,"surface_interest":true,"health_check":true}}"#,
            )
            .unwrap()
            .with_endpoint_negotiation(&live_negotiation) else {
                panic!("captured server must be running");
            };

            assert_eq!(
                remote_install_running_server_plan(
                    endpoint_protocol_generation,
                    detached_server_daemon,
                    surface_interest,
                    health_check,
                    live_handoff,
                    true,
                    true,
                ),
                expected,
                "setup must follow the running server's negotiated capabilities",
            );
        }
    }

    #[test]
    fn parse_remote_server_status_json_reads_running_server() {
        assert_eq!(
            parse_remote_server_status_json(
                r#"{"status":"running","running":true,"version":"0.6.0","protocol":8,"capabilities":{"live_handoff":true,"detached_server_daemon":true,"endpoint_protocol_generation":1,"surface_interest":true,"health_check":true}}"#
            )
            .unwrap(),
            RemoteServerStatus::Running {
                version: Some("0.6.0".into()),
                endpoint_protocol_generation: Some(1),
                surface_interest: true,
                health_check: true,
                live_handoff: true,
                detached_server_daemon: true
            }
        );
    }

    #[test]
    fn parse_remote_server_status_json_treats_missing_capability_as_old_server() {
        assert_eq!(
            parse_remote_server_status_json(
                r#"{"status":"running","running":true,"version":"0.6.0","protocol":8}"#
            )
            .unwrap(),
            RemoteServerStatus::Running {
                version: Some("0.6.0".into()),
                endpoint_protocol_generation: None,
                surface_interest: false,
                health_check: false,
                live_handoff: false,
                detached_server_daemon: false
            }
        );
    }

    #[test]
    fn parse_remote_server_status_json_reads_stopped_server() {
        assert_eq!(
            parse_remote_server_status_json(
                r#"{"status":"not_running","running":false,"version":null,"protocol":null}"#
            )
            .unwrap(),
            RemoteServerStatus::NotRunning
        );
    }

    #[test]
    fn remote_update_manifest_uses_root_assets_for_latest_version() {
        let manifest: RemoteUpdateManifest = serde_json::from_str(
            r#"{
                "version": "1.2.3",
                "assets": {
                    "linux-x86_64": "https://example.com/latest"
                },
                "sha256": {
                    "linux-x86_64": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                },
                "releases": {
                    "1.2.3": {
                        "assets": {
                            "linux-x86_64": "https://example.com/archive"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let release = manifest.release_for_version("1.2.3").unwrap();
        assert_eq!(
            release.assets.get("linux-x86_64").map(RemoteAssetRef::url),
            Some("https://example.com/latest")
        );
        assert_eq!(
            release.sha256.get("linux-x86_64").map(String::as_str),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
    }

    #[test]
    fn remote_update_manifest_reads_archived_release_assets() {
        let manifest: RemoteUpdateManifest = serde_json::from_str(
            r#"{
                "version": "1.2.4",
                "assets": {
                    "linux-x86_64": "https://example.com/latest"
                },
                "releases": {
                    "1.2.3": {
                        "notes": "ignored",
                        "assets": {
                            "linux-x86_64": "https://example.com/archive"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            manifest
                .release_for_version("1.2.3")
                .and_then(|release| release.assets.get("linux-x86_64"))
                .map(RemoteAssetRef::url),
            Some("https://example.com/archive")
        );
    }

    #[test]
    fn remote_update_manifest_uses_archived_release_protocol() {
        let manifest: RemoteUpdateManifest = serde_json::from_str(
            r#"{
                "version": "1.2.4",
                "protocol": 42,
                "assets": {
                    "linux-x86_64": "https://example.com/latest"
                },
                "releases": {
                    "1.2.3": {
                        "notes": "ignored",
                        "protocol": 41,
                        "assets": {
                            "linux-x86_64": "https://example.com/archive"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            manifest
                .release_for_version("1.2.3")
                .and_then(|release| release.protocol),
            Some(41)
        );
    }

    #[test]
    fn remote_update_manifest_does_not_inherit_latest_protocol_for_archived_assets() {
        let manifest: RemoteUpdateManifest = serde_json::from_str(
            r#"{
                "version": "1.2.4",
                "protocol": 42,
                "assets": {
                    "linux-x86_64": "https://example.com/latest"
                },
                "releases": {
                    "1.2.3": {
                        "notes": "ignored",
                        "assets": {
                            "linux-x86_64": "https://example.com/archive"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            manifest
                .release_for_version("1.2.3")
                .and_then(|release| release.protocol),
            None
        );
    }

    #[test]
    fn remote_preview_manifest_falls_back_to_archived_exact_build_assets() {
        let manifest: RemotePreviewManifest = serde_json::from_str(
            r#"{
                "build_id": "2026-06-06-new",
                "protocol": 12,
                "assets": {
                    "linux-x86_64": {
                        "url": "https://example.com/new",
                        "sha256": "new"
                    }
                },
                "builds": {
                    "2026-06-02-old": {
                        "protocol": 11,
                        "assets": {
                            "linux-x86_64": {
                                "url": "https://example.com/old",
                                "sha256": "old"
                            }
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let (protocol, assets) =
            preview_assets_for_build(&manifest, "2026-06-02-old").expect("archived build");
        let asset = assets.get("linux-x86_64").expect("asset");
        assert_eq!(protocol, 11);
        assert_eq!(asset.url(), "https://example.com/old");
        assert_eq!(asset.sha256(), Some("old"));
    }

    #[test]
    fn remote_live_handoff_uses_prepared_binary_identity() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let command = remote_herdr.executable.live_handoff_command(
            crate::session::DEFAULT_SESSION_NAME,
            19,
            "0.7.9",
        );
        assert!(command.contains("--expected-protocol 19"));
        assert!(command.contains("--expected-version 0.7.9"));
        assert!(!command.contains(&format!(
            "--expected-protocol {CURRENT_PROTOCOL} --expected-version {}",
            current_version()
        )));
    }

    #[test]
    fn install_source_description_uses_override_binary() {
        let platform = RemotePlatform {
            os: "linux",
            arch: "aarch64",
        };
        assert_eq!(
            install_source_description_for(&platform, Some(Path::new("/tmp/herdr-aarch64")), false),
            "HERDR_REMOTE_BINARY (/tmp/herdr-aarch64)"
        );
    }

    #[test]
    fn install_source_description_uses_local_binary_when_allowed() {
        let platform = RemotePlatform::local();

        assert_eq!(
            install_source_description_for(&platform, None, true),
            "the current local herdr binary"
        );
    }

    #[test]
    fn install_source_description_uses_release_asset_when_local_binary_cannot_seed_remote() {
        let platform = RemotePlatform::local();

        assert_eq!(
            install_source_description_for(&platform, None, false),
            format!(
                "the {} {} asset for {}",
                current_version(),
                current_channel(),
                platform.asset_key()
            )
        );
    }

    #[test]
    fn resolve_install_source_uses_override_binary_without_temporary_cleanup() {
        let platform = RemotePlatform {
            os: "linux",
            arch: "aarch64",
        };
        let source = resolve_install_source(&platform, Some(PathBuf::from("/tmp/herdr-aarch64")))
            .expect("override source");
        assert_eq!(source.path, PathBuf::from("/tmp/herdr-aarch64"));
        assert!(source.temporary_dir.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_local_forward_endpoint_uses_private_state_dir() {
        let path = local_forward_socket_path("user@example.com", "work");
        assert!(path.starts_with(crate::platform::remote_private_temp_base()));
        assert!(path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("herdr-r-")));
    }

    #[cfg(unix)]
    fn remote_env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    #[cfg(unix)]
    fn socket_path_byte_len(path: &Path) -> usize {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().len()
    }

    #[cfg(unix)]
    #[test]
    fn local_forward_socket_path_uses_readable_name_when_it_fits() {
        let _guard = remote_env_lock().lock().unwrap();
        // Short target + session leave plenty of room — keep the human-
        // readable form so the socket path stays grep-friendly.
        let path = local_forward_socket_path("dev", "default");
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        assert!(
            filename.starts_with("herdr-remote-"),
            "expected readable name, got {filename}"
        );
        assert!(filename.contains("-dev-default."), "got {filename}");
        assert!(
            fits_unix_socket_path(&path),
            "socket path too long: {} ({} bytes)",
            path.display(),
            socket_path_byte_len(&path)
        );
    }

    #[cfg(unix)]
    #[test]
    fn local_forward_socket_path_fits_in_sun_path() {
        let _guard = remote_env_lock().lock().unwrap();
        // Worst case for the readable form: macOS-style 49-char TMPDIR +
        // max-length sanitized components. Should fall back to the hashed
        // short name, which fits under TMPDIR.
        let target = "longish-host.example.com";
        let session = "a-fairly-long-session-name-here";
        let path = local_forward_socket_path(target, session);
        assert!(
            fits_unix_socket_path(&path),
            "socket path too long for sun_path: {} ({} bytes)",
            path.display(),
            socket_path_byte_len(&path)
        );
    }

    #[cfg(unix)]
    #[test]
    fn local_forward_socket_path_falls_back_to_tmp_when_dir_is_long() {
        let _guard = remote_env_lock().lock().unwrap();
        // Force a TMPDIR long enough that even the hashed short name cannot
        // fit inside it. The fallback should drop to /tmp.
        let prior = std::env::var_os("TMPDIR");
        let long_dir = std::env::temp_dir().join("a".repeat(80));
        let _ = fs::create_dir_all(&long_dir);
        std::env::set_var("TMPDIR", &long_dir);

        let path = local_forward_socket_path("longish-host.example.com", "default");
        let fits = fits_unix_socket_path(&path);
        let parent = path.parent().map(Path::to_path_buf);
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        match prior {
            Some(v) => std::env::set_var("TMPDIR", v),
            None => std::env::remove_var("TMPDIR"),
        }
        let _ = fs::remove_dir_all(&long_dir);

        assert!(fits, "fallback path still overflows: {}", path.display());
        assert_eq!(parent.as_deref(), Some(Path::new("/tmp")));
        assert!(
            filename.starts_with("herdr-r-"),
            "expected hashed fallback, got {filename}"
        );
    }

    #[test]
    fn install_source_cleanup_removes_temporary_directory() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-install-source-cleanup-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).expect("create temp dir");
        let path = dir.join("herdr.tmp");
        fs::write(&path, b"test").expect("write temp file");

        InstallSource::temporary(path, dir.clone()).cleanup();

        assert!(!dir.exists());
    }
}
