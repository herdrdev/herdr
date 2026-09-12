use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::{io, path::PathBuf};

const BOOTSTRAP_ARG: &str = "--desktop-bootstrap";

#[derive(Serialize, Deserialize)]
struct DesktopBootstrap {
    config_path: Option<String>,
    xdg_config_home: Option<String>,
    xdg_state_home: Option<String>,
    startup_cwd: String,
}

pub(crate) fn apply_desktop_bootstrap_args(args: &[String]) -> Result<Vec<String>, String> {
    let (cleaned, bootstrap) = extract_bootstrap_args(args)?;
    if let Some(bootstrap) = bootstrap {
        apply_bootstrap(&bootstrap)?;
    }
    Ok(cleaned)
}

fn extract_bootstrap_args(
    args: &[String],
) -> Result<(Vec<String>, Option<DesktopBootstrap>), String> {
    let mut cleaned = Vec::with_capacity(args.len());
    if let Some(program) = args.first() {
        cleaned.push(program.clone());
    }

    let mut bootstrap = None;
    let mut index = 1;
    while index < args.len() {
        if args[index] == "--" {
            cleaned.extend_from_slice(&args[index..]);
            break;
        }
        if args[index] == BOOTSTRAP_ARG {
            if bootstrap.is_some() {
                return Err(format!("{BOOTSTRAP_ARG} can only be specified once"));
            }
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("missing value for {BOOTSTRAP_ARG}"))?;
            bootstrap = Some(decode_bootstrap(value)?);
            index += 2;
            continue;
        }
        cleaned.push(args[index].clone());
        index += 1;
    }

    Ok((cleaned, bootstrap))
}

pub(crate) fn run_remote_desktop_command(args: &[String]) -> io::Result<()> {
    match args {
        [operation] if operation == "inspect" => {
            let mut report = serde_json::to_value(super::inspect_remote_desktop_host()?)
                .map_err(io::Error::other)?;
            report["account_sid"] = base64::engine::general_purpose::STANDARD
                .encode(super::desktop_host::desktop_account_sid()?)
                .into();
            report["host"] = super::hostname()
                .ok_or_else(|| io::Error::other("Windows host name is unavailable"))?
                .into();
            println!("{report}");
            Ok(())
        }
        [operation, flag, raw_session] if operation == "start" && flag == "--windows-session" => {
            let windows_session = raw_session.parse::<u32>().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "--windows-session must be an unsigned integer",
                )
            })?;
            super::start_remote_desktop_server(windows_session, &capture_bootstrap()?)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: herdr remote-desktop <inspect|start --windows-session <id>>",
        )),
    }
}

fn capture_bootstrap() -> io::Result<String> {
    let bootstrap = DesktopBootstrap {
        config_path: unicode_env(crate::config::CONFIG_PATH_ENV_VAR)?,
        xdg_config_home: unicode_env("XDG_CONFIG_HOME")?,
        xdg_state_home: unicode_env("XDG_STATE_HOME")?,
        startup_cwd: std::env::current_dir()?
            .into_os_string()
            .into_string()
            .map_err(|_| io::Error::other("SSH working directory is not valid Unicode"))?,
    };
    let bytes = serde_json::to_vec(&bootstrap).map_err(io::Error::other)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

fn unicode_env(name: &str) -> io::Result<Option<String>> {
    std::env::var_os(name)
        .map(|value| {
            value.into_string().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{name} is not valid Unicode"),
                )
            })
        })
        .transpose()
}

fn decode_bootstrap(value: &str) -> Result<DesktopBootstrap, String> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|error| format!("invalid {BOOTSTRAP_ARG}: {error}"))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("invalid {BOOTSTRAP_ARG}: {error}"))
}

fn apply_bootstrap(bootstrap: &DesktopBootstrap) -> Result<(), String> {
    crate::platform::detach_remote_desktop_console()
        .map_err(|error| format!("failed to detach the Windows desktop server console: {error}"))?;
    apply_env(crate::config::CONFIG_PATH_ENV_VAR, &bootstrap.config_path);
    apply_env("XDG_CONFIG_HOME", &bootstrap.xdg_config_home);
    apply_env("XDG_STATE_HOME", &bootstrap.xdg_state_home);
    std::env::set_var(
        crate::server::autodetect::STARTUP_CWD_ENV_VAR,
        &bootstrap.startup_cwd,
    );
    std::env::set_current_dir(PathBuf::from(&bootstrap.startup_cwd)).map_err(|error| {
        format!(
            "failed to restore desktop server working directory {}: {error}",
            bootstrap.startup_cwd
        )
    })
}

fn apply_env(name: &str, value: &Option<String>) {
    match value {
        Some(value) => std::env::set_var(name, value),
        None => std::env::remove_var(name),
    }
}

pub(crate) fn desktop_server_args(bootstrap: &str) -> Vec<String> {
    let mut args = vec![BOOTSTRAP_ARG.to_owned(), bootstrap.to_owned()];
    if let Some(session) = crate::session::active_name() {
        args.extend(["--session".to_owned(), session]);
    }
    args.push("server".to_owned());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_arguments_are_removed_before_normal_dispatch() {
        let bootstrap = DesktopBootstrap {
            config_path: None,
            xdg_config_home: None,
            xdg_state_home: None,
            startup_cwd: std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        };
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&bootstrap).unwrap());
        let args = vec![
            "herdr".into(),
            BOOTSTRAP_ARG.into(),
            encoded,
            "--session".into(),
            "agents".into(),
            "server".into(),
        ];

        assert_eq!(
            extract_bootstrap_args(&args).unwrap().0,
            vec!["herdr", "--session", "agents", "server"]
        );
    }
}
