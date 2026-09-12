//! Explicit full-registry delivery. HTTPS authenticates the configured origin;
//! snapshot hashes detect corruption, not compromise of that publishing origin.

use std::io::Read;
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::files;

pub(crate) const DEFAULT_ORIGIN: &str = "https://registry.herdr.dev";
pub(crate) const MAX_POINTER_BYTES: usize = 4096;
pub(crate) const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Channel {
    #[default]
    Stable,
    Preview,
    Staging,
}

impl Channel {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Preview => "preview",
            Self::Staging => "staging",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChannelPointer {
    pub(crate) schema: u32,
    pub(crate) channel: Channel,
    pub(crate) generation: u64,
    pub(crate) snapshot_sha256: String,
    pub(crate) snapshot_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteRevision {
    pub(crate) origin: String,
    pub(crate) pointer: ChannelPointer,
    pub(crate) commit: String,
}

impl RemoteRevision {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if normalize_origin(&self.origin)? != self.origin {
            return Err("registry provenance origin must be normalized".into());
        }
        self.pointer.validate(self.pointer.channel)?;
        if !git_id(&self.commit) {
            return Err("invalid registry source commit".into());
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Compatibility {
    registry_api: u32,
    min_detection_engine: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    repository: String,
    commit: String,
    agents_tree: String,
    dirty: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema: u32,
    compatibility: Compatibility,
    source: Source,
    content_sha256: String,
    #[serde(deserialize_with = "deserialize_files")]
    files: Vec<SnapshotFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotFile {
    path: String,
    bytes: u64,
    sha256: String,
    text: String,
}

#[derive(Debug)]
pub(crate) struct VerifiedSnapshot {
    pub(crate) files: Vec<(String, String)>,
    pub(crate) content_sha256: String,
    pub(crate) commit: String,
}

fn deserialize_files<'de, D>(deserializer: D) -> Result<Vec<SnapshotFile>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = Vec<SnapshotFile>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a bounded sorted registry inventory")
        }
        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut result: Vec<SnapshotFile> = Vec::new();
            let mut total = 0u64;
            while let Some(file) = sequence.next_element::<SnapshotFile>()? {
                total += file.text.len() as u64;
                if result.len() >= files::MAX_FILES
                    || file.path.len() > 240
                    || file.text.len() as u64 > files::MAX_FILE_BYTES
                    || total > files::MAX_TOTAL_BYTES
                    || result.last().is_some_and(|last| last.path >= file.path)
                {
                    return Err(serde::de::Error::custom(
                        "registry inventory exceeds bounds or is not strictly sorted",
                    ));
                }
                result.push(file);
            }
            Ok(result)
        }
    }
    deserializer.deserialize_seq(Visitor)
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn git_id(value: &str) -> bool {
    hex(value, 40) || hex(value, 64)
}

impl ChannelPointer {
    fn validate(&self, channel: Channel) -> Result<(), String> {
        if self.schema != 1
            || self.channel != channel
            || self.generation == 0
            || !hex(&self.snapshot_sha256, 64)
            || self.snapshot_bytes == 0
            || self.snapshot_bytes > MAX_SNAPSHOT_BYTES as u64
        {
            return Err("invalid or unsupported registry channel pointer".into());
        }
        Ok(())
    }
}

pub(crate) fn parse_pointer(bytes: &[u8], channel: Channel) -> Result<ChannelPointer, String> {
    if bytes.len() > MAX_POINTER_BYTES {
        return Err("registry channel pointer exceeds limit".into());
    }
    let pointer: ChannelPointer = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    pointer.validate(channel)?;
    Ok(pointer)
}

pub(crate) fn validate_snapshot(bytes: &[u8]) -> Result<VerifiedSnapshot, String> {
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err("registry snapshot exceeds limit".into());
    }
    let snapshot: Snapshot = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if snapshot.schema != 1
        || snapshot.compatibility.registry_api != 1
        || snapshot.compatibility.min_detection_engine == 0
        || snapshot.compatibility.min_detection_engine
            > crate::detect::manifest_version::MANIFEST_ENGINE_VERSION
    {
        return Err(
            "registry snapshot requires an unsupported registry API or detection engine".into(),
        );
    }
    if snapshot.source.repository != "https://github.com/herdrdev/agent-registry"
        || !git_id(&snapshot.source.commit)
        || !git_id(&snapshot.source.agents_tree)
        || snapshot.source.dirty
        || !hex(&snapshot.content_sha256, 64)
    {
        return Err("registry snapshot has invalid or uncommitted provenance".into());
    }
    let mut inventory = Sha256::new();
    let mut files = Vec::with_capacity(snapshot.files.len());
    for file in snapshot.files {
        if file
            .text
            .bytes()
            .any(|byte| (byte < 0x20 && !b"\t\n\r".contains(&byte)) || byte == 0x7f)
        {
            return Err(format!(
                "registry file contains control bytes: {}",
                file.path
            ));
        }
        if file.bytes != file.text.len() as u64
            || !hex(&file.sha256, 64)
            || sha256(file.text.as_bytes()) != file.sha256
        {
            return Err(format!("registry file size/hash mismatch: {}", file.path));
        }
        inventory.update(format!("{}  {}\n", file.sha256, file.path).as_bytes());
        files.push((file.path, file.text));
    }
    if format!("{:x}", inventory.finalize()) != snapshot.content_sha256 {
        return Err("registry content digest mismatch".into());
    }
    let borrowed: Vec<_> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    super::validate_packages(&borrowed)?;
    Ok(VerifiedSnapshot {
        files,
        content_sha256: snapshot.content_sha256,
        commit: snapshot.source.commit,
    })
}

pub(crate) fn origin() -> Result<String, String> {
    let origin =
        std::env::var("HERDR_AGENT_REGISTRY_ORIGIN").unwrap_or_else(|_| DEFAULT_ORIGIN.to_owned());
    normalize_origin(&origin)
}

fn normalize_origin(origin: &str) -> Result<String, String> {
    // Accept an HTTPS origin, not a URL template, path or credentials. Canonical
    // host/port spelling prevents channel high-water marks being bypassed by aliases.
    let invalid = || {
        "registry origin must be a bounded HTTPS hostname with optional port, without path or credentials".to_string()
    };
    let authority = origin
        .strip_prefix("https://")
        .ok_or_else(invalid)?
        .trim_end_matches('/');
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => {
            let port = port.parse::<u16>().map_err(|_| invalid())?;
            if port == 0 {
                return Err(invalid());
            }
            (host, Some(port))
        }
        None => (authority, None),
    };
    if host.is_empty()
        || host.len() > 253
        || host.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return Err(invalid());
    }
    let host = host.to_ascii_lowercase();
    Ok(match port {
        None | Some(443) => format!("https://{host}"),
        Some(port) => format!("https://{host}:{port}"),
    })
}

