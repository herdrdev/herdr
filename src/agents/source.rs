//! Core-independent, bounded parser for the agent source repository.
//! This module deliberately does not bind identities to compiled agents or installers.
use std::collections::{BTreeMap, BTreeSet};

use serde::{de::DeserializeOwned, Deserialize};

#[derive(Debug, Clone)]
pub(crate) struct Package {
    pub(crate) identity: Identity,
    pub(crate) process: Option<ProcessDefinition>,
    pub(crate) resume: Option<ResumeDefinition>,
    pub(crate) integration: Option<IntegrationDefinition>,
    pub(crate) detection: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity {
    pub(crate) schema: u32,
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) aliases: Vec<String>,
    pub(crate) startable: bool,
    pub(crate) launch: LaunchDefinition,
    pub(crate) sound: Option<SoundDefinition>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LaunchDefinition {
    pub(crate) unix: String,
    pub(crate) windows: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SoundDefinition {
    pub(crate) key: String,
    pub(crate) default: SoundDefault,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProcessDefinition {
    pub(crate) names: Vec<String>,
    #[serde(default)]
    pub(crate) secondary_runtime_argv_fallback: bool,
    pub(crate) versioned_basename_prefix: Option<String>,
    #[serde(default)]
    pub(crate) package_paths: Vec<PackagePath>,
    pub(crate) bundled_node: Option<BundledNodeDefinition>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackagePath {
    pub(crate) kind: PackageMatch,
    pub(crate) components: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BundledNodeDefinition {
    pub(crate) runtime_basename: String,
    pub(crate) entrypoint_basename: String,
    pub(crate) package_directory: String,
    pub(crate) versions_directory: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResumeDefinition {
    pub(crate) accepted_references: Vec<ReferenceKind>,
    pub(crate) preferred_reference: ReferenceKind,
    pub(crate) strategy: ResumeStrategy,
    pub(crate) token: String,
    #[serde(default)]
    pub(crate) resume_options: ResumeOptionsDefinition,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResumeOptionsDefinition {
    #[serde(default)]
    pub(crate) flags: Vec<String>,
    #[serde(default)]
    pub(crate) options: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IntegrationDefinition {
    pub(crate) cli_name: String,
    pub(crate) aliases: Vec<String>,
    pub(crate) commands: PlatformCommands,
    pub(crate) supported: PlatformSupport,
    pub(crate) versions: PlatformVersions,
    pub(crate) assets: Vec<AssetDefinition>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlatformCommands {
    pub(crate) unix: Vec<String>,
    pub(crate) windows: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlatformSupport {
    pub(crate) unix: bool,
    pub(crate) windows: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlatformVersions {
    pub(crate) unix: u32,
    pub(crate) windows: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AssetDefinition {
    pub(crate) path: String,
    pub(crate) platform: String,
    pub(crate) role: String,
    pub(crate) install_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SoundDefault {
    Default,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PackageMatch {
    NormalizedComponents,
    ExactSuffix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReferenceKind {
    Id,
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResumeStrategy {
    SeparateFlag,
    JoinedFlag,
    Subcommand,
}

const MAX_FILES: usize = 4096;
const MAX_PACKAGES: usize = 256;
const MAX_TOML_BYTES: usize = 256 * 1024;
const MAX_ASSET_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 32 * 1024 * 1024;
const MAX_LIST: usize = 64;

fn ensure(ok: bool, message: impl Into<String>) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn canonical_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

fn lookup_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && !value.contains("  ")
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"-_ ".contains(&c))
        && value.bytes().any(|c| c.is_ascii_lowercase())
}

/// Safe on both platforms, including Windows device names and trailing-dot rules.
fn basename(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 128
        || value == "."
        || value == ".."
        || value.ends_with('.')
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.@".contains(&c))
    {
        return false;
    }
    let stem = value.split('.').next().unwrap_or("").to_ascii_lowercase();
    !matches!(stem.as_str(), "con" | "prn" | "aux" | "nul" | "clock$")
        && !(stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

fn asset_path(path: &str) -> bool {
    path.strip_prefix("assets/").is_some_and(basename)
}

fn parse<T: DeserializeOwned>(text: &str, path: &str) -> Result<T, String> {
    toml::from_str(text).map_err(|error| format!("{path}: {error}"))
}

fn claim(names: &mut BTreeSet<String>, value: &str, namespace: &str) -> Result<(), String> {
    ensure(
        lookup_name(value),
        format!("invalid {namespace} name: {value}"),
    )?;
    ensure(
        names.insert(value.to_owned()),
        format!("duplicate {namespace} name: {value}"),
    )
}

/// `files` contains UTF-8 text and paths relative to the source root. No filesystem
/// access, enum lookup, detection evaluation, or executable asset evaluation occurs.
pub(crate) fn load_packages(files: &[(&str, &str)]) -> Result<Vec<Package>, String> {
    ensure(
        !files.is_empty() && files.len() <= MAX_FILES,
        "invalid source file count",
    )?;
    let mut total = 0usize;
    let mut directories: BTreeMap<&str, BTreeMap<&str, &str>> = BTreeMap::new();
    // Validate all paths and input bounds before invoking the TOML parser.
    for &(path, text) in files {
        ensure(path.len() <= 256, "source path too long")?;
        let parts: Vec<_> = path.split('/').collect();
        ensure(
            parts.len() >= 3
                && parts[0] == "agents"
                && canonical_id(parts[1])
                && basename(parts[1]),
            format!("invalid source path: {path}"),
        )?;
        let relative = path
            .strip_prefix(&format!("agents/{}/", parts[1]))
            .ok_or_else(|| format!("invalid source path: {path}"))?;
        let is_asset = parts.len() == 4 && asset_path(relative);
        ensure(
            is_asset
                || (parts.len() == 3
                    && matches!(
                        parts[2],
                        "agent.toml"
                            | "process.toml"
                            | "resume.toml"
                            | "integration.toml"
                            | "detection.toml"
                    )),
            format!("unrecognized or unsafe source path: {path}"),
        )?;
        ensure(
            text.len()
                <= if is_asset {
                    MAX_ASSET_BYTES
                } else {
                    MAX_TOML_BYTES
                },
            format!("source file too large: {path}"),
        )?;
        total = total
            .checked_add(text.len())
            .ok_or("source size overflow")?;
        ensure(total <= MAX_TOTAL_BYTES, "source exceeds total size limit")?;
        ensure(!text.contains('\0'), format!("NUL in source file: {path}"))?;
        let directory = directories.entry(parts[1]).or_default();
        ensure(
            directory.insert(relative, text).is_none(),
            format!("duplicate source path: {path}"),
        )?;
        ensure(
            directories.len() <= MAX_PACKAGES,
            "too many source packages",
        )?;
    }
    let mut identities = BTreeSet::new();
    let mut processes = BTreeSet::new();
    let mut sounds = BTreeSet::new();
    let mut integrations = BTreeSet::new();
    let mut prefixes = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut bundled_nodes = BTreeSet::new();
    let mut packages = Vec::new();
    for (directory, files) in directories {
        let context = |file: &str| format!("agents/{directory}/{file}");
        let agent = files
            .get("agent.toml")
            .ok_or_else(|| format!("{directory}: missing agent.toml"))?;
        let identity: Identity = parse(agent, &context("agent.toml"))?;
        ensure(identity.schema == 1, "unsupported agent schema")?;
        ensure(
            canonical_id(&identity.id) && identity.id == directory,
            "agent id must equal package directory",
        )?;
        ensure(
            !identity.name.trim().is_empty()
                && identity.name.len() <= 128
                && identity.name.trim() == identity.name
                && !identity.name.chars().any(char::is_control),
            "invalid agent name",
        )?;
        ensure(identity.aliases.len() <= MAX_LIST, "too many aliases")?;
        claim(&mut identities, &identity.id, "identity")?;
        for alias in &identity.aliases {
            claim(&mut identities, alias, "identity")?;
        }
        ensure(
            basename(&identity.launch.unix) && basename(&identity.launch.windows),
            "unsafe launch executable",
        )?;
        if let Some(sound) = &identity.sound {
            ensure(!sound.key.contains(' '), "invalid sound key")?;
            claim(&mut sounds, &sound.key, "sound")?;
        }
        let process: Option<ProcessDefinition> = files
            .get("process.toml")
            .map(|text| parse(text, &context("process.toml")))
            .transpose()?;
        if let Some(process) = &process {
            ensure(
                !process.names.is_empty() && process.names.len() <= MAX_LIST,
                "invalid process name count",
            )?;
            for name in &process.names {
                ensure(
                    name.len() <= 128 && lookup_name(name.strip_prefix('.').unwrap_or(name)),
                    format!("invalid process name: {name}"),
                )?;
                ensure(
                    processes.insert(name.clone()),
                    format!("duplicate process name: {name}"),
                )?;
            }
            if let Some(prefix) = &process.versioned_basename_prefix {
                ensure(
                    basename(prefix)
                        && prefix.ends_with('-')
                        && prefix == &prefix.to_ascii_lowercase(),
                    "invalid versioned basename prefix",
                )?;
                ensure(
                    prefixes.insert(prefix.clone()),
                    "duplicate versioned basename prefix",
                )?;
            }
            ensure(
                process.package_paths.len() <= MAX_LIST,
                "too many package paths",
            )?;
            for path in &process.package_paths {
                ensure(
                    !path.components.is_empty()
                        && path.components.len() <= 32
                        && path.components.iter().all(|c| basename(c)),
                    "unsafe package path components",
                )?;
                ensure(
                    paths.insert((
                        path.kind,
                        path.components
                            .iter()
                            .map(|c| c.to_ascii_lowercase())
                            .collect::<Vec<_>>(),
                    )),
                    "duplicate package path",
                )?;
            }
            if let Some(node) = &process.bundled_node {
                ensure(
                    [
                        &node.runtime_basename,
                        &node.entrypoint_basename,
                        &node.package_directory,
                        &node.versions_directory,
                    ]
                    .iter()
                    .all(|s| basename(s)),
                    "unsafe bundled node layout",
                )?;
                ensure(
                    bundled_nodes.insert([
                        node.runtime_basename.to_ascii_lowercase(),
                        node.entrypoint_basename.to_ascii_lowercase(),
                        node.package_directory.to_ascii_lowercase(),
                        node.versions_directory.to_ascii_lowercase(),
                    ]),
                    "duplicate bundled node layout",
                )?;
            }
        }
        let resume: Option<ResumeDefinition> = files
            .get("resume.toml")
            .map(|text| parse(text, &context("resume.toml")))
            .transpose()?;
        if let Some(resume) = &resume {
            validate_resume(resume)?;
        }
        let integration: Option<IntegrationDefinition> = files
            .get("integration.toml")
            .map(|text| parse(text, &context("integration.toml")))
            .transpose()?;
        if let Some(integration) = &integration {
            ensure(
                integration.aliases.len() <= MAX_LIST,
                "too many integration aliases",
            )?;
            claim(&mut integrations, &integration.cli_name, "integration")?;
            for alias in &integration.aliases {
                claim(&mut integrations, alias, "integration")?;
            }
            validate_integration(&identity.id, integration, &files)?;
        } else {
            ensure(
                !files.keys().any(|p| p.starts_with("assets/")),
                "assets require integration.toml",
            )?;
        }
        let detection = files
            .get("detection.toml")
            .map(|text| {
                let _: toml::Table = parse(text, &context("detection.toml"))?;
                Ok::<_, String>((*text).to_owned())
            })
            .transpose()?;
        packages.push(Package {
            identity,
            process,
            resume,
            integration,
            detection,
        });
    }
    // Prefix recognition requires a following digit; reject exact names and
    // longer prefixes that would recognize the same versioned basename.
    for prefix in &prefixes {
        let matches_prefix = |name: &str| {
            name.strip_prefix(prefix.as_str())
                .is_some_and(|tail| tail.as_bytes().first().is_some_and(u8::is_ascii_digit))
        };
        ensure(
            !processes.iter().any(|name| matches_prefix(name)),
            "versioned basename prefix overlaps process name",
        )?;
        ensure(
            !prefixes.iter().any(|other| matches_prefix(other)),
            "overlapping versioned basename prefixes",
        )?;
    }
    Ok(packages)
}

fn validate_resume(resume: &ResumeDefinition) -> Result<(), String> {
    let refs = &resume.accepted_references;
    ensure(
        !refs.is_empty()
            && refs.len() <= 2
            && refs.iter().collect::<BTreeSet<_>>().len() == refs.len(),
        "invalid accepted resume references",
    )?;
    ensure(
        refs.contains(&resume.preferred_reference),
        "preferred resume reference must be accepted",
    )?;
    let word = |s: &str| {
        !s.is_empty()
            && s.len() <= 64
            && s.as_bytes()[0].is_ascii_lowercase()
            && s.bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    };
    let valid = match resume.strategy {
        ResumeStrategy::SeparateFlag => resume.token.strip_prefix("--").is_some_and(word),
        ResumeStrategy::JoinedFlag => resume
            .token
            .strip_prefix("--")
            .and_then(|s| s.strip_suffix('='))
            .is_some_and(word),
        ResumeStrategy::Subcommand => word(&resume.token),
    };
    ensure(valid, "unknown resume strategy or unsafe token")?;
    let policy = &resume.resume_options;
    ensure(
        policy.flags.len() + policy.options.len() <= MAX_LIST,
        "too many resume options",
    )?;
    let mut names = BTreeSet::new();
    for name in policy.flags.iter().chain(&policy.options) {
        ensure(
            name.len() <= 64
                && name
                    .strip_prefix("--")
                    .or_else(|| name.strip_prefix('-'))
                    .is_some_and(word)
                && !super::session::reserved_resume_option(name)
                && name != resume.token.trim_end_matches('=')
                && names.insert(name),
            "invalid, duplicate or reserved resume option",
        )?;
    }
    Ok(())
}

fn validate_integration(
    id: &str,
    integration: &IntegrationDefinition,
    files: &BTreeMap<&str, &str>,
) -> Result<(), String> {
    ensure(
        integration.supported.unix || integration.supported.windows,
        "integration supports no platforms",
    )?;
    ensure(
        !integration.assets.is_empty() && integration.assets.len() <= MAX_LIST,
        "invalid integration asset count",
    )?;
    for (supported, commands, version) in [
        (
            integration.supported.unix,
            &integration.commands.unix,
            integration.versions.unix,
        ),
        (
            integration.supported.windows,
            &integration.commands.windows,
            integration.versions.windows,
        ),
    ] {
        ensure(
            commands.len() <= MAX_LIST
                && commands.iter().all(|c| basename(c))
                && commands
                    .iter()
                    .map(|c| c.to_ascii_lowercase())
                    .collect::<BTreeSet<_>>()
                    .len()
                    == commands.len(),
            "invalid integration commands",
        )?;
        ensure(
            if supported {
                !commands.is_empty() && version > 0
            } else {
                commands.is_empty() && version == 0
            },
            "integration platform support, commands and versions disagree",
        )?;
    }
    let mut referenced = BTreeSet::new();
    let mut roles = BTreeSet::new();
    let mut installed = BTreeSet::new();
    for asset in &integration.assets {
        ensure(
            asset_path(&asset.path) && basename(&asset.install_name),
            "unsafe integration asset path or install name",
        )?;
        ensure(
            matches!(asset.role.as_str(), "reporter" | "tui" | "manifest"),
            "unknown integration asset role",
        )?;
        ensure(
            matches!(asset.platform.as_str(), "all" | "unix" | "windows"),
            "unknown integration asset platform",
        )?;
        let text = files
            .get(asset.path.as_str())
            .ok_or_else(|| format!("{id}: missing asset {}", asset.path))?;
        referenced.insert(asset.path.as_str());
        let marker_id = if id == "agy" {
            "antigravity_cli".to_owned()
        } else if asset.role == "tui" {
            format!("{id}-tui")
        } else {
            id.to_owned()
        };
        let ids = markers(text, "HERDR_INTEGRATION_ID=");
        ensure(
            ids.len() <= 1 && ids.iter().all(|value| *value == marker_id),
            "integration asset ID marker mismatch",
        )?;
        let versions = markers(text, "HERDR_INTEGRATION_VERSION=");
        ensure(versions.len() <= 1, "duplicate integration version markers")?;
        for (platform, supported, version) in [
            (
                "unix",
                integration.supported.unix,
                integration.versions.unix,
            ),
            (
                "windows",
                integration.supported.windows,
                integration.versions.windows,
            ),
        ] {
            if asset.platform != "all" && asset.platform != platform {
                continue;
            }
            ensure(supported, "asset targets unsupported platform")?;
            ensure(
                roles.insert((platform, asset.role.as_str())),
                "duplicate asset role on platform",
            )?;
            ensure(
                installed.insert((platform, asset.install_name.to_ascii_lowercase())),
                "duplicate asset install name on platform",
            )?;
            for value in &versions {
                ensure(
                    value.parse::<u32>().ok() == Some(version),
                    "integration asset version marker mismatch",
                )?;
            }
        }
    }
    ensure(
        files
            .keys()
            .filter(|path| path.starts_with("assets/"))
            .all(|path| referenced.contains(path)),
        "unreferenced integration asset",
    )?;
    for (platform, supported) in [
        ("unix", integration.supported.unix),
        ("windows", integration.supported.windows),
    ] {
        ensure(
            !supported || roles.contains(&(platform, "reporter")),
            "supported integration platform requires reporter asset",
        )?;
    }
    Ok(())
}

fn markers<'a>(text: &'a str, key: &str) -> Vec<&'a str> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let comment = line.strip_prefix('#').or_else(|| line.strip_prefix("//"))?;
            comment.trim().strip_prefix(key).map(str::trim)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    const AGENT: &str = "schema = 1\nid = 'future-agent'\nname = 'Future agent'\naliases = ['future agent']\nstartable = true\n[launch]\nunix = 'future-agent'\nwindows = 'future-agent.cmd'\n";
    const INTEGRATION: &str = "cli_name = 'future-agent'\naliases = []\n[commands]\nunix = ['future-agent']\nwindows = ['future-agent']\n[supported]\nunix = true\nwindows = true\n[versions]\nunix = 2\nwindows = 2\n[[assets]]\npath = 'assets/reporter.js'\nplatform = 'all'\nrole = 'reporter'\ninstall_name = 'reporter.js'\n";

    #[test]
    fn unknown_identity_and_missing_optional_capabilities_work() {
        let packages = load_packages(&[("agents/future-agent/agent.toml", AGENT)]).unwrap();
        let package = &packages[0];
        assert_eq!(package.identity.id, "future-agent");
        assert!(
            package.process.is_none()
                && package.resume.is_none()
                && package.integration.is_none()
                && package.detection.is_none()
        );
    }

    #[test]
    fn hidden_process_names_do_not_widen_identity_or_sound_names() {
        let process = "names = ['future-agent', '.future-agent']";
        let packages = load_packages(&[
            ("agents/future-agent/agent.toml", AGENT),
            ("agents/future-agent/process.toml", process),
        ])
        .unwrap();
        assert_eq!(
            packages[0].process.as_ref().unwrap().names,
            ["future-agent", ".future-agent"]
        );
        for name in [
            ".",
            "..future-agent",
            "../future-agent",
            &format!(".{}", "a".repeat(128)),
        ] {
            assert!(load_packages(&[
                ("agents/future-agent/agent.toml", AGENT),
                (
                    "agents/future-agent/process.toml",
                    &format!("names = ['{name}']")
                ),
            ])
            .is_err());
        }
        for identity in [
            AGENT.replace("['future agent']", "['.future-agent']"),
            format!("{AGENT}\n[sound]\nkey = '.future-agent'\ndefault = 'default'\n"),
        ] {
            assert!(load_packages(&[("agents/future-agent/agent.toml", &identity)]).is_err());
        }
    }

    #[test]
    fn rejects_unknown_fields_even_in_nested_structs() {
        for extra in ["extra = true\n", "authority = 'full'\n"] {
            assert!(load_packages(&[(
                "agents/future-agent/agent.toml",
                &format!("{AGENT}{extra}")
            )])
            .is_err());
        }
        assert!(load_packages(&[
            ("agents/future-agent/agent.toml", AGENT),
            (
                "agents/future-agent/process.toml",
                "names = ['future-agent']\nadapter = 'magic'"
            )
        ])
        .is_err());
    }

    #[test]
    fn rejects_missing_identity_and_unsafe_paths() {
        for path in [
            "agents/future-agent/../agent.toml",
            "/agents/future-agent/agent.toml",
            "agents\\future-agent\\agent.toml",
            "agents/future-agent/other.toml",
            "agents/future-agent/assets/../x",
            "agents/future-agent/assets/CON.txt",
            "agents/con/agent.toml",
        ] {
            assert!(load_packages(&[(path, AGENT)]).is_err(), "{path}");
        }
        assert!(load_packages(&[(
            "agents/future-agent/process.toml",
            "names = ['future-agent']"
        )])
        .is_err());
        assert!(load_packages(&[("agents/other/agent.toml", AGENT)]).is_err());
        for executable in ["../future", "C:\\evil.exe", "NUL", "future.", "x;sh"] {
            assert!(load_packages(&[(
                "agents/future-agent/agent.toml",
                &AGENT.replace("'future-agent.cmd'", &format!("'{executable}'"))
            )])
            .is_err());
        }
    }

    #[test]
    fn namespace_collisions_are_rejected_but_namespaces_are_independent() {
        let other = AGENT.replace("future-agent", "other");
        assert!(load_packages(&[
            ("agents/future-agent/agent.toml", AGENT),
            ("agents/other/agent.toml", &other)
        ])
        .is_err());
        let other = other.replace("future agent", "other alias");
        let files = [
            ("agents/future-agent/agent.toml", AGENT),
            ("agents/other/agent.toml", &other),
            ("agents/future-agent/process.toml", "names = ['other']"),
        ];
        assert!(load_packages(&files).is_ok());
        let mut files = files.to_vec();
        files.push(("agents/other/process.toml", "names = ['other']"));
        assert!(load_packages(&files).is_err());
    }

    #[test]
    fn resume_uses_closed_reference_and_argv_forms() {
        for (strategy, token, valid) in [
            ("separate_flag", "--resume", true),
            ("joined_flag", "--resume=", true),
            ("subcommand", "resume", true),
            ("shell", "resume", false),
            ("joined_flag", "--resume", false),
            ("separate_flag", "--resume=", false),
            ("subcommand", "resume;sh", false),
        ] {
            let text = format!("accepted_references = ['id']\npreferred_reference = 'id'\nstrategy = '{strategy}'\ntoken = '{token}'");
            assert_eq!(
                load_packages(&[
                    ("agents/future-agent/agent.toml", AGENT),
                    ("agents/future-agent/resume.toml", &text)
                ])
                .is_ok(),
                valid
            );
        }
    }

    #[test]
    fn resume_options_policy_is_bounded_unique_and_cannot_own_session_selection() {
        let base = "accepted_references = ['id']\npreferred_reference = 'id'\nstrategy = 'joined_flag'\ntoken = '--restore='\n";
        for (policy, valid) in [
            ("", true),
            (
                "[resume_options]\nflags=['--yolo','-f']\noptions=['--model']",
                true,
            ),
            (
                "[resume_options]\nflags=['--yolo']\noptions=['--yolo']",
                false,
            ),
            ("[resume_options]\noptions=['--restore']", false),
            ("[resume_options]\nflags=['--continue']", false),
            ("[resume_options]\noptions=['-r']", false),
            ("[resume_options]\noptions=['--model=value']", false),
            ("[resume_options]\nflags=['--']", false),
            ("[resume_options]\nunknown=[]", false),
        ] {
            let text = format!("{base}{policy}");
            assert_eq!(
                load_packages(&[
                    ("agents/future-agent/agent.toml", AGENT),
                    ("agents/future-agent/resume.toml", &text),
                ])
                .is_ok(),
                valid,
                "{policy}"
            );
        }
        let mut resume: ResumeDefinition = toml::from_str(base).unwrap();
        resume.resume_options.flags = (0..=MAX_LIST).map(|i| format!("--flag-{i}")).collect();
        assert!(validate_resume(&resume).is_err());
    }

    #[test]
    fn validates_asset_markers_references_and_roles() {
        let good = "// HERDR_INTEGRATION_ID=future-agent\n// HERDR_INTEGRATION_VERSION=2\n";
        let test = |definition: &str, asset: &str| {
            load_packages(&[
                ("agents/future-agent/agent.toml", AGENT),
                ("agents/future-agent/integration.toml", definition),
                ("agents/future-agent/assets/reporter.js", asset),
            ])
        };
        assert!(test(INTEGRATION, good).is_ok());
        assert!(test(INTEGRATION, &good.replace("VERSION=2", "VERSION=3")).is_err());
        assert!(test(INTEGRATION, &good.replace("ID=future-agent", "ID=other")).is_err());
        assert!(test(
            &INTEGRATION.replace("assets/reporter.js", "assets/missing.js"),
            good
        )
        .is_err());
        assert!(test(
            &INTEGRATION.replace("assets/reporter.js", "../reporter.js"),
            good
        )
        .is_err());
        assert!(test(
            &format!(
                "{INTEGRATION}{}",
                INTEGRATION
                    .split("[[assets]]")
                    .nth(1)
                    .map(|s| format!("[[assets]]{s}"))
                    .unwrap()
            ),
            good
        )
        .is_err());
        assert!(test(&INTEGRATION.replace("windows = 2", "windows = 3"), good).is_err());
    }

    #[test]
    fn rejects_bad_identity_schema_aliases_and_sound_policy() {
        for text in [
            AGENT.replace("schema = 1", "schema = 2"),
            AGENT.replace("future agent", "Future Agent"),
            AGENT.replace("future agent", " future agent"),
            AGENT.replace("future agent", "future  agent"),
            AGENT.replace("['future agent']", "['future agent', 'future agent']"),
            format!("{AGENT}\n[sound]\nkey = 'future'\ndefault = 'custom'"),
        ] {
            assert!(load_packages(&[("agents/future-agent/agent.toml", &text)]).is_err());
        }
    }

    #[test]
    fn sound_and_integration_lookup_collisions_are_rejected() {
        let first = format!("{AGENT}\n[sound]\nkey = 'shared'\ndefault = 'off'");
        let second = first
            .replace("future-agent", "other")
            .replace("future agent", "other alias");
        assert!(load_packages(&[
            ("agents/future-agent/agent.toml", &first),
            ("agents/other/agent.toml", &second)
        ])
        .unwrap_err()
        .contains("duplicate sound"));
        let second = second.replace("key = 'shared'", "key = 'other'");
        let files = [
            ("agents/future-agent/agent.toml", first.as_str()),
            ("agents/other/agent.toml", second.as_str()),
            ("agents/future-agent/integration.toml", INTEGRATION),
            ("agents/future-agent/assets/reporter.js", "// reporter"),
            ("agents/other/integration.toml", INTEGRATION),
            ("agents/other/assets/reporter.js", "// reporter"),
        ];
        assert!(load_packages(&files)
            .unwrap_err()
            .contains("duplicate integration"));
    }

    #[test]
    fn process_semantics_and_nested_fields_are_closed() {
        for text in [
            "names = []",
            "names = ['future', 'future']",
            "names = ['Future']",
            "names = ['future']\nversioned_basename_prefix = '../future-'",
            "names = ['future']\n[[package_paths]]\nkind = 'glob'\ncomponents = ['future']",
            "names = ['future']\n[[package_paths]]\nkind = 'exact_suffix'\ncomponents = ['..']",
            "names = ['future']\n[[package_paths]]\nkind = 'exact_suffix'\ncomponents = ['future']\nextra = true",
            "names = ['future']\n[bundled_node]\nruntime_basename = 'node.exe'\nentrypoint_basename = '../index.js'\npackage_directory = 'future'\nversions_directory = 'versions'",
        ] {
            assert!(load_packages(&[("agents/future-agent/agent.toml", AGENT),
                ("agents/future-agent/process.toml", text)]).is_err(), "{text}");
        }
    }

    #[test]
    fn process_matchers_cannot_collide_across_packages() {
        let other = AGENT
            .replace("future-agent", "other")
            .replace("future agent", "other alias");
        let check = |first: &str, second: &str| {
            let first = format!("names = ['future-agent']\n{first}");
            let second = format!("names = ['other']\n{second}");
            load_packages(&[
                ("agents/future-agent/agent.toml", AGENT),
                ("agents/future-agent/process.toml", &first),
                ("agents/other/agent.toml", &other),
                ("agents/other/process.toml", &second),
            ])
        };
        for kind in ["normalized_components", "exact_suffix"] {
            let path = format!("[[package_paths]]\nkind = '{kind}'\ncomponents = ['node_modules', 'shared', 'cli.js']");
            assert!(check(&path, &path.replace("shared", "SHARED"))
                .unwrap_err()
                .contains("duplicate package path"));
            assert!(check(&path, &path.replace("shared", "different")).is_ok());
        }
        let node = "[bundled_node]\nruntime_basename = 'node.exe'\nentrypoint_basename = 'index.js'\npackage_directory = 'shared'\nversions_directory = 'versions'";
        assert!(check(node, &node.replace("node.exe", "NODE.EXE"))
            .unwrap_err()
            .contains("duplicate bundled node"));
        assert!(check(node, &node.replace("shared", "different")).is_ok());
        let prefix = "versioned_basename_prefix = 'shared-bin-'";
        let overlap = "versioned_basename_prefix = 'shared-bin-1-'";
        for (first, second) in [(prefix, overlap), (overlap, prefix)] {
            assert!(check(first, second)
                .unwrap_err()
                .contains("overlapping versioned basename prefixes"));
        }
        assert!(check(prefix, "versioned_basename_prefix = 'shared-bin-other-'").is_ok());
    }

    #[test]
    fn resume_references_must_be_known_unique_and_include_preference() {
        for (references, preferred) in [
            ("[]", "id"),
            ("['id', 'id']", "id"),
            ("['id']", "path"),
            ("['directory']", "directory"),
        ] {
            let text = format!("accepted_references = {references}\npreferred_reference = '{preferred}'\nstrategy = 'separate_flag'\ntoken = '--resume'");
            assert!(load_packages(&[
                ("agents/future-agent/agent.toml", AGENT),
                ("agents/future-agent/resume.toml", &text)
            ])
            .is_err());
        }
    }

    #[test]
    fn detection_is_generic_toml_not_bound_to_core_identity() {
        let detection = "historical_identity = 'an-alias'\n[rules]\nfuture_rule = true\n";
        let packages = load_packages(&[
            ("agents/future-agent/agent.toml", AGENT),
            ("agents/future-agent/detection.toml", detection),
        ])
        .unwrap();
        assert_eq!(packages[0].detection.as_deref(), Some(detection));
        assert!(load_packages(&[
            ("agents/future-agent/agent.toml", AGENT),
            ("agents/future-agent/detection.toml", "[broken")
        ])
        .is_err());
    }

    #[test]
    fn integration_assets_require_declared_supported_roles() {
        let test = |definition: &str| {
            load_packages(&[
                ("agents/future-agent/agent.toml", AGENT),
                ("agents/future-agent/integration.toml", definition),
                ("agents/future-agent/assets/reporter.js", "// reporter"),
            ])
        };
        for definition in [
            INTEGRATION.replace("role = 'reporter'", "role = 'installer'"),
            INTEGRATION.replace("platform = 'all'", "platform = 'linux'"),
            INTEGRATION.replace("windows = true", "windows = false"),
            INTEGRATION.replace("windows = 2", "windows = 0"),
            INTEGRATION.replace("install_name = 'reporter.js'", "install_name = 'CON.txt'"),
            format!("{INTEGRATION}adapter = 'custom'\n"),
        ] {
            assert!(test(&definition).is_err(), "{definition}");
        }
        assert!(load_packages(&[
            ("agents/future-agent/agent.toml", AGENT),
            ("agents/future-agent/assets/reporter.js", "// orphan")
        ])
        .is_err());
        assert!(load_packages(&[
            ("agents/future-agent/agent.toml", AGENT),
            ("agents/future-agent/integration.toml", INTEGRATION),
            ("agents/future-agent/assets/reporter.js", "// reporter"),
            ("agents/future-agent/assets/orphan.js", "// orphan")
        ])
        .is_err());
    }

    #[test]
    fn historical_agy_and_tui_markers_are_preserved() {
        let agy = AGENT.replace("future-agent", "agy");
        assert!(load_packages(&[
            ("agents/agy/agent.toml", &agy),
            ("agents/agy/integration.toml", INTEGRATION),
            (
                "agents/agy/assets/reporter.js",
                "# HERDR_INTEGRATION_ID=antigravity_cli\n# HERDR_INTEGRATION_VERSION=2"
            )
        ])
        .is_ok());
        let definition = format!("{INTEGRATION}\n[[assets]]\npath = 'assets/tui.js'\nplatform = 'all'\nrole = 'tui'\ninstall_name = 'tui.js'\n");
        assert!(load_packages(&[
            ("agents/future-agent/agent.toml", AGENT),
            ("agents/future-agent/integration.toml", &definition),
            ("agents/future-agent/assets/reporter.js", "// reporter"),
            (
                "agents/future-agent/assets/tui.js",
                "// HERDR_INTEGRATION_ID=future-agent-tui\n// HERDR_INTEGRATION_VERSION=2"
            )
        ])
        .is_ok());
    }

    #[test]
    fn rejects_oversize_before_parsing_and_duplicate_files() {
        let large = " ".repeat(MAX_TOML_BYTES + 1);
        assert!(load_packages(&[("agents/future-agent/agent.toml", &large)])
            .unwrap_err()
            .contains("too large"));
        assert!(load_packages(&[("agents/future-agent/agent.toml", AGENT); 2]).is_err());
    }
}
