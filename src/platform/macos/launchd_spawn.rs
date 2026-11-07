//! Starts the server daemon outside the launching terminal app.
//!
//! A process started from a terminal stays in that terminal app's launchd job
//! and resource coalition, even after `setsid`. When the app quits, macOS
//! removes its job and kills what is left inside it, so a server started from
//! Ghostty died with every pane on Cmd-Q (herdr#4920).
//!
//! The daemon is therefore submitted as a one-shot job in the user's GUI
//! launchd domain, which gives it a coalition of its own. The job immediately
//! forks the real server and exits, so the server is not a running GUI job that
//! logout would stop; it keeps the lifetime a directly spawned server had.

use std::ffi::{CString, OsStr};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LAUNCHCTL: &str = "/bin/launchctl";
const LABEL_PREFIX: &str = "dev.herdr.server.";
/// Tells the job process to fork the server and exit; never inherited by panes.
const DETACH_ENV: &str = "HERDR_LAUNCHD_DETACH";
/// Finished jobs stay loaded until logout; older ones are removed on the next launch.
const STALE_JOB_AGE: Duration = Duration::from_secs(60);

/// Submits `command` (program, arguments, environment changes, working
/// directory) as a run-once job in the caller's GUI launchd domain. Fails, so
/// the caller can spawn directly, when there is no GUI domain or the command
/// cannot be represented exactly in a launchd job. Returns the job label.
pub(crate) fn spawn_launchd_job(command: &Command) -> io::Result<String> {
    if std::env::var_os("SSH_CONNECTION").is_some() {
        // SSH logins already run outside any terminal app; keep them out of
        // the desktop session too.
        return Err(io::Error::other("started over SSH"));
    }
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let domain = format!("gui/{uid}");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?;
    let label = format!("{LABEL_PREFIX}{}.{}", std::process::id(), now.as_nanos());
    let working_dir = match command.get_current_dir() {
        Some(dir) => Some(dir.to_path_buf()),
        None => std::env::current_dir().ok(),
    };
    let plist = job_plist(&label, command, working_dir.as_deref(), current_umask())?;
    remove_stale_jobs(&domain, now);

    let dir = PrivateDir::create()?;
    let plist_path = dir.path().join("job.plist");
    write_private_file(&plist_path, plist.as_bytes())?;
    launchctl(&["bootstrap", &domain, &plist_path.to_string_lossy()])?;
    Ok(label)
}

/// Called first thing in the server process. Inside the launchd job, forks
/// the real server into its own session and lets the job process exit.
/// Returns a warning to log once logging is up if the server stays in the job.
pub(crate) fn leave_launchd_job() -> Option<String> {
    std::env::var_os(DETACH_ENV)?;
    std::env::remove_var(DETACH_ENV);
    // launchd names the job here; panes should see what a terminal shell sees.
    std::env::set_var("XPC_SERVICE_NAME", "0");
    // SAFETY: runs before the server creates threads; the child continues as
    // the server and the parent exits without running destructors.
    match unsafe { libc::fork() } {
        // SAFETY: plain syscall in the new child.
        0 if unsafe { libc::setsid() } < 0 => Some(format!(
            "server could not start its own session after leaving launchd: {}",
            io::Error::last_os_error()
        )),
        0 => None,
        // Keep serving from the job process; logout will stop this server.
        -1 => Some(format!(
            "server could not leave its launchd job: {}",
            io::Error::last_os_error()
        )),
        _ => unsafe { libc::_exit(0) },
    }
}

/// A fresh directory only this user can enter, removed with its contents.
struct PrivateDir(PathBuf);

impl PrivateDir {
    fn create() -> io::Result<Self> {
        let template = std::env::temp_dir().join("herdr-launchd.XXXXXXXX");
        let mut bytes = CString::new(template.as_os_str().as_bytes())
            .map_err(|_| io::Error::other("temporary path contains a NUL byte"))?
            .into_bytes_with_nul();
        // SAFETY: mkdtemp edits the NUL-terminated template in place and
        // creates the directory with mode 0700, failing if it exists.
        let created = unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) };
        if created.is_null() {
            return Err(io::Error::last_os_error());
        }
        bytes.pop();
        Ok(Self(PathBuf::from(OsStr::from_bytes(&bytes))))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_private_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents)
}

