//! Pane-local zsh startup files; user dotfiles remain untouched.

use portable_pty::CommandBuilder;
use std::{io, path::PathBuf, sync::Mutex};

const FILES: [(&str, &str); 3] = [
    (".zshenv", include_str!("zsh/zshenv")),
    (".zprofile", include_str!("zsh/zprofile")),
    (".zshrc", include_str!("zsh/zshrc")),
];

/// Install versioned zsh startup wrappers and redirect this pane to them.
///
/// Preserve the original ZDOTDIR for sourcing user dotfiles. Other shells are
/// unchanged; installation or directory validation failures return an error
/// before modifying the command environment.
pub(super) fn apply(cmd: &mut CommandBuilder, shell: &str) -> io::Result<()> {
    if std::path::Path::new(shell)
        .file_name()
        .is_none_or(|name| name != "zsh")
    {
        return Ok(());
    }
    // Versioned immutable assets allow old sessions to keep their startup files.
    static DIRECTORY: Mutex<Option<PathBuf>> = Mutex::new(None);
    let directory = startup_directory(&DIRECTORY, || {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        for (_, contents) in FILES {
            hash.update(contents);
        }
        let path = std::path::absolute(crate::config::state_dir())?
            .join("shell-integration")
            .join(format!("zsh-{:x}", hash.finalize()));
        crate::platform::prepare_shell_integration_directory(&path)?;
        for (name, contents) in FILES {
            let file = path.join(name);
            {
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(io::Error::other)?
                    .as_nanos();
                let temporary = path.join(format!("{name}.{}.{nonce}", std::process::id()));
                use std::io::Write;
                let mut output = crate::platform::create_private_state_file(&temporary)?;
                let result = output
                    .write_all(contents.as_bytes())
                    .and_then(|()| crate::platform::replace_file(&temporary, &file));
                if result.is_err() {
                    let _ = std::fs::remove_file(&temporary);
                }
                result?;
            }
        }
        Ok(path)
    })?;
    crate::platform::prepare_shell_integration_directory(&directory)?;
    if let Some(original) = cmd.get_env("ZDOTDIR").map(std::ffi::OsStr::to_owned) {
        cmd.env("_HERDR_ZSH_ZDOTDIR", original);
    } else {
        cmd.env_remove("_HERDR_ZSH_ZDOTDIR");
    }
    cmd.env("_HERDR_ZSH_INIT_DIR", &directory);
    cmd.env("ZDOTDIR", directory);
    Ok(())
}

/// Serialize asset installation and cache only successful initialization.
fn startup_directory(
    cache: &Mutex<Option<PathBuf>>,
    initialize: impl FnOnce() -> io::Result<PathBuf>,
) -> io::Result<PathBuf> {
    let mut directory = cache
        .lock()
        .map_err(|_| io::Error::other("zsh startup cache poisoned"))?;
    if directory
        .as_ref()
        .is_some_and(|path| FILES.iter().any(|(name, _)| !path.join(name).is_file()))
    {
        *directory = None;
    }
    if directory.is_none() {
        *directory = Some(initialize()?);
    }
    directory
        .clone()
        .ok_or_else(|| io::Error::other("zsh startup directory missing"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn startup_initialization_retries_after_failure() {
        let cache = Mutex::new(None);
        let root = std::env::temp_dir().join(format!("herdr-zsh-cache-{}", std::process::id()));
        let install = || {
            std::fs::create_dir_all(&root)?;
            for (name, contents) in FILES {
                std::fs::write(root.join(name), contents)?;
            }
            Ok(root.clone())
        };
        assert!(startup_directory(&cache, || Err(io::Error::other("temporary failure"))).is_err());
        assert_eq!(startup_directory(&cache, install).unwrap(), root);
        assert_eq!(
            startup_directory(&cache, || panic!("already initialized")).unwrap(),
            root
        );
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(startup_directory(&cache, install).unwrap(), root);
        assert!(FILES.iter().all(|(name, _)| root.join(name).is_file()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn zsh_startup_preserves_dotfiles_and_prompt_hooks() {
        if Command::new("zsh").arg("--version").output().is_err() {
            return; // zsh is optional on non-macOS test hosts.
        }
        let root = std::env::temp_dir().join(format!("herdr-zsh-startup-{}", std::process::id()));
        let startup = root.join("startup");
        let user = root.join("user");
        let redirected = root.join("redirected");
        for dir in [&startup, &user, &redirected] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for (name, contents) in FILES {
            std::fs::write(startup.join(name), contents).unwrap();
        }
        std::fs::write(
            user.join(".zshenv"),
            "export ZDOTDIR=$TEST_REDIRECTED; order=env",
        )
        .unwrap();
        std::fs::write(redirected.join(".zprofile"), "order+=,profile").unwrap();
        std::fs::write(
            redirected.join(".zshrc"),
            "order+=,rc; PS1=$'plain\\n> '; PS2='continue> '",
        )
        .unwrap();
        std::fs::write(redirected.join(".zlogin"), "order+=,login").unwrap();
        let script = r#"
[[ $order == env,profile,rc,login && $ZDOTDIR == $TEST_REDIRECTED ]] || exit 10
[[ -z ${_HERDR_ZSH_INIT_DIR+x} && -z ${_HERDR_ZSH_ZDOTDIR+x} ]] || exit 11
_herdr_zsh_precmd
[[ $PS1 == *$'\e]133;A;redraw=1'* && $PS1 == *$'\e]133;B'* ]] || exit 12
first=$PS1
_herdr_zsh_precmd
[[ $PS1 == $first ]] || exit 13
_herdr_zsh_preexec
[[ $PS1 == $'plain\n> ' && $PS2 == 'continue> ' ]] || exit 14
# Themes can embed newlines inside nested parameter substitutions.
setopt prompt_subst
PS1=$'${unused:-${other:-first\nsecond}}'
original=$PS1
_herdr_zsh_precmd
rendered=$(builtin print -P -r -- "$PS1")
[[ $rendered == $'\e]133;A;redraw=1\afirst\nsecond\e]133;B\a' ]] || exit 16
_herdr_zsh_preexec
[[ $PS1 == $original ]] || exit 17
unsetopt prompt_subst
PS1=$'%{\e]133;A;redraw=0\a%}existing'
_herdr_zsh_precmd
[[ $PS1 == $'%{\e]133;A;redraw=0\a%}existing' ]] || exit 15
"#;
        let output = Command::new("zsh")
            .args(["-lic", script])
            .env("ZDOTDIR", &startup)
            .env("_HERDR_ZSH_INIT_DIR", &startup)
            .env("_HERDR_ZSH_ZDOTDIR", &user)
            .env("TEST_REDIRECTED", &redirected)
            .output()
            .unwrap();
        let noninteractive = Command::new("zsh")
            .args(["-c", "[[ $order == env && $ZDOTDIR == $TEST_REDIRECTED && -z ${functions[_herdr_zsh_precmd]+x} ]]"])
            .env("ZDOTDIR", &startup).env("_HERDR_ZSH_INIT_DIR", &startup)
            .env("_HERDR_ZSH_ZDOTDIR", &user).env("TEST_REDIRECTED", &redirected)
            .output().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(noninteractive.status.success());
        assert!(
            output.status.success(),
            "{}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