pub(crate) fn download(
    origin: &str,
    channel: Channel,
) -> Result<(RemoteRevision, VerifiedSnapshot), String> {
    download_with(origin, channel, fetch)
}

fn download_with(
    origin: &str,
    channel: Channel,
    mut fetch: impl FnMut(&str, usize, bool) -> Result<Vec<u8>, String>,
) -> Result<(RemoteRevision, VerifiedSnapshot), String> {
    let base = normalize_origin(origin)?;
    let pointer = parse_pointer(
        &fetch(
            &format!("{base}/v1/channels/{}.json", channel.as_str()),
            MAX_POINTER_BYTES,
            true,
        )?,
        channel,
    )?;
    let bytes = fetch(
        &format!("{base}/v1/snapshots/{}.json", pointer.snapshot_sha256),
        pointer.snapshot_bytes as usize,
        false,
    )?;
    if bytes.len() as u64 != pointer.snapshot_bytes || sha256(&bytes) != pointer.snapshot_sha256 {
        return Err("downloaded registry snapshot does not match the channel size/hash".into());
    }
    let snapshot = validate_snapshot(&bytes)?;
    let revision = RemoteRevision {
        origin: base,
        pointer,
        commit: snapshot.commit.clone(),
    };
    Ok((revision, snapshot))
}

