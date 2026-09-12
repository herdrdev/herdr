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
    let prepared = if require_desktop {
        PreparedRemoteHerdr {
            remote_herdr: find_desktop_remote_herdr(ssh, require_surface_interest)?,
            stop_after_install_approved: false,
        }
    } else {
        prepare_remote_herdr(ssh, live_handoff, require_surface_interest)?
    };
    let remote = prepared.remote_herdr;
    if require_desktop {
        // Refuse a server in another login before offering to stop or hand it off.
        inspect_desktop_placement(ssh, &remote)?;
    }
    ensure_remote_server_ready(
        ssh,
        &remote,
        prepared.stop_after_install_approved,
        live_handoff,
        require_surface_interest,
    )?;
    if require_desktop {
        ensure_desktop_server_ready(ssh, &remote)?;
    }
    Ok(remote)
}

fn remote_binary_supports_host_requirement(
    ssh: &RemoteSsh,
    remote: &RemoteHerdr,
    require_surface_interest: bool,
) -> io::Result<bool> {
    Ok(remote_client_status(ssh, remote)?.is_some_and(|status| {
        status.supports_endpoint_requirement(require_surface_interest)
            && (!remote.require_desktop || status.remote_desktop_host)
    }))
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

fn remote_desktop_inspection(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<RemoteDesktopInspection> {
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

pub(super) fn inspect_desktop_placement(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<()> {
    match remote_desktop_inspection(ssh, remote_herdr)? {
        RemoteDesktopInspection::Ready { .. } | RemoteDesktopInspection::Start { .. } => Ok(()),
        inspection => desktop_inspection_error(inspection),
    }
}

pub(super) fn ensure_desktop_server_ready(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<()> {
    let windows_session = match remote_desktop_inspection(ssh, remote_herdr)? {
        RemoteDesktopInspection::Ready { .. } => return Ok(()),
        RemoteDesktopInspection::Start { windows_session } => windows_session,
        inspection => return desktop_inspection_error(inspection),
    };
    if !io::stdin().is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "starting Herdr in the signed-in Windows desktop requires approval in an interactive terminal",
        ));
    }
    eprint!(
        "Start Herdr in your signed-in Windows desktop? Agents can interact with desktop apps. A one-time Windows task will launch Herdr and be removed once it's ready. [y/N] "
    );
    io::stderr().flush()?;
    if !read_remote_confirmation(&mut io::stdin().lock(), false)? {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Windows desktop server start cancelled",
        ));
    }
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
    match remote_desktop_inspection(ssh, remote_herdr)? {
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

pub(super) fn find_desktop_remote_herdr(
    ssh: &RemoteSsh,
    require_surface_interest: bool,
) -> io::Result<RemoteHerdr> {
    let remote = remote_host_for_platform(detect_remote_platform(ssh)?, true)?;
    for candidate in desktop_binary_candidates(ssh, &remote)? {
        if remote_binary_supports_host_requirement(ssh, &candidate, require_surface_interest)? {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(io::ErrorKind::Unsupported, format!("no desktop-capable Herdr package is installed on {}; install or update the Windows package before retrying --remote-desktop", ssh.target())))
}

#[cfg(test)]
mod tests {
    use super::*;

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
