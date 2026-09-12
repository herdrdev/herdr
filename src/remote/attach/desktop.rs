use super::*;

pub(super) fn remote_host_for_platform(
    platform: RemotePlatform,
    require_desktop: bool,
) -> io::Result<RemoteHerdr> {
    if require_desktop && !platform.is_windows() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "--remote-desktop requires a Windows SSH target",
        ));
    }
    let mut remote = RemoteHerdr::for_platform(platform);
    remote.require_desktop = require_desktop;
    Ok(remote)
}

pub(super) fn prepare_remote_host(
    ssh: &RemoteSsh,
    live_handoff: bool,
    require_surface_interest: bool,
    require_desktop: bool,
) -> io::Result<RemoteHerdr> {
    let platform = detect_remote_platform(ssh)?;
    if platform.is_windows() {
        return prepare_windows_desktop_offer(
            ssh,
            platform,
            require_surface_interest,
            require_desktop,
        );
    }
    remote_host_for_platform(platform.clone(), require_desktop)?;
    let prepared = prepare_remote_herdr(ssh, live_handoff, require_surface_interest, platform)?;
    ensure_remote_server_ready(
        ssh,
        &prepared.remote_herdr,
        prepared.stop_after_install_approved,
        live_handoff,
        require_surface_interest,
    )?;
    Ok(prepared.remote_herdr)
}

fn prepare_windows_desktop_offer(
    ssh: &RemoteSsh,
    platform: RemotePlatform,
    require_surface_interest: bool,
    require_desktop: bool,
) -> io::Result<RemoteHerdr> {
    let mut remote =
        find_windows_remote_host(ssh, platform, require_surface_interest, require_desktop)?;
    if remote.require_desktop {
        let report = remote_desktop_inspection(ssh, &remote)?;
        match report.placement {
            RemoteDesktopInspection::Ready { .. } => {
                check_existing_server(
                    remote_server_status(ssh, &remote, require_surface_interest)?,
                    require_surface_interest,
                )?;
                return Ok(remote);
            }
            RemoteDesktopInspection::Conflict { .. } => {
                if require_desktop {
                    desktop_inspection_error(report.placement)?;
                }
                eprintln!("Herdr is already running outside the signed-in desktop. Continuing without desktop access. To switch, stop that server explicitly or choose another --session.");
                remote.require_desktop = false;
                // Inspect compatibility without offering to stop the existing server.
                check_existing_server(
                    remote_server_status(ssh, &remote, require_surface_interest)?,
                    require_surface_interest,
                )?;
                return Ok(remote);
            }
            RemoteDesktopInspection::Start { windows_session } => {
                let consent_path =
                    desktop_consent_path(&crate::config::state_dir(), ssh.target(), &report);
                let choice = if !io::stdin().is_terminal() {
                    DesktopChoice::No
                } else if consent_path.is_file() {
                    DesktopChoice::Always
                } else {
                    eprintln!("Allow agents to use apps in your signed-in Windows desktop? Herdr uses a temporary Task Scheduler task to start there.");
                    eprintln!(
                        "  y - Yes, once
  a - Yes, always for this machine and account
  n - No, continue without desktop access"
                    );
                    eprint!("[y/a/N] ");
                    io::stderr().flush()?;
                    read_desktop_choice(&mut io::stdin().lock())?
                };
                if choice == DesktopChoice::No {
                    if require_desktop {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "Windows desktop server start cancelled",
                        ));
                    }
                    remote.require_desktop = false;
                } else {
                    start_desktop_server(ssh, &remote, windows_session)?;
                    if choice == DesktopChoice::Always {
                        remember_desktop_consent(&consent_path)?;
                    }
                }
            }
            inspection => {
                if require_desktop {
                    desktop_inspection_error(inspection)?;
                }
                eprintln!("No unique signed-in Windows desktop is available. Continuing without desktop access.");
                remote.require_desktop = false;
            }
        }
    }
    ensure_remote_server_ready(ssh, &remote, false, false, require_surface_interest)?;
    Ok(remote)
}

