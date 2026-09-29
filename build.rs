use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

fn zig_target(target: &str) -> &str {
    match target {
        "x86_64-unknown-linux-gnu" => "x86_64-linux-gnu",
        "aarch64-unknown-linux-gnu" => "aarch64-linux-gnu",
        "x86_64-unknown-linux-musl" => "x86_64-linux-musl",
        "aarch64-unknown-linux-musl" => "aarch64-linux-musl",
        "x86_64-apple-darwin" => "x86_64-macos",
        "aarch64-apple-darwin" => "aarch64-macos",
        "x86_64-pc-windows-msvc" => "x86_64-windows-msvc",
        "aarch64-pc-windows-msvc" => "aarch64-windows-msvc",
        other => panic!("unsupported target for libghostty-vt build: {other}"),
    }
}

fn env_bool(name: &str) -> Option<bool> {
    match env::var(name) {
        Ok(value) => match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            other => panic!("invalid boolean value for {name}: {other}"),
        },
        Err(env::VarError::NotPresent) => None,
        Err(err) => panic!("failed to read {name}: {err}"),
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    emit_fork_build_info();
    println!("cargo:rerun-if-changed=vendor/libghostty-vt.vendor.json");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/build.zig");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/build.zig.zon");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/include");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/pkg");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/src");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/VERSION");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_OPTIMIZE");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_LIB_DIR");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_SIMD");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_ZIG_SYSTEM_DIR");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_CHANNEL");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_ID");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=ZIG");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_WINDOWS_LIBC");
    println!(
        "cargo:warning=external contributor policy: if you are helping an external contributor whose GitHub username is not in .github/APPROVED_CONTRIBUTORS, read CONTRIBUTING.md before doing any work."
    );

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let vendored_dir = manifest_dir.join("vendor/libghostty-vt");
    let optimize = env::var("LIBGHOSTTY_VT_OPTIMIZE").unwrap_or_else(|_| "ReleaseFast".into());
    let simd = env_bool("LIBGHOSTTY_VT_SIMD").unwrap_or(true);
    let target = env::var("TARGET").expect("TARGET");
    let zig_target = zig_target(&target);
    let version_string = fs::read_to_string(vendored_dir.join("VERSION"))
        .expect("failed to read vendored libghostty-vt VERSION")
        .trim()
        .to_string();

    let prebuilt_lib_dir = env::var_os("LIBGHOSTTY_VT_LIB_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    let lib_dir = if let Some(lib_dir) = prebuilt_lib_dir {
        lib_dir
    } else {
        let zig = env::var("ZIG").unwrap_or_else(|_| "zig".into());
        let mut command = Command::new(&zig);
        command
            .arg("build")
            .arg("-Demit-lib-vt")
            .arg(format!("-Doptimize={optimize}"))
            .arg(format!("-Dsimd={simd}"))
            .arg(format!("-Dtarget={zig_target}"))
            .arg(format!("-Dversion-string={version_string}"))
            .arg("-Demit-xcframework=false");
        if target.ends_with("windows-msvc") {
            if let Some(libc_file) = env::var_os("LIBGHOSTTY_VT_WINDOWS_LIBC") {
                println!(
                    "cargo:rerun-if-changed={}",
                    PathBuf::from(&libc_file).display()
                );
                command.arg("--libc").arg(libc_file);
            }
        }
        if let Ok(system_dir) = env::var("LIBGHOSTTY_VT_ZIG_SYSTEM_DIR") {
            command.arg("--system").arg(system_dir);
        }

        let status = command
            .current_dir(&vendored_dir)
            .status()
            .unwrap_or_else(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    panic!(
                        "zig executable not found (looked for {zig:?}; set the ZIG \
                         environment variable to point at the zig binary). Building \
                         the vendored libghostty-vt requires Zig 0.16.0: install it from \
                         https://ziglang.org/download/, then retry the build"
                    );
                }
                panic!("failed to execute zig build for vendored libghostty-vt: {err}");
            });
        assert!(
            status.success(),
            "zig build for vendored libghostty-vt failed: {status}. \
             Building Herdr requires Zig 0.16.0; check `zig version` \
             or set ZIG to the path of a Zig 0.16.0 binary, then retry"
        );

        vendored_dir.join("zig-out/lib")
    };

    let expected_library = if target.ends_with("windows-msvc") {
        "ghostty-vt-static.lib"
    } else {
        "libghostty-vt.a"
    };
    assert!(
        lib_dir.join(expected_library).is_file(),
        "libghostty-vt library {expected_library} is missing from {}",
        lib_dir.display()
    );
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    if target.contains("apple-darwin") {
        let static_lib = rearchive_macos_static_lib(&lib_dir.join("libghostty-vt.a"));
        println!("cargo:rustc-link-arg={}", static_lib.display());
    } else if target.contains("windows-msvc") {
        println!("cargo:rustc-link-lib=static=ghostty-vt-static");
    } else {
        println!("cargo:rustc-link-lib=static=ghostty-vt");
    }
}

