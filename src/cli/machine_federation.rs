use std::io;
use std::time::Duration;

use toml_edit::{value, DocumentMut, Item, Table, Value};

use crate::api::client::{parse_response_value, ApiClient, ConnectionTarget};
use crate::api::schema::{AgentListParams, EmptyParams, Method, Request, ResponseResult};
use crate::client::endpoint::{EndpointCatalog, ProfileId, SavedSshEndpoint};
use crate::config::{ConfigReloadStatus, FederationConfig};

#[derive(Debug)]
struct LegacyPeer {
    alias: String,
    label: String,
    target: String,
    session: String,
    expected_id: Option<String>,
    profile_id: Option<ProfileId>,
}

fn read_config() -> Result<String, String> {
    match crate::config::read_optional_config(&crate::config::config_path()) {
        Ok(Some(content)) => Ok(content),
        Ok(None) => Ok(String::new()),
        Err(error) => Err(format!("failed to read config: {error}")),
    }
}

fn resolve_profile(
    catalog: &EndpointCatalog,
    selector: &str,
) -> Result<Option<SavedSshEndpoint>, String> {
    if let Ok(id) = ProfileId::parse(selector.to_owned()) {
        if let Some(profile) = catalog.ssh.iter().find(|profile| profile.id == id) {
            return Ok(Some(profile.clone()));
        }
    }
    let mut matches = catalog
        .ssh
        .iter()
        .filter(|profile| profile.label == selector);
    let first = matches.next().cloned();
    if matches.next().is_some() {
        return Err(format!(
            "machine label {selector:?} is ambiguous; use a profile ID"
        ));
    }
    Ok(first)
}

fn legacy_peers(content: &str) -> Result<Vec<LegacyPeer>, String> {
    let doc = content
        .parse::<DocumentMut>()
        .map_err(|error| format!("invalid config TOML: {error}"))?;
    let Some(peers) = doc
        .get("federation")
        .and_then(|fed| fed.get("peers"))
        .and_then(Item::as_array_of_tables)
    else {
        return Ok(Vec::new());
    };
    let mut result = Vec::new();
    for peer in peers {
        let Some(target) = peer
            .get("endpoint")
            .and_then(Item::as_str)
            .and_then(|endpoint| endpoint.strip_prefix("ssh://"))
        else {
            continue;
        };
        for (key, _) in peer.iter() {
            if !matches!(
                key,
                "alias"
                    | "display_label"
                    | "endpoint"
                    | "remote_session"
                    | "expected_node_id"
                    | "profile_id"
            ) {
                return Err(format!(
                    "legacy SSH peer has unsupported {key:?}; refusing to change its trust policy"
                ));
            }
        }
        let alias = peer
            .get("alias")
            .and_then(Item::as_str)
            .ok_or("legacy SSH peer has no alias")?
            .to_owned();
        let label = peer
            .get("display_label")
            .and_then(Item::as_str)
            .unwrap_or(&alias)
            .to_owned();
        let session = peer
            .get("remote_session")
            .and_then(Item::as_str)
            .unwrap_or("default")
            .to_owned();
        let expected_id = peer
            .get("expected_node_id")
            .map(|item| {
                item.as_str()
                    .ok_or("invalid legacy machine ID")
                    .map(str::to_owned)
            })
            .transpose()?;
        let profile_id = peer
            .get("profile_id")
            .map(|item| {
                item.as_str()
                    .ok_or("invalid legacy profile ID")
                    .and_then(|id| {
                        ProfileId::parse(id.to_owned()).map_err(|_| "invalid legacy profile ID")
                    })
            })
            .transpose()?;
        if result.iter().any(|old: &LegacyPeer| {
            old.alias == alias || (old.target == target && old.session == session)
        }) {
            return Err(format!(
                "duplicate legacy SSH peer {alias:?}; migrate peers separately before federating"
            ));
        }
        result.push(LegacyPeer {
            alias,
            label,
            target: target.to_owned(),
            session,
            expected_id,
            profile_id,
        });
    }
    Ok(result)
}