fn check_existing_server(
    status: RemoteServerStatus,
    require_surface_interest: bool,
) -> io::Result<()> {
    let RemoteServerStatus::Running {
        endpoint_protocol_generation,
        surface_interest,
        health_check,
        detached_server_daemon,
        ..
    } = status
    else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the remote Herdr server stopped before attachment; reconnect to prepare it",
        ));
    };
    if remote_server_restart_reason(
        endpoint_protocol_generation,
        detached_server_daemon,
        require_surface_interest,
        surface_interest,
        health_check,
    )
    .is_some()
    {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "the existing remote Herdr server is incompatible and was left running; stop it explicitly or choose another --session"));
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum DesktopChoice {
    Once,
    Always,
    No,
}

fn read_desktop_choice(input: &mut impl io::BufRead) -> io::Result<DesktopChoice> {
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    Ok(match answer.trim().to_ascii_lowercase().as_str() {
        "y" => DesktopChoice::Once,
        "a" => DesktopChoice::Always,
        _ => DesktopChoice::No,
    })
}

fn desktop_consent_path(state: &Path, target: &str, report: &RemoteDesktopReport) -> PathBuf {
    use sha2::{Digest as _, Sha256};
    let mut hash = Sha256::new();
    for part in [target, &report.host, &report.account_sid] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    state
        .join("remote-desktop-approvals")
        .join(format!("{:x}", hash.finalize()))
}

fn remember_desktop_consent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match crate::platform::create_private_state_file(path) {
        Ok(file) => file.sync_all(),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_file() => Ok(()),
        Err(error) => Err(error),
    }
}

impl RemoteHerdr {
    pub(super) fn bridge_command(&self, session: &str) -> String {
        if self.require_desktop {
            self.desktop_bridge_command(session)
        } else {
            self.executable.bridge_command(session)
        }
    }

    pub(super) fn saved_bridge_command(&self, session: &str) -> String {
        if self.require_desktop {
            self.desktop_bridge_command(session)
        } else {
            self.executable.saved_bridge_command(session)
        }
    }

    fn desktop_bridge_command(&self, session: &str) -> String {
        let args =
            RemoteExecutable::session_args(session, &["remote-client-bridge", "--require-desktop"]);
        match &self.executable {
            RemoteExecutable::WindowsPath(path) => {
                windows_powershell_streaming_application_command(path, &args)
            }
            RemoteExecutable::PosixShellPath(_) => {
                posix_remote_output_command(&format!("exec {}", self.executable.command(&args)))
            }
        }
    }
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
enum RemoteDesktopInspection {
    Ready { pid: u32, windows_session: u32 },
    Start { windows_session: u32 },
    Conflict { pid: u32, windows_session: u32 },
    NoLogin,
    MultipleLogins,
}

#[derive(Debug, Deserialize)]
struct RemoteDesktopReport {
    #[serde(flatten)]
    placement: RemoteDesktopInspection,
    account_sid: String,
    host: String,
}

fn remote_desktop_inspection(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<RemoteDesktopReport> {
    let command = remote_herdr
        .executable
        .desktop_inspect_command(&ssh.session_name);
    let output = ssh.shell_output(&remote_herdr.platform, &command)?;
    if !output.status.success() {
        return Err(command_failed(
            "remote Windows desktop inspection failed",
            &output,
        ));
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .next_back()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "remote Herdr did not report Windows desktop placement",
            )
        })
}

fn start_desktop_server(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    windows_session: u32,
) -> io::Result<()> {
    let command = remote_herdr
        .executable
        .desktop_start_command(&ssh.session_name, windows_session);
    let output = ssh.shell_output(&remote_herdr.platform, &command)?;
    if !output.status.success() {
        return Err(command_failed(
            "remote Windows desktop start failed",
            &output,
        ));
    }
    match remote_desktop_inspection(ssh, remote_herdr)?.placement {
        RemoteDesktopInspection::Ready {
            windows_session: actual,
            ..
        } if actual == windows_session => Ok(()),
        inspection => desktop_inspection_error(inspection),
    }
}

