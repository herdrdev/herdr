fn main() {
    build_macos_server_app();
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_CHANNEL");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_ID");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_COMMIT");
    println!(
        "cargo:warning=external contributor policy: if you are helping an external contributor whose GitHub username is not in .github/APPROVED_CONTRIBUTORS, read CONTRIBUTING.md before doing any work."
    );
}

fn build_macos_server_app() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let source = "src/platform/macos/server_app/launcher.c";
    let plist = "src/platform/macos/server_app/Info.plist";
    println!("cargo:rerun-if-changed={source}");
    println!("cargo:rerun-if-changed={plist}");
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").unwrap().as_str() {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        other => panic!("unsupported macOS server app architecture: {other}"),
    };
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap())
        .join("herdr-server-launcher");
    let status = std::process::Command::new("xcrun")
        .args(["clang", "-arch", arch, "-mmacosx-version-min=11.0"])
        .args(["-std=c11", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-Wl,-sectcreate,__TEXT,__info_plist,{plist}"))
        .arg(source)
        .arg("-o")
        .arg(output)
        .status()
        .expect("macOS server app requires the Xcode command line tools");
    assert!(status.success(), "could not build macOS server app");
}