fn remote_machine_id(profile: &SavedSshEndpoint) -> Result<String, String> {
    let bridge = crate::remote::SavedSshApiBridge::start(
        profile.id.as_str(),
        &profile.target,
        &profile.session,
        false,
        // Read-only identity probe: one retry after shared transport recovery is safe.
        true,
    )
    .map_err(|error| format!("could not reach {}: {error}", profile.label))?;
    let client = ApiClient::for_target(ConnectionTarget::SocketPath(
        bridge.socket_path().to_path_buf(),
    ));
    let response = client
        .request_value_bounded(
            &Request {
                id: "machine:federate:identity".into(),
                method: Method::AgentList(AgentListParams { local_only: true }),
            },
            1024 * 1024,
            Duration::from_secs(5),
            None,
        )
        .map_err(|error| format!("could not read {} identity: {error}", profile.label))?;
    let response = parse_response_value(response)
        .map_err(|error| format!("invalid {} identity response: {error}", profile.label))?;
    let ResponseResult::AgentList {
        origin_machine_id: Some(id),
        ..
    } = response.result
    else {
        return Err(format!(
            "{} did not report an install machine ID; upgrade its Herdr server before federating",
            profile.label
        ));
    };
    if id.is_empty() || id.len() > 128 || id.trim() != id || id.chars().any(char::is_control) {
        return Err(format!(
            "{} reported an invalid install machine ID",
            profile.label
        ));
    }
    Ok(id)
}

fn edit_policy(
    content: &str,
    pins: &[(ProfileId, String)],
    remove: Option<&ProfileId>,
    migrated_aliases: &[String],
) -> Result<String, String> {
    let mut doc = content
        .parse::<DocumentMut>()
        .map_err(|error| format!("invalid config TOML: {error}"))?;
    if doc.get("federation").is_none() {
        doc["federation"] = Item::Table(Table::new());
    }
    if !doc["federation"].is_table() {
        return Err("federation config must be a table".into());
    }
    if !migrated_aliases.is_empty() {
        let peers = doc["federation"]["peers"]
            .as_array_of_tables_mut()
            .ok_or("legacy federation.peers is not an array of tables")?;
        peers.retain(|peer| {
            let migrated_ssh = peer
                .get("endpoint")
                .and_then(Item::as_str)
                .is_some_and(|endpoint| endpoint.starts_with("ssh://"))
                && peer
                    .get("alias")
                    .and_then(Item::as_str)
                    .is_some_and(|alias| migrated_aliases.iter().any(|selected| selected == alias));
            !migrated_ssh
        });
        if peers.is_empty() {
            doc["federation"].as_table_mut().unwrap().remove("peers");
        }
    }
    if !pins.is_empty() {
        doc["federation"]["coordinator"] = value(true);
        if doc["federation"].get("saved_machines").is_none() {
            doc["federation"]
                .as_table_mut()
                .unwrap()
                .insert("saved_machines", Item::Table(Table::new()));
        }
        let policies = doc["federation"]["saved_machines"]
            .as_table_mut()
            .ok_or("federation.saved_machines must be a table")?;
        for (id, machine_id) in pins {
            if let Some(existing) = policies.get(id.as_str()) {
                let pinned = existing
                    .get("expected_machine_id")
                    .and_then(Item::as_str)
                    .ok_or("saved machine policy has no expected_machine_id")?;
                if pinned != machine_id {
                    return Err(format!("machine {} identity changed: pinned {pinned}, observed {machine_id}; refusing to replace the pin", id));
                }
                continue;
            }
            let mut policy = Table::new();
            policy["expected_machine_id"] = value(machine_id.as_str());
            policies.insert(id.as_str(), Item::Table(policy));
        }
    }
    if let Some(id) = remove {
        let federation = doc["federation"]
            .as_table_mut()
            .ok_or("federation config must be a table")?;
        if let Some(item) = federation.get_mut("saved_machines") {
            let empty = match item {
                Item::Table(policies) => {
                    policies.remove(id.as_str());
                    policies.is_empty()
                }
                Item::Value(Value::InlineTable(policies)) => {
                    policies.remove(id.as_str());
                    policies.is_empty()
                }
                _ => return Err("federation.saved_machines must be a table".into()),
            };
            if empty {
                federation.remove("saved_machines");
            }
        }
    }
    let updated = doc.to_string();
    let parsed = updated
        .parse::<toml::Value>()
        .map_err(|error| format!("invalid updated config: {error}"))?;
    if legacy_peers(&updated)?.is_empty() {
        if let Some(federation) = parsed.get("federation") {
            let _: FederationConfig = federation
                .clone()
                .try_into()
                .map_err(|error| format!("invalid updated federation config: {error}"))?;
        }
    }
    Ok(updated)
}

fn store_if_unchanged(original: &str, updated: &str) -> Result<(), String> {
    crate::config::update_file_at_checked(
        &crate::config::config_path(),
        "federation policy",
        |current| {
            if current != original {
                return Err(
                    "config changed while checking the remote identity; retry federation command"
                        .into(),
                );
            }
            Ok(updated.to_owned())
        },
    )
}