fn desktop_inspection_error(inspection: RemoteDesktopInspection) -> io::Result<()> {
    match inspection {
        RemoteDesktopInspection::Conflict {
            pid,
            windows_session,
        } => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "remote Herdr server process {pid} is running in nonqualifying Windows session {windows_session}; use another --session or stop it explicitly"
            ),
        )),
        RemoteDesktopInspection::NoLogin => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no active desktop login exists for this Windows account; sign in to Windows first",
        )),
        RemoteDesktopInspection::MultipleLogins => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "more than one active desktop login exists for this Windows account; disconnect the extra login before starting Herdr",
        )),
        RemoteDesktopInspection::Start { .. } => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the Windows desktop Herdr server did not become ready",
        )),
        RemoteDesktopInspection::Ready { .. } => Err(io::Error::other(
            "the Windows desktop Herdr server started in a different login",
        )),
    }
}

impl RemoteExecutable {
    fn desktop_inspect_command(&self, session_name: &str) -> String {
        self.session_command(session_name, &["remote-desktop", "inspect"])
    }

    fn desktop_start_command(&self, session_name: &str, windows_session: u32) -> String {
        let windows_session = windows_session.to_string();
        self.session_command(
            session_name,
            &[
                "remote-desktop",
                "start",
                "--windows-session",
                &windows_session,
            ],
        )
    }
}

const WINDOWS_REMOTE_PATH_MARKER: &str = "herdr-remote-path:1:";
fn windows_remote_binary_candidate_command() -> String {
    windows_powershell_script_command(&format!(
        r#"function Emit-HerdrPath([string]$CandidatePath) {{ if ([string]::IsNullOrWhiteSpace($CandidatePath) -or -not (Test-Path -LiteralPath $CandidatePath -PathType Leaf)) {{ return }}; $candidateFullPath = [System.IO.Path]::GetFullPath($CandidatePath); $encodedCandidate = [System.Convert]::ToBase64String([System.Text.Encoding]::UTF8.GetBytes($candidateFullPath)); [Console]::Out.WriteLine('{WINDOWS_REMOTE_PATH_MARKER}' + $encodedCandidate) }}; $pathCommand = Get-Command herdr.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1; if ($null -ne $pathCommand) {{ Emit-HerdrPath $pathCommand.Source }}; $herdrHome = if ([string]::IsNullOrWhiteSpace($env:HERDR_HOME)) {{ Join-Path $env:USERPROFILE '.herdr' }} else {{ $env:HERDR_HOME }}; $activeJunction = Get-Item -LiteralPath (Join-Path $herdrHome 'packages\standalone\current') -Force -ErrorAction SilentlyContinue; if ($null -ne $activeJunction -and -not [string]::IsNullOrWhiteSpace([string]$activeJunction.Target)) {{ Emit-HerdrPath (Join-Path ([string]$activeJunction.Target) 'herdr.exe') }}; exit 0"#
    ))
}

fn windows_remote_binary_candidates(remote_herdr: &RemoteHerdr, stdout: &str) -> Vec<RemoteHerdr> {
    let mut candidates = Vec::new();
    for path in stdout.lines().filter_map(|line| {
        line.trim()
            .strip_prefix(WINDOWS_REMOTE_PATH_MARKER)
            .and_then(decode_windows_remote_path)
    }) {
        push_if_new_remote_binary_candidate(
            &mut candidates,
            RemoteHerdr {
                executable: RemoteExecutable::WindowsPath(path),
                ..remote_herdr.clone()
            },
        );
    }
    candidates
}

fn decode_windows_remote_path(encoded: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let path = String::from_utf8(bytes).ok()?;
    (!path.is_empty()).then_some(path)
}

pub(super) fn desktop_binary_candidates(
    ssh: &RemoteSsh,
    remote: &RemoteHerdr,
) -> io::Result<Vec<RemoteHerdr>> {
    let output = ssh.framed_user_shell_output(&windows_remote_binary_candidate_command())?;
    if !output.status.success() {
        return Err(command_failed("remote binary discovery failed", &output));
    }
    Ok(windows_remote_binary_candidates(
        remote,
        &String::from_utf8_lossy(&output.stdout),
    ))
}