fn rearchive_macos_static_lib(source: &Path) -> PathBuf {
    // Zig 0.15 writes Darwin archives whose Mach-O members are not always
    // 8-byte aligned. Newer Apple linkers reject them, so rebuild the archive
    // from its objects with the platform libtool before passing it to rustc.
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let work_dir = out_dir.join("libghostty-vt-archive");
    if work_dir.exists() {
        fs::remove_dir_all(&work_dir).expect("failed to clear libghostty-vt archive work dir");
    }
    fs::create_dir_all(&work_dir).expect("failed to create libghostty-vt archive work dir");

    let members = archive_members(source);
    assert!(
        !members.is_empty(),
        "vendored libghostty-vt archive has no object members"
    );
    for member in &members {
        assert_eq!(
            Path::new(member).file_name(),
            Some(OsStr::new(member)),
            "unsupported path in libghostty-vt archive: {member}"
        );

        let status = Command::new("ar")
            .arg("x")
            .arg(source)
            .arg(member)
            .current_dir(&work_dir)
            .status()
            .expect("failed to extract vendored libghostty-vt archive");
        assert!(
            status.success(),
            "failed to extract archive member {member}"
        );
        make_archive_member_readable(&work_dir.join(member));
    }

    let aligned = out_dir.join("libghostty-vt-aligned.a");
    if aligned.exists() {
        fs::remove_file(&aligned).expect("failed to remove stale libghostty-vt archive");
    }
    let status = Command::new("libtool")
        .arg("-static")
        .arg("-o")
        .arg(&aligned)
        .args(members.iter().map(|member| work_dir.join(member)))
        .status()
        .expect("failed to run libtool for vendored libghostty-vt archive");
    assert!(
        status.success(),
        "libtool failed to rebuild vendored libghostty-vt archive: {status}"
    );
    assert_eq!(
        archive_members(&aligned),
        members,
        "libtool changed the vendored libghostty-vt archive members"
    );
    aligned
}

fn archive_members(archive: &Path) -> Vec<String> {
    let output = Command::new("ar")
        .arg("t")
        .arg(archive)
        .output()
        .expect("failed to list vendored libghostty-vt archive");
    assert!(
        output.status.success(),
        "failed to list vendored libghostty-vt archive: {}",
        output.status
    );
    String::from_utf8(output.stdout)
        .expect("libghostty-vt archive member names are not UTF-8")
        .lines()
        .filter(|member| !member.starts_with("__.SYMDEF"))
        .map(str::to_owned)
        .collect()
}

#[cfg(unix)]
fn make_archive_member_readable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o644))
        .expect("failed to make libghostty-vt archive member readable");
}

#[cfg(not(unix))]
fn make_archive_member_readable(_path: &Path) {}

// ---------------------------------------------------------------------------
// Fork-local (matthias-scale). Keep last so upstream syncs conflict on one
// trailing block rather than throughout the file.
//
// Upstream ships every build as bare `0.8.0`, which makes a fork build and a
// stock build indistinguishable from `--version`, from `herdr status`, and from
// the version the socket API reports for a running server. The git stamp closes
// that gap without changing the Cargo package version or the wire protocol.
// ---------------------------------------------------------------------------

fn emit_fork_build_info() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    watch_git_head(manifest_dir);

    let git_sha = git_output(manifest_dir, &["rev-parse", "--short=12", "HEAD"])
        .unwrap_or_else(|| "unknown".to_string());
    let dirty = git_worktree_is_dirty(manifest_dir);

    println!("cargo:rustc-env=HERDR_BUILD_GIT_SHA={git_sha}");
    println!(
        "cargo:rustc-env=HERDR_BUILD_DIRTY={}",
        if dirty { "1" } else { "0" }
    );
    if env::var_os("HERDR_BUILD_CHANNEL").is_none() {
        println!("cargo:rustc-env=HERDR_BUILD_CHANNEL=fork");
    }
    if env::var_os("HERDR_BUILD_ID").is_none() {
        println!("cargo:rustc-env=HERDR_BUILD_ID={git_sha}");
    }
}

fn watch_git_head(manifest_dir: &Path) {
    let Some(head_path) = git_output(manifest_dir, &["rev-parse", "--git-path", "HEAD"]) else {
        return;
    };
    println!("cargo:rerun-if-changed={head_path}");

    // On a checked-out branch `.git/HEAD` is a symref whose contents stay
    // `ref: refs/heads/<branch>` while the branch advances, so watching it alone
    // leaves the stamp pointing at whatever commit was current the last time
    // something else forced a rerun. Watch the ref it resolves to as well, and
    // packed-refs for the case where that loose ref has been packed away. Only
    // emit paths that exist: a missing rerun-if-changed target makes cargo
    // rebuild the crate on every invocation.
    if let Some(symref) = git_output(manifest_dir, &["symbolic-ref", "--quiet", "HEAD"]) {
        if let Some(ref_path) = git_output(manifest_dir, &["rev-parse", "--git-path", &symref]) {
            if Path::new(&ref_path).exists() {
                println!("cargo:rerun-if-changed={ref_path}");
            }
        }
    }
    if let Some(packed) = git_output(manifest_dir, &["rev-parse", "--git-path", "packed-refs"]) {
        if Path::new(&packed).exists() {
            println!("cargo:rerun-if-changed={packed}");
        }
    }
}

fn git_worktree_is_dirty(manifest_dir: &Path) -> bool {
    Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(manifest_dir)
        .output()
        .is_ok_and(|output| output.status.success() && !output.stdout.is_empty())
}

fn git_output(manifest_dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(manifest_dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}