fn reload_live() -> i32 {
    let response = ApiClient::local().request(Request {
        id: "machine:federation:reload".into(),
        method: Method::ServerReloadConfig(EmptyParams {}),
    });
    match response {
        Ok(response) => match response.result {
            ResponseResult::ConfigReload {
                status: ConfigReloadStatus::Applied,
                ..
            } => 0,
            ResponseResult::ConfigReload {
                status,
                diagnostics,
            } => {
                eprintln!(
                    "policy saved, but live reload {status:?}: {}",
                    diagnostics.join("; ")
                );
                1
            }
            _ => {
                eprintln!("policy saved, but daemon returned an unexpected reload response");
                1
            }
        },
        Err(error) => {
            eprintln!("policy saved, but live reload failed: {error}; run `herdr server reload-config` after starting the daemon");
            1
        }
    }
}

pub(super) fn federate(args: &[String]) -> io::Result<i32> {
    let (selector, migrate_all) = match args {
        [selector] => (selector, false),
        [flag, selector] | [selector, flag] if flag == "--migrate-all-legacy" => (selector, true),
        _ => {
            eprintln!("usage: herdr machine federate <label-or-id> [--migrate-all-legacy]");
            return Ok(2);
        }
    };
    let original_catalog = EndpointCatalog::load().map_err(io::Error::other)?;
    let mut catalog = original_catalog.clone();
    let original_config = read_config().map_err(io::Error::other)?;
    let legacy = legacy_peers(&original_config).map_err(io::Error::other)?;
    let selected = resolve_profile(&catalog, selector).map_err(io::Error::other)?;
    let mut legacy_matches = legacy.iter().filter(|peer| {
        peer.alias == *selector
            || peer.label == *selector
            || selected.as_ref().is_some_and(|profile| {
                peer.profile_id.as_ref() == Some(&profile.id)
                    || (peer.target == profile.target && peer.session == profile.session)
            })
    });
    let selected_legacy = legacy_matches.next();
    if legacy_matches.next().is_some() {
        return Err(io::Error::other(format!(
            "legacy machine {selector:?} is ambiguous"
        )));
    }
    if let (Some(profile), Some(peer)) = (selected.as_ref(), selected_legacy) {
        if profile.target != peer.target || profile.session != peer.session {
            return Err(io::Error::other(format!(
                "machine {selector:?} matches different saved and legacy peers; use the saved profile ID"
            )));
        }
    }
    if selected.is_none() && selected_legacy.is_none() {
        eprintln!("machine {selector:?} was not found in saved profiles or legacy SSH peers");
        return Ok(1);
    }
    if !migrate_all && !legacy.is_empty() && (legacy.len() > 1 || selected_legacy.is_none()) {
        return Err(io::Error::other(
            "legacy SSH peers remain in config; use `herdr machine federate <label-or-id> --migrate-all-legacy` to migrate them together, or remove them before opting in a different saved machine",
        ));
    }
    let peers: Vec<_> = if migrate_all {
        legacy.iter().collect()
    } else {
        selected_legacy.into_iter().collect()
    };
    let mut profiles: Vec<(SavedSshEndpoint, Option<String>)> = Vec::new();
    for peer in &peers {
        let profile = if let Some(id) = &peer.profile_id {
            catalog
                .ssh
                .iter()
                .find(|profile| &profile.id == id)
                .cloned()
                .ok_or_else(|| {
                    io::Error::other(format!(
                        "legacy peer {} references a missing saved profile",
                        peer.alias
                    ))
                })?
        } else if let Some(profile) = catalog
            .ssh
            .iter()
            .find(|profile| profile.target == peer.target && profile.session == peer.session)
        {
            profile.clone()
        } else {
            let id = catalog
                .add_ssh(&peer.label, &peer.target, &peer.session)
                .map_err(io::Error::other)?;
            catalog
                .ssh
                .iter()
                .find(|profile| profile.id == id)
                .unwrap()
                .clone()
        };
        if profile.target != peer.target || profile.session != peer.session {
            return Err(io::Error::other(format!(
                "legacy peer {} does not match its saved profile",
                peer.alias
            )));
        }
        profiles.push((profile, peer.expected_id.clone()));
    }
    if let Some(profile) = selected {
        if !profiles
            .iter()
            .any(|(candidate, _)| candidate.id == profile.id)
        {
            profiles.push((profile, None));
        }
    }
    let mut pins = Vec::with_capacity(profiles.len());
    for (profile, expected_id) in &profiles {
        if !profile.enabled {
            return Err(io::Error::other(format!(
                "{} is disabled; run `herdr machine enable {}` first",
                profile.label, profile.id
            )));
        }
        let id = remote_machine_id(profile).map_err(io::Error::other)?;
        if expected_id
            .as_deref()
            .is_some_and(|expected| expected != id)
        {
            return Err(io::Error::other(format!(
                "legacy peer {} identity changed; refusing to replace its pin",
                profile.label
            )));
        }
        pins.push((profile.id.clone(), id));
    }
    let aliases: Vec<_> = peers.iter().map(|peer| peer.alias.clone()).collect();
    let updated = edit_policy(&original_config, &pins, None, &aliases).map_err(io::Error::other)?;
    if EndpointCatalog::load_profiles().map_err(io::Error::other)? != original_catalog.ssh {
        return Err(io::Error::other(
            "saved machine profiles changed while checking remote identities; retry federation command",
        ));
    }
    let catalog_changed = catalog.ssh != original_catalog.ssh;
    if catalog_changed {
        catalog.store_profiles().map_err(io::Error::other)?;
    }
    if updated != original_config {
        store_if_unchanged(&original_config, &updated).map_err(|error| {
            // A saved profile without a federation policy is untrusted. Rewriting
            // the old catalog here could erase another process's profile edits.
            io::Error::other(if catalog_changed {
                format!("{error}; saved profile remains untrusted")
            } else {
                error
            })
        })?;
    }
    println!("Federated {} machine(s), including {selector}.", pins.len());
    if updated == original_config {
        return Ok(0);
    }
    Ok(reload_live())
}