fn fetch(url: &str, limit: usize, fresh: bool) -> Result<Vec<u8>, String> {
    let mut command = crate::noninteractive_process::curl_command();
    command.args([
        "--disable",
        "--fail",
        "--silent",
        "--show-error",
        "--proto",
        "=https",
        "--connect-timeout",
        "5",
        "--max-time",
        "30",
        "--max-filesize",
        &limit.to_string(),
        "--header",
        "Accept-Encoding: identity",
        "--url",
        url,
    ]);
    if fresh {
        command.args(["--header", "Cache-Control: no-cache"]);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("registry download could not start curl: {e}"))?;
    let result: Result<Vec<u8>, String> = (|| {
        let stdout = child
            .stdout
            .as_mut()
            .ok_or("registry download has no stdout")?;
        read_download(stdout, limit)
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child
        .wait()
        .map_err(|e| format!("registry download wait failed: {e}"))?;
    let bytes = result?;
    if !status.success() {
        return Err(format!(
            "registry download failed for {url}; active registry is unchanged"
        ));
    }
    Ok(bytes)
}

fn read_download(reader: impl Read, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("registry download exceeded its byte limit".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(super) fn fixture() -> Vec<u8> {
        let text = "schema = 1\nid = 'new-agent'\nname = 'New agent'\naliases = []\nstartable = true\n[launch]\nunix = 'new-agent'\nwindows = 'new-agent'\n";
        let path = "agents/new-agent/agent.toml";
        let hash = sha256(text.as_bytes());
        serde_json::to_vec(&json!({
            "schema":1,
            "compatibility":{"registry_api":1,"min_detection_engine":3},
            "source":{"repository":"https://github.com/herdrdev/agent-registry","commit":"a".repeat(40),"agents_tree":"b".repeat(40),"dirty":false},
            "content_sha256":sha256(format!("{hash}  {path}\n").as_bytes()),
            "files":[{"path":path,"bytes":text.len(),"sha256":hash,"text":text}]
        })).unwrap()
    }

    #[test]
    fn publishing_golden_fixture_validates_with_the_real_package_compiler() {
        let bytes = include_bytes!("../../scripts/fixtures/agent-registry-snapshot-v1.json");
        let snapshot = validate_snapshot(bytes).unwrap();
        assert_eq!(snapshot.files[0].0, "agents/example/agent.toml");
        assert!(snapshot.files[0].1.contains("café"));
    }

    #[test]
    fn bounded_download_counts_streamed_bytes_and_preserves_read_errors() {
        assert_eq!(read_download(&b"1234"[..], 4).unwrap(), b"1234");
        assert!(read_download(&b"12345"[..], 4)
            .unwrap_err()
            .contains("byte limit"));
        struct Interrupted;
        impl Read for Interrupted {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "interrupted response",
                ))
            }
        }
        assert!(read_download(Interrupted, 4)
            .unwrap_err()
            .contains("interrupted response"));
    }

    #[test]
    fn malicious_inventory_and_download_failure_cannot_pass_validation() {
        let mut changed: serde_json::Value = serde_json::from_slice(&fixture()).unwrap();
        let file = &mut changed["files"][0];
        file["path"] = json!("agents/new-agent/../agent.toml");
        let inventory = format!(
            "{}  {}\n",
            file["sha256"].as_str().unwrap(),
            file["path"].as_str().unwrap()
        );
        changed["content_sha256"] = json!(sha256(inventory.as_bytes()));
        assert!(validate_snapshot(&serde_json::to_vec(&changed).unwrap()).is_err());
        let mut duplicate: serde_json::Value = serde_json::from_slice(&fixture()).unwrap();
        let original = duplicate["files"][0].clone();
        duplicate["files"].as_array_mut().unwrap().push(original);
        assert!(validate_snapshot(&serde_json::to_vec(&duplicate).unwrap())
            .unwrap_err()
            .contains("sorted"));
        let failed = download_with(DEFAULT_ORIGIN, Channel::Stable, |_, _, _| {
            Err("offline".into())
        });
        assert_eq!(failed.unwrap_err(), "offline");
        let bytes = fixture();
        let pointer = serde_json::to_vec(&ChannelPointer {
            schema: 1,
            channel: Channel::Stable,
            generation: 1,
            snapshot_sha256: sha256(&bytes),
            snapshot_bytes: bytes.len() as u64,
        })
        .unwrap();
        let mut call = 0;
        let failed = download_with(DEFAULT_ORIGIN, Channel::Stable, |_, _, _| {
            call += 1;
            Ok(if call == 1 {
                pointer.clone()
            } else {
                bytes[..bytes.len() - 1].to_vec()
            })
        });
        assert!(failed.unwrap_err().contains("size/hash"));
    }

    #[test]
    fn remote_snapshot_checks_exact_bytes_and_new_identity() {
        let bytes = fixture();
        let verified = validate_snapshot(&bytes).unwrap();
        assert_eq!(verified.files[0].0, "agents/new-agent/agent.toml");
        let pointer = ChannelPointer {
            schema: 1,
            channel: Channel::Staging,
            generation: 1,
            snapshot_sha256: sha256(&bytes),
            snapshot_bytes: bytes.len() as u64,
        };
        let pointer_bytes = serde_json::to_vec(&pointer).unwrap();
        let mut calls = 0;
        let (revision, downloaded) =
            download_with(DEFAULT_ORIGIN, Channel::Staging, |url, limit, fresh| {
                calls += 1;
                if calls == 1 {
                    assert_eq!(url, "https://registry.herdr.dev/v1/channels/staging.json");
                    assert_eq!(limit, MAX_POINTER_BYTES);
                    assert!(fresh);
                    Ok(pointer_bytes.clone())
                } else {
                    assert_eq!(
                        url,
                        format!(
                            "{DEFAULT_ORIGIN}/v1/snapshots/{}.json",
                            pointer.snapshot_sha256
                        )
                    );
                    assert_eq!(limit, bytes.len());
                    assert!(!fresh, "immutable snapshots may use the CDN cache");
                    Ok(bytes.clone())
                }
            })
            .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(revision.pointer, pointer);
        assert_eq!(downloaded.content_sha256, verified.content_sha256);
    }

    #[test]
    fn equivalent_origins_share_publication_history_identity() {
        assert_eq!(
            normalize_origin("https://REGISTRY.HERDR.DEV:0443/").unwrap(),
            DEFAULT_ORIGIN
        );
        assert_eq!(
            normalize_origin("https://registry.example:8443/").unwrap(),
            "https://registry.example:8443"
        );
        for origin in [
            "https://host:0",
            "https://host:65536",
            "https://host:443:80",
            "https://a..example",
            "https://-host.example",
        ] {
            assert!(normalize_origin(origin).is_err());
        }
    }

    #[test]
    fn rejects_untrusted_urls_and_malformed_snapshot_contracts() {
        for origin in [
            "http://registry.herdr.dev",
            "https://user@host",
            "https://host/path",
            "https://host?x",
            "file:///test",
            "https://host\n",
        ] {
            assert!(normalize_origin(origin).is_err(), "{origin}");
        }
        let original: serde_json::Value = serde_json::from_slice(&fixture()).unwrap();
        for (pointer, replacement) in [
            ("/schema", json!(2)),
            ("/compatibility/registry_api", json!(2)),
            ("/compatibility/min_detection_engine", json!(999)),
            ("/source/dirty", json!(true)),
            ("/source/commit", json!("bad")),
            ("/files/0/bytes", json!(0)),
            ("/files/0/sha256", json!("0".repeat(64))),
            ("/content_sha256", json!("0".repeat(64))),
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(pointer).unwrap() = replacement;
            assert!(
                validate_snapshot(&serde_json::to_vec(&changed).unwrap()).is_err(),
                "{pointer}"
            );
        }
        let duplicate = String::from_utf8(fixture())
            .unwrap()
            .replacen('{', "{\"schema\":1,", 1);
        assert!(validate_snapshot(duplicate.as_bytes()).is_err());
    }

    #[test]
    fn pointer_rejects_mismatch_duplicates_and_oversized_envelopes() {
        let bytes = br#"{"schema":1,"channel":"stable","generation":1,"snapshot_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","snapshot_bytes":12}"#;
        assert!(parse_pointer(bytes, Channel::Stable).is_ok());
        assert!(parse_pointer(bytes, Channel::Preview).is_err());
        assert!(parse_pointer(&vec![b' '; MAX_POINTER_BYTES + 1], Channel::Stable).is_err());
        let duplicate =
            String::from_utf8(bytes.to_vec())
                .unwrap()
                .replacen('{', "{\"schema\":1,", 1);
        assert!(parse_pointer(duplicate.as_bytes(), Channel::Stable).is_err());
    }
}
