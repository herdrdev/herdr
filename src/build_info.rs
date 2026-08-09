//! Build identity helpers.

use std::sync::OnceLock;

static EXECUTABLE_SHA256: OnceLock<Option<String>> = OnceLock::new();
static RELEASE_MANIFEST_DIGEST: OnceLock<Option<String>> = OnceLock::new();

pub fn initialize() {
    let _ = EXECUTABLE_SHA256.get_or_init(compute_executable_sha256);
    let _ = RELEASE_MANIFEST_DIGEST.get_or_init(read_release_manifest_digest);
}

pub const BASE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn channel() -> &'static str {
    non_empty(option_env!("HERDR_BUILD_CHANNEL")).unwrap_or("stable")
}

pub fn build_id() -> Option<&'static str> {
    non_empty(option_env!("HERDR_BUILD_ID"))
}

pub fn version() -> String {
    match channel() {
        "stable" => BASE_VERSION.to_string(),
        channel => match build_id() {
            Some(build_id) => format!("{BASE_VERSION}-{channel}.{build_id}"),
            None => format!("{BASE_VERSION}-{channel}"),
        },
    }
}

pub fn source_commit() -> Option<&'static str> {
    non_empty(option_env!("HERDR_BUILD_COMMIT"))
}

pub fn release_manifest_digest() -> Option<String> {
    RELEASE_MANIFEST_DIGEST
        .get_or_init(read_release_manifest_digest)
        .clone()
}

pub fn executable_sha256() -> Option<String> {
    EXECUTABLE_SHA256
        .get_or_init(compute_executable_sha256)
        .clone()
}

fn read_release_manifest_digest() -> Option<String> {
    validated_sha256(std::env::var("HERDR_RELEASE_MANIFEST_DIGEST").ok())
}

fn compute_executable_sha256() -> Option<String> {
    let path = std::env::current_exe().ok()?;
    let bytes = std::fs::read(path).ok()?;
    Some(hex_digest(&bytes))
}

fn validated_sha256(value: Option<String>) -> Option<String> {
    let value = value?;
    (value.len() == 64
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        && value.bytes().all(|byte| !byte.is_ascii_uppercase()))
    .then_some(value)
}

fn hex_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

pub fn is_preview() -> bool {
    channel() == "preview"
}

fn non_empty(value: Option<&'static str>) -> Option<&'static str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn stable_version_defaults_to_cargo_version() {
        assert!(!super::version().is_empty());
    }
}