pub(super) fn find_windows_remote_host(
    ssh: &RemoteSsh,
    platform: RemotePlatform,
    require_surface_interest: bool,
    require_desktop: bool,
) -> io::Result<RemoteHerdr> {
    let remote = remote_host_for_platform(platform, true)?;
    let mut ordinary = None;
    for mut candidate in desktop_binary_candidates(ssh, &remote)? {
        if let Some(status) = remote_client_status(ssh, &candidate)? {
            if status.supports_endpoint_requirement(require_surface_interest) {
                if status.remote_desktop_host {
                    return Ok(candidate);
                }
                candidate.require_desktop = false;
                ordinary.get_or_insert(candidate);
            }
        }
    }
    if !require_desktop {
        if let Some(remote) = ordinary {
            return Ok(remote);
        }
    }
    Err(io::Error::new(io::ErrorKind::Unsupported, format!("no compatible {}Herdr package is installed on {}; install or update the Windows package before retrying", if require_desktop { "desktop-capable " } else { "" }, ssh.target())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_server_reuse_requires_capabilities_and_detached_lifecycle() {
        let generation = Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION);
        for (protocol, detached, surface, health, saved, ready) in [
            (generation, true, true, true, true, true),
            (generation, true, false, false, false, true),
            (None, true, true, true, true, false),
            (generation, false, true, true, true, false),
            (generation, true, false, true, true, false),
            (generation, true, true, false, true, false),
        ] {
            let status = RemoteServerStatus::Running {
                version: None,
                endpoint_protocol_generation: protocol,
                surface_interest: surface,
                health_check: health,
                live_handoff: false,
                detached_server_daemon: detached,
            };
            assert_eq!(check_existing_server(status, saved).is_ok(), ready);
        }
        assert_eq!(
            check_existing_server(RemoteServerStatus::NotRunning, false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn desktop_consent_defaults_to_no_and_remembers_only_the_matching_account() {
        for (answer, expected) in [
            ("y\n", DesktopChoice::Once),
            ("A\n", DesktopChoice::Always),
            ("n\n", DesktopChoice::No),
            ("\n", DesktopChoice::No),
            ("", DesktopChoice::No),
            ("yes please\n", DesktopChoice::No),
        ] {
            assert_eq!(
                read_desktop_choice(&mut answer.as_bytes()).unwrap(),
                expected
            );
        }
        let root =
            std::env::temp_dir().join(format!("herdr-desktop-consent-{}", std::process::id()));
        let mut report = RemoteDesktopReport {
            placement: RemoteDesktopInspection::Start { windows_session: 1 },
            host: "workbox".into(),
            account_sid: "account-one".into(),
        };
        let path = desktop_consent_path(&root, "work", &report);
        remember_desktop_consent(&path).unwrap();
        assert!(desktop_consent_path(&root, "work", &report).is_file());
        report.placement = RemoteDesktopInspection::Start { windows_session: 3 };
        assert_eq!(desktop_consent_path(&root, "work", &report), path);
        assert_ne!(desktop_consent_path(&root, "other-target", &report), path);
        report.host = "other-host".into();
        assert_ne!(desktop_consent_path(&root, "work", &report), path);
        report.host = "workbox".into();
        report.account_sid = "account-two".into();
        assert_ne!(desktop_consent_path(&root, "work", &report), path);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(path.parent().unwrap()).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn desktop_selection_rejects_unix_and_preserves_intent_across_discovery() {
        let unix = RemotePlatform {
            os: "linux",
            arch: "x86_64",
        };
        assert_eq!(
            remote_host_for_platform(unix, true).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        let host = remote_host_for_platform(
            RemotePlatform {
                os: "windows",
                arch: "x86_64",
            },
            true,
        )
        .unwrap();
        let path = base64::engine::general_purpose::STANDARD.encode("C:\\Herdr Test\\herdr.exe");
        let hosts =
            windows_remote_binary_candidates(&host, &format!("{WINDOWS_REMOTE_PATH_MARKER}{path}"));
        let host = &hosts[0];
        for command in [
            host.bridge_command("agents"),
            host.saved_bridge_command("agents"),
            remote_api_bridge_command(host, "agents", false),
            remote_api_bridge_command(host, "agents", true),
        ] {
            let encoded = command.split_whitespace().last().unwrap();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap();
            let units = bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect::<Vec<_>>();
            let script = String::from_utf16(&units).unwrap();
            assert!(script.contains("--session agents"), "{script}");
            assert!(script.contains("--require-desktop"), "{script}");
            assert!(script.contains("C:\\Herdr Test\\herdr.exe"), "{script}");
        }
    }
}