fn launchctl(args: &[&str]) -> io::Result<String> {
    let output = Command::new(LAUNCHCTL)
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(io::Error::other(format!(
            "launchctl {} failed ({}): {}",
            args.first().copied().unwrap_or_default(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn job_is_running(target: &str) -> bool {
    launchctl(&["print", target]).is_ok_and(|info| {
        info.lines()
            .any(|line| line.trim_start().starts_with("pid = "))
    })
}

/// Removes finished herdr server jobs from earlier launches. Running jobs and
/// recent ones (which may still be starting) are left alone.
fn remove_stale_jobs(domain: &str, now: Duration) {
    let Ok(list) = launchctl(&["list"]) else {
        return;
    };
    for label in stale_job_labels(&list, now) {
        let target = format!("{domain}/{label}");
        if job_is_running(&target) {
            continue;
        }
        if let Err(err) = launchctl(&["bootout", &target]) {
            tracing::debug!(%err, label, "could not remove finished server job");
        }
    }
}

fn stale_job_labels(list_output: &str, now: Duration) -> Vec<&str> {
    list_output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let pid = fields.next()?.trim();
            let label = fields.nth(1)?.trim();
            let started = label
                .strip_prefix(LABEL_PREFIX)?
                .rsplit_once('.')?
                .1
                .parse::<u128>()
                .ok()?;
            let stale =
                pid == "-" && now.as_nanos().saturating_sub(started) >= STALE_JOB_AGE.as_nanos();
            stale.then_some(label)
        })
        .collect()
}

/// Panes inherit the server's umask; launchd would otherwise apply its own.
/// Reading it briefly sets the process mask to 077.
fn current_umask() -> libc::mode_t {
    // SAFETY: umask only swaps the process mask; it is restored immediately.
    unsafe {
        let mask = libc::umask(0o077);
        libc::umask(mask);
        mask
    }
}

fn job_plist(
    label: &str,
    command: &Command,
    working_dir: Option<&Path>,
    umask: libc::mode_t,
) -> io::Result<String> {
    let mut plist = String::from(concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
        "<plist version=\"1.0\">\n<dict>\n",
    ));
    push_key_string(&mut plist, "Label", label)?;
    plist.push_str("<key>ProgramArguments</key>\n<array>\n");
    push_string(&mut plist, command.get_program())?;
    for arg in command.get_args() {
        push_string(&mut plist, arg)?;
    }
    plist.push_str("</array>\n<key>EnvironmentVariables</key>\n<dict>\n");
    for (key, value) in resolved_environment(command) {
        push_key(&mut plist, &key)?;
        push_string(&mut plist, &value)?;
    }
    push_key_string(&mut plist, DETACH_ENV, "1")?;
    plist.push_str("</dict>\n");
    if let Some(dir) = working_dir {
        push_key(&mut plist, OsStr::new("WorkingDirectory"))?;
        push_string(&mut plist, dir.as_os_str())?;
    }
    for key in ["StandardInPath", "StandardOutPath", "StandardErrorPath"] {
        push_key_string(&mut plist, key, "/dev/null")?;
    }
    // Run once and never restart. The forked server must survive the job
    // process exiting, and keeps normal scheduling priority.
    plist.push_str(concat!(
        "<key>RunAtLoad</key>\n<true/>\n",
        "<key>KeepAlive</key>\n<false/>\n",
        "<key>AbandonProcessGroup</key>\n<true/>\n",
    ));
    push_key_string(&mut plist, "ProcessType", "Interactive")?;
    plist.push_str(&format!("<key>Umask</key>\n<integer>{umask}</integer>\n"));
    plist.push_str("</dict>\n</plist>\n");
    Ok(plist)
}

fn resolved_environment(command: &Command) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut env: Vec<_> = std::env::vars_os().collect();
    for (key, value) in command.get_envs() {
        env.retain(|(existing, _)| existing != key);
        if let Some(value) = value {
            env.push((key.to_os_string(), value.to_os_string()));
        }
    }
    env
}

fn push_key_string(plist: &mut String, key: &str, value: &str) -> io::Result<()> {
    push_key(plist, OsStr::new(key))?;
    push_string(plist, OsStr::new(value))
}

fn push_key(plist: &mut String, key: &OsStr) -> io::Result<()> {
    plist.push_str("<key>");
    push_escaped(plist, key)?;
    plist.push_str("</key>\n");
    Ok(())
}

fn push_string(plist: &mut String, value: &OsStr) -> io::Result<()> {
    plist.push_str("<string>");
    push_escaped(plist, value)?;
    plist.push_str("</string>\n");
    Ok(())
}

/// Escapes for XML text. Rejects what a plist cannot carry byte-for-byte
/// (invalid UTF-8, control characters other than tab and newline, CR) so the
/// caller falls back to a direct spawn instead of silently altering a value.
fn push_escaped(plist: &mut String, value: &OsStr) -> io::Result<()> {
    let text = value
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "value is not UTF-8"))?;
    for ch in text.chars() {
        match ch {
            '&' => plist.push_str("&amp;"),
            '<' => plist.push_str("&lt;"),
            '>' => plist.push_str("&gt;"),
            '"' => plist.push_str("&quot;"),
            '\t' | '\n' => plist.push(ch),
            c if (c as u32) < 0x20 || c == '\u{7f}' || c == '\u{fffe}' || c == '\u{ffff}' => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "value contains a control character",
                ));
            }
            c => plist.push(c),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn only_old_finished_herdr_jobs_are_stale() {
        let now = Duration::from_secs(1_000);
        let old = Duration::from_secs(900).as_nanos();
        let recent = Duration::from_secs(990).as_nanos();
        let list = format!(
            "PID\tStatus\tLabel\n\
             -\t0\tdev.herdr.server.12.{old}\n\
             42\t0\tdev.herdr.server.13.{old}\n\
             -\t0\tdev.herdr.server.14.{recent}\n\
             -\t0\tcom.example.other.{old}\n"
        );

        assert_eq!(
            stale_job_labels(&list, now),
            vec![format!("dev.herdr.server.12.{old}").as_str()]
        );
    }

    #[test]
    fn plist_escapes_values_and_never_restarts() {
        let mut command = Command::new("/opt/her<dr>");
        command
            .arg("a&b")
            .env("HERDR_TEST_VALUE", "\"q\"\tx")
            .env_remove("HOME");
        let plist = job_plist(
            "dev.herdr.server.1.2",
            &command,
            Some(Path::new("/tmp")),
            0o077,
        )
        .expect("plist");

        assert!(plist.contains("<string>/opt/her&lt;dr&gt;</string>"));
        assert!(plist.contains("<string>a&amp;b</string>"));
        assert!(plist.contains("<key>HERDR_TEST_VALUE</key>\n<string>&quot;q&quot;\tx</string>"));
        assert!(!plist.contains("<key>HOME</key>"));
        assert!(plist.contains(&format!("<key>{DETACH_ENV}</key>")));
        assert!(plist.contains("<key>KeepAlive</key>\n<false/>"));
        assert!(plist.contains("<key>WorkingDirectory</key>\n<string>/tmp</string>"));
        assert!(plist.contains("<key>Umask</key>\n<integer>63</integer>"));
    }

    #[test]
    fn values_a_plist_would_alter_are_rejected() {
        for value in [
            std::ffi::OsString::from("prompt \u{1b}[0m"),
            std::ffi::OsString::from("line\rbreak"),
            std::ffi::OsString::from_vec(vec![0x66, 0xff]),
        ] {
            let mut command = Command::new("/bin/true");
            command.env("HERDR_TEST_VALUE", &value);
            assert!(
                job_plist("dev.herdr.server.1.2", &command, None, 0o022).is_err(),
                "{value:?}"
            );
        }
    }

    #[test]
    fn private_dir_is_owner_only_and_removed() {
        use std::os::unix::fs::PermissionsExt;

        let path = {
            let dir = PrivateDir::create().expect("dir");
            let mode = std::fs::metadata(dir.path())
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
            let file = dir.path().join("job.plist");
            write_private_file(&file, b"x").expect("write");
            let mode = std::fs::metadata(&file).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            assert!(
                write_private_file(&file, b"y").is_err(),
                "never reuses a file"
            );
            dir.path().to_path_buf()
        };
        assert!(!path.exists());
    }

    fn resource_coalition(pid: u32) -> u64 {
        const PROC_PIDCOALITIONINFO: libc::c_int = 20;
        let mut info = [0u64; 5];
        let size = std::mem::size_of_val(&info) as libc::c_int;
        // SAFETY: the buffer is large enough for proc_pidcoalitioninfo.
        let read = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                PROC_PIDCOALITIONINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        assert!(read > 0, "coalition lookup failed for {pid}");
        info[0]
    }

    #[test]
    fn a_detached_child_stays_in_the_callers_coalition() {
        // The failure this module avoids: setsid does not leave the coalition.
        use std::os::unix::process::CommandExt;
        let mut command = Command::new("sleep");
        command.arg("5");
        // SAFETY: setsid is async-signal-safe and touches only the child.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = command.spawn().expect("spawn");
        let coalition = resource_coalition(child.id());
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(coalition, resource_coalition(std::process::id()));
    }

    const DETACH_HELPER_OUT: &str = "HERDR_LAUNCHD_DETACH_HELPER_OUT";

    /// Body of the launchd job in `detached_server_outlives_its_launcher_job`.
    #[test]
    #[ignore = "runs only as the launchd job of another test"]
    fn detach_helper() {
        let Some(out) = std::env::var_os(DETACH_HELPER_OUT) else {
            return;
        };
        let warning = leave_launchd_job();
        // The launcher exits right after forking; wait until that is visible.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        // SAFETY: plain process query.
        while unsafe { libc::getppid() } != 1 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        // SAFETY: plain process queries.
        let (pid, ppid, sid) = unsafe { (libc::getpid(), libc::getppid(), libc::getsid(0)) };
        let line = format!(
            "{pid} {ppid} {sid} {} {} {}",
            std::env::var("XPC_SERVICE_NAME").unwrap_or_default(),
            std::env::var_os(DETACH_ENV).is_some(),
            warning.is_some(),
        );
        let _ = std::fs::write(out, line);
        std::thread::sleep(Duration::from_secs(30));
    }

    fn gui_domain_available() -> Option<libc::uid_t> {
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        (launchctl(&["print", &format!("gui/{uid}")]).is_ok()
            && std::env::var_os("SSH_CONNECTION").is_none())
        .then_some(uid)
    }

    fn wait_for_output(out: &Path) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::fs::metadata(out).map_or(true, |meta| meta.len() == 0)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::read_to_string(out).expect("job output")
    }

    /// Needs a GUI login session; skipped only where the gui domain is absent.
    #[test]
    fn detached_server_outlives_its_launcher_job() {
        let Some(uid) = gui_domain_available() else {
            eprintln!("skipping: no gui launchd domain for this test process");
            return;
        };
        let dir = PrivateDir::create().expect("dir");
        let out = dir.path().join("out");
        let mut command = Command::new(std::env::current_exe().expect("test binary"));
        command
            .args([
                "--exact",
                "platform::macos::launchd_spawn::tests::detach_helper",
            ])
            .args(["--ignored", "--test-threads=1"])
            .env(DETACH_HELPER_OUT, &out);

        let label = spawn_launchd_job(&command).expect("launchd job");
        let written = wait_for_output(&out);
        let target = format!("gui/{uid}/{label}");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while job_is_running(&target) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let launcher_running = job_is_running(&target);
        // Removing the finished job must not take the detached server with it.
        let _ = launchctl(&["bootout", &target]);
        std::thread::sleep(Duration::from_millis(300));

        let fields: Vec<&str> = written.split(' ').collect();
        let pid: libc::pid_t = fields[0].parse().expect("pid");
        // SAFETY: probes and then stops the helper process started by this test.
        let alive_after_bootout = unsafe { libc::kill(pid, 0) } == 0;
        let detached_coalition = resource_coalition(pid as u32);
        unsafe { libc::kill(pid, libc::SIGKILL) };

        assert!(!launcher_running, "the launcher job finished: {written}");
        assert!(
            alive_after_bootout,
            "the detached server survives: {written}"
        );
        assert_eq!(fields[1], "1", "reparented to launchd: {written}");
        assert_eq!(fields[2], fields[0], "leads its own session: {written}");
        assert_eq!(fields[3], "0", "job name hidden from panes: {written}");
        assert_eq!(fields[4], "false", "detach marker consumed: {written}");
        assert_eq!(fields[5], "false", "no detach warning: {written}");
        assert_ne!(
            detached_coalition,
            resource_coalition(std::process::id()),
            "outside the caller's coalition"
        );
    }

    /// Needs a GUI login session; skipped only where the gui domain is absent.
    #[test]
    fn job_runs_outside_the_callers_coalition_and_is_not_restarted() {
        let Some(uid) = gui_domain_available() else {
            eprintln!("skipping: no gui launchd domain for this test process");
            return;
        };
        let dir = PrivateDir::create().expect("dir");
        let out = dir.path().join("out");
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("printf '%s %s %s' $$ \"$HERDR_LAUNCHD_TEST\" \"${HERDR_LAUNCHD_DETACH-unset}\" > out; exec sleep 30")
            .env("HERDR_LAUNCHD_TEST", "yes")
            .current_dir(dir.path());

        let label = spawn_launchd_job(&command).expect("launchd job");
        let written = wait_for_output(&out);
        let mut fields = written.split(' ');
        let pid: u32 = fields.next().and_then(|pid| pid.parse().ok()).expect("pid");
        let job_coalition = resource_coalition(pid);
        let own_coalition = resource_coalition(std::process::id());

        // SAFETY: signals the job process started by this test.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        std::thread::sleep(Duration::from_millis(500));
        let target = format!("gui/{uid}/{label}");
        let restarted = job_is_running(&target);
        let _ = launchctl(&["bootout", &target]);

        assert_eq!(fields.next(), Some("yes"), "environment reached the job");
        assert_eq!(fields.next(), Some("1"), "the job is told to detach");
        assert_ne!(job_coalition, own_coalition, "job has its own coalition");
        assert!(!restarted, "launchd does not restart the server");
    }
}
