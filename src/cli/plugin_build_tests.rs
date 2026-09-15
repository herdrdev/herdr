use super::*;

fn build_with_detached_child(exit_code: i32) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("herdr-build-{}-{nonce}", std::process::id()));
    let checkout = root.join("checkout");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(
        checkout.join("child.ps1"),
        r#"
[IO.File]::WriteAllText('child-ready', [Environment]::CurrentDirectory)
$deadline = [DateTime]::UtcNow.AddSeconds(60)
while (-not [IO.File]::Exists('..\stop') -and [DateTime]::UtcNow -lt $deadline) {
    Start-Sleep -Milliseconds 10
}
"#,
    )
    .unwrap();
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
$info = New-Object System.Diagnostics.ProcessStartInfo
$info.FileName = (Get-Process -Id $PID).Path
$info.Arguments = '-NoProfile -NonInteractive -File child.ps1'
$info.WorkingDirectory = [Environment]::CurrentDirectory
$info.UseShellExecute = $false
$info.CreateNoWindow = $true
$info.RedirectStandardOutput = $true
$info.RedirectStandardError = $true
$child = [Diagnostics.Process]::Start($info)
$deadline = [DateTime]::UtcNow.AddSeconds(10)
while (-not [IO.File]::Exists('child-ready')) {{
    if ($child.HasExited -or [DateTime]::UtcNow -gt $deadline) {{
        throw 'child did not acquire its working directory'
    }}
    Start-Sleep -Milliseconds 10
}}
[IO.File]::WriteAllText('built.txt', 'built')
[Console]::Out.WriteLine('build output')
[Console]::Error.WriteLine('build diagnostic')
exit {exit_code}
"#
    );
    let command = vec![
        "powershell.exe".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-Command".into(),
        script,
    ];
    let result = run_plugin_build_command("example.detached", 1, 1, &checkout, &command);
    let child_cwd = std::fs::read_to_string(checkout.join("child-ready"))
        .and_then(|path| Path::new(&path).canonicalize());
    let expected_cwd = checkout.canonicalize();
    let destination = root.join("installed");
    let rename = std::fs::rename(&checkout, &destination);

    // Also clean up when testing the unfixed runner: never leave the fixture's
    // background process alive after an assertion failure.
    std::fs::write(root.join("stop"), "stop").unwrap();
    let artifact = std::fs::read_to_string(destination.join("built.txt"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while checkout.exists() && std::time::Instant::now() < deadline {
        if std::fs::rename(&checkout, &destination).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let cleanup = std::fs::remove_dir_all(&root);

    assert_eq!(child_cwd.unwrap(), expected_cwd.unwrap());
    rename.expect("build descendants must release the checkout before installation");
    cleanup.expect("build descendants must not leave an orphaned checkout");
    assert_eq!(artifact.unwrap(), "built");
    if exit_code == 0 {
        result.unwrap();
    } else {
        let failure = result.unwrap_err();
        match failure.kind {
            PluginBuildFailureKind::Exit {
                status,
                stdout,
                stderr,
            } => {
                assert_eq!(status.code(), Some(exit_code));
                assert!(stdout.text.contains("build output"));
                assert!(stderr.text.contains("build diagnostic"));
            }
            other => panic!("expected the original build failure, got {other:?}"),
        }
    }
}

#[test]
fn successful_plugin_build_releases_detached_child_checkout() {
    build_with_detached_child(0);
}

#[test]
fn failed_plugin_build_releases_detached_child_checkout_and_preserves_output() {
    build_with_detached_child(7);
}

#[test]
fn missing_plugin_build_program_reports_start_failure() {
    let error = run_plugin_build_command(
        "example.missing",
        1,
        1,
        &std::env::temp_dir(),
        &["definitely-missing-herdr-build-tool-xyz".into()],
    )
    .unwrap_err();
    assert!(matches!(error.kind, PluginBuildFailureKind::Start { .. }));
}