pub(super) fn unfederate(args: &[String]) -> io::Result<i32> {
    let [selector] = args else {
        eprintln!("usage: herdr machine unfederate <label-or-id>");
        return Ok(2);
    };
    let catalog = EndpointCatalog::load().map_err(io::Error::other)?;
    let Some(profile) = resolve_profile(&catalog, selector).map_err(io::Error::other)? else {
        eprintln!("saved machine {selector:?} was not found");
        return Ok(1);
    };
    let original = read_config().map_err(io::Error::other)?;
    let doc = original
        .parse::<DocumentMut>()
        .map_err(|error| io::Error::other(format!("invalid config TOML: {error}")))?;
    if doc
        .get("federation")
        .and_then(|federation| federation.get("saved_machines"))
        .and_then(|policies| policies.get(profile.id.as_str()))
        .is_none()
    {
        println!("Machine {} is already not federated.", profile.label);
        return Ok(0);
    }
    let updated = edit_policy(&original, &[], Some(&profile.id), &[]).map_err(io::Error::other)?;
    if updated == original {
        println!("Machine {} is already not federated.", profile.label);
        return Ok(0);
    }
    store_if_unchanged(&original, &updated).map_err(io::Error::other)?;
    println!(
        "Unfederated machine {} ({}); saved SSH profile remains available.",
        profile.label, profile.id
    );
    Ok(reload_live())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_id() -> ProfileId {
        ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap()
    }

    #[test]
    fn opt_in_migrates_legacy_ssh_without_losing_tcp_or_comments_and_opt_out_keeps_profile() {
        let original = r#"# Keep this user's header
[theme]
name = "midnight"

[federation]
coordinator = false
[[federation.peers]]
alias = "near"
endpoint = "tcp://127.0.0.1:7020"

[[federation.peers]]
alias = "old-ssh"
endpoint = "ssh://jerry@laptop"
"#;
        let legacy = legacy_peers(original).unwrap();
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].alias, "old-ssh");
        let id = profile_id();
        let federated = edit_policy(
            original,
            &[(id.clone(), "machine_verified".into())],
            None,
            &["old-ssh".into()],
        )
        .unwrap();
        let value: toml::Value = federated.parse().unwrap();
        assert!(federated.contains("# Keep this user's header"));
        assert_eq!(value["theme"]["name"].as_str(), Some("midnight"));
        assert_eq!(value["federation"]["coordinator"].as_bool(), Some(true));
        assert_eq!(value["federation"]["peers"].as_array().unwrap().len(), 1);
        assert_eq!(
            value["federation"]["peers"][0]["alias"].as_str(),
            Some("near")
        );
        assert_eq!(
            value["federation"]["saved_machines"][id.as_str()]["expected_machine_id"].as_str(),
            Some("machine_verified")
        );
        let unfederated = edit_policy(&federated, &[], Some(&id), &[]).unwrap();
        let value: toml::Value = unfederated.parse().unwrap();
        assert!(value["federation"].get("saved_machines").is_none());
        assert_eq!(value["federation"]["peers"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn migration_keeps_non_ssh_peer_with_same_alias() {
        let original = r#"[federation]
[[federation.peers]]
alias = "shared"
endpoint = "ssh://jerry@laptop"
[[federation.peers]]
alias = "shared"
endpoint = "tcp://127.0.0.1:7020"
"#;
        let migrated = edit_policy(
            original,
            &[(profile_id(), "machine_verified".into())],
            None,
            &["shared".into()],
        )
        .unwrap();
        let value: toml::Value = migrated.parse().unwrap();
        let peers = value["federation"]["peers"].as_array().unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0]["endpoint"].as_str(), Some("tcp://127.0.0.1:7020"));
    }

    #[test]
    fn migrating_multiple_legacy_peers_preserves_both_pins_in_one_update() {
        let source = r#"[federation]
[[federation.peers]]
alias = "first"
endpoint = "ssh://jerry@first"
[[federation.peers]]
alias = "second"
endpoint = "ssh://jerry@second"
"#;
        let first = profile_id();
        let second = ProfileId::parse("fedcba9876543210fedcba9876543210").unwrap();
        let migrated = edit_policy(
            source,
            &[
                (first.clone(), "machine_first".into()),
                (second.clone(), "machine_second".into()),
            ],
            None,
            &["first".into(), "second".into()],
        )
        .unwrap();
        assert!(legacy_peers(&migrated).unwrap().is_empty());
        let config: toml::Value = migrated.parse().unwrap();
        assert_eq!(
            config["federation"]["saved_machines"][first.as_str()]["expected_machine_id"].as_str(),
            Some("machine_first")
        );
        assert_eq!(
            config["federation"]["saved_machines"][second.as_str()]["expected_machine_id"].as_str(),
            Some("machine_second")
        );
    }

    #[test]
    fn empty_config_can_be_opted_in_and_out_without_a_federation_table() {
        let id = profile_id();
        let federated = edit_policy("", &[(id.clone(), "machine_new".into())], None, &[]).unwrap();
        let config: toml::Value = federated.parse().unwrap();
        assert_eq!(config["federation"]["coordinator"].as_bool(), Some(true));
        assert_eq!(
            config["federation"]["saved_machines"][id.as_str()]["expected_machine_id"].as_str(),
            Some("machine_new")
        );
        let unfederated = edit_policy(&federated, &[], Some(&id), &[]).unwrap();
        let config: toml::Value = unfederated.parse().unwrap();
        assert!(config["federation"].get("saved_machines").is_none());
    }

    #[test]
    fn unfederate_removes_inline_policy_without_touching_other_machines() {
        let id = profile_id();
        let other = "fedcba9876543210fedcba9876543210";
        let config = format!(
            "[federation]\ncoordinator = true\nsaved_machines = {{ \"{}\" = {{ expected_machine_id = \"machine_one\" }}, \"{other}\" = {{ expected_machine_id = \"machine_two\" }} }}\n",
            id.as_str()
        );
        let updated = edit_policy(&config, &[], Some(&id), &[]).unwrap();
        let parsed: toml::Value = updated.parse().unwrap();
        assert!(parsed["federation"]["saved_machines"]
            .get(id.as_str())
            .is_none());
        assert_eq!(
            parsed["federation"]["saved_machines"][other]["expected_machine_id"].as_str(),
            Some("machine_two")
        );
        assert_eq!(parsed["federation"]["coordinator"].as_bool(), Some(true));
        let other = ProfileId::parse(other.to_owned()).unwrap();
        let cleared = edit_policy(&updated, &[], Some(&other), &[]).unwrap();
        let parsed: toml::Value = cleared.parse().unwrap();
        assert!(parsed["federation"].get("saved_machines").is_none());
    }

    #[test]
    fn changed_identity_is_rejected_and_legacy_credentials_cannot_be_silently_migrated() {
        let id = profile_id();
        let config = format!(
            "[federation]\ncoordinator = true\n[federation.saved_machines.\"{}\"]\nexpected_machine_id = \"machine_original\"\n",
            id.as_str()
        );
        let error =
            edit_policy(&config, &[(id, "machine_replaced".into())], None, &[]).unwrap_err();
        assert!(error.contains("identity changed"), "{error}");

        let legacy = r#"[federation]
[[federation.peers]]
alias = "restricted"
endpoint = "ssh://jerry@laptop"
token_file = "/private/token"
"#;
        assert!(legacy_peers(legacy).unwrap_err().contains("unsupported"));
    }
}
