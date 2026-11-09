//! The panels that are about configuration and the world around the agents: skills, git, the event bus, and the
//! models (vendors and roles). Reads come from files and `git`; the only writes are to the global `config.yaml`.

use serde_yaml_ng::{Mapping, Value};
use shepherd_core::bus::{ConsumerGroupSummary, Message};
use shepherd_core::config::{self, Role, Vendor};
use shepherd_core::git::{collect_git_status, GitRepoStatus};
use shepherd_core::load_agent_registry;
use shepherd_core::patterns;
use shepherd_core::skills::{
    name_ok, new_skill, override_skill, repo_skills_dir, user_skills_dir, validate, Catalog, Scope,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SkillRow {
    pub name: String,
    /// Where it lives: `embedded:<name>` for the ones that ship with Shepherd, else its folder.
    pub dir: String,
    pub description: String,
    pub scope: String,
    pub global: bool,
    /// Passed validation.
    pub enabled: bool,
    pub problems: Vec<String>,
    pub body: String,
}

/// The merged skill catalog (`repo > user > default`); `global` also reflects `skills.global`.
pub fn skills(root: &Path) -> Result<Vec<SkillRow>, String> {
    let cfg = config::load_config(root).map_err(|e| e.to_string())?;
    let catalog = Catalog::load(Some(root), Some(&user_skills_dir()));
    let globals = catalog.global_names(&cfg);
    Ok(catalog
        .all()
        .into_iter()
        .map(|skill| SkillRow {
            global: globals.contains(&skill.name),
            enabled: skill.ok(),
            body: skill.body().to_string(),
            dir: skill.dir.clone(),
            scope: format!("{:?}", skill.scope),
            problems: skill.problems.clone(),
            description: skill.description.clone(),
            name: skill.name,
        })
        .collect())
}

/// Creates `<dir>/<name>/SKILL.md` from Shepherd's template, in the project (`repo`) or for the user, and returns
/// the file to edit.
pub fn create_skill(root: &Path, name: &str, repo: bool) -> Result<PathBuf, String> {
    let dir = if repo {
        repo_skills_dir(root)
    } else {
        user_skills_dir()
    };
    create_skill_in(&dir, name, root)
}

fn create_skill_in(dir: &Path, name: &str, root: &Path) -> Result<PathBuf, String> {
    if !name_ok(name) {
        return Err("a name is lowercase letters, digits and dashes".into());
    }
    // a name that is taken anywhere (a default, the user's or the project's) would shadow or be shadowed
    if Catalog::load(Some(root), Some(&user_skills_dir()))
        .get(name)
        .is_some()
    {
        return Err(format!("a skill called `{name}` already exists"));
    }
    new_skill(dir, name)
}

/// Makes a skill that ships with Shepherd editable by copying it to the user's skills folder; a skill of the user
/// or of the project is returned as it is. Returns the `SKILL.md` to edit.
pub fn customize_skill(root: &Path, name: &str) -> Result<PathBuf, String> {
    customize_skill_in(root, &user_skills_dir(), name)
}

fn customize_skill_in(root: &Path, user_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let catalog = Catalog::load(Some(root), Some(user_dir));
    override_skill(&catalog, name, user_dir)
}

/// The `SKILL.md` of a skill the user may edit (its own or the project's), not one that ships with Shepherd.
pub fn editable_skill_file(root: &Path, name: &str) -> Result<PathBuf, String> {
    editable_skill_file_in(root, &user_skills_dir(), name)
}

fn editable_skill_file_in(root: &Path, user_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let catalog = Catalog::load(Some(root), Some(user_dir));
    let skill = catalog.get(name).ok_or(format!("unknown skill `{name}`"))?;
    if skill.scope == Scope::Default {
        return Err(format!("`{name}` ships with Shepherd: customize it first"));
    }
    Ok(Path::new(&skill.dir).join("SKILL.md"))
}

/// Writes a `SKILL.md` and returns what is wrong with it (nothing, when it passes Shepherd's own validation).
pub fn save_skill(file: &Path, text: &str) -> Result<Vec<String>, String> {
    std::fs::write(file, text).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(validate(file))
}

/// Deletes the folder of a skill of the user or of the project, and everything in it. One that ships with
/// Shepherd cannot be deleted (customizing it and deleting the copy brings the original back).
pub fn delete_skill(root: &Path, name: &str) -> Result<(), String> {
    delete_skill_in(root, &user_skills_dir(), name)
}

fn delete_skill_in(root: &Path, user_dir: &Path, name: &str) -> Result<(), String> {
    let file = editable_skill_file_in(root, user_dir, name)?;
    let dir = file.parent().ok_or("the skill has no folder")?;
    let allowed = [user_dir.to_path_buf(), repo_skills_dir(root)];
    if !allowed.iter().any(|base| dir.starts_with(base)) {
        return Err(format!(
            "{} is not a skills folder of this user or project",
            dir.display()
        ));
    }
    std::fs::remove_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// Worktrees of the project with dirty/ahead/behind, the linked agent and PR (needs `git`; PRs need `gh`).
/// Can take seconds: call it off the UI thread.
pub fn git(root: &Path) -> Result<GitRepoStatus, String> {
    let registry = load_agent_registry(config::state_dir().join("agents.json")).unwrap_or_default();
    collect_git_status(root, &registry.agents)
}

#[derive(Debug, Clone)]
pub struct TopicRow {
    pub name: String,
    pub partitions: u32,
    pub messages_count: usize,
    pub paused: bool,
    pub groups: Vec<ConsumerGroupSummary>,
}

/// The topics of the session behind `api`, with their consumer groups. Asked of the server, which is the one
/// that knows where its bus is (every session keeps its own).
pub fn topics(api: &Path) -> Result<Vec<TopicRow>, String> {
    let list = crate::api::call(api, "shepherd.topic.list", serde_json::json!({}))?;
    let groups = crate::api::call(api, "shepherd.topic.groups", serde_json::json!({}))?;
    Ok(parse_topics(&list, &groups))
}

/// `shepherd.topic.list` + `shepherd.topic.groups` results into rows.
pub fn parse_topics(list: &serde_json::Value, groups: &serde_json::Value) -> Vec<TopicRow> {
    let groups: Vec<ConsumerGroupSummary> = groups["groups"]
        .as_array()
        .map(|g| {
            g.iter()
                .filter_map(|g| serde_json::from_value(g.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    list["topics"]
        .as_array()
        .map(|topics| {
            topics
                .iter()
                .filter_map(|t| {
                    let name = t["name"].as_str()?.to_owned();
                    Some(TopicRow {
                        paused: t["paused"].as_bool().unwrap_or(false),
                        groups: groups.iter().filter(|g| g.topic == name).cloned().collect(),
                        partitions: t["partitions"].as_u64().unwrap_or(1) as u32,
                        messages_count: t["messages_count"].as_u64().unwrap_or(0) as usize,
                        name,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The newest `limit` messages of a topic, oldest first.
pub fn topic_messages(api: &Path, topic: &str, limit: usize) -> Result<Vec<Message>, String> {
    let reply = crate::api::call(
        api,
        "shepherd.topic.replay",
        serde_json::json!({ "topic": topic }),
    )?;
    Ok(parse_messages(&reply, limit))
}

/// A `shepherd.topic.replay` result: the newest `limit` messages (1..=500), oldest first.
pub fn parse_messages(reply: &serde_json::Value, limit: usize) -> Vec<Message> {
    let mut messages: Vec<Message> = reply["messages"]
        .as_array()
        .map(|m| {
            m.iter()
                .filter_map(|m| serde_json::from_value(m.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    let skip = messages.len().saturating_sub(limit.clamp(1, 500));
    messages.drain(..skip);
    messages
}

#[derive(Debug, Clone, Default)]
pub struct Models {
    pub vendors: BTreeMap<String, Vendor>,
    pub roles: BTreeMap<String, Role>,
    /// File the forms write to (the global `config.yaml`).
    pub target: PathBuf,
}

pub fn models(root: &Path) -> Result<Models, String> {
    let cfg = config::load_config(root).map_err(|e| e.to_string())?;
    Ok(Models {
        vendors: cfg.vendors.into_iter().collect(),
        roles: cfg.roles.into_iter().collect(),
        target: config::global_config_path(),
    })
}

/// Sets (`Some`) or removes (`None`) `section.name` in a YAML mapping, leaving the other keys alone.
fn apply(map: &mut Mapping, section: &str, name: &str, value: Option<Value>) {
    let key = Value::from(section);
    if !matches!(map.get(&key), Some(Value::Mapping(_))) {
        map.insert(key.clone(), Value::Mapping(Mapping::new()));
    }
    if let Some(Value::Mapping(sec)) = map.get_mut(&key) {
        match value {
            Some(v) => sec.insert(Value::from(name), v),
            None => sec.remove(name),
        };
    }
}

/// Rewrites the global `config.yaml` with one entry changed; a `.bak` copy is kept. Re-serializing drops YAML comments.
fn update_yaml(section: &str, name: &str, value: Option<Value>) -> Result<PathBuf, String> {
    if !patterns::is_valid_ident(name) {
        return Err("invalid name (letters, numbers, - and _)".into());
    }
    let path = config::global_config_path();
    let mut map = match std::fs::read_to_string(&path) {
        Ok(t) if !t.trim().is_empty() => {
            match serde_yaml_ng::from_str(&t).map_err(|e| e.to_string())? {
                Value::Mapping(m) => m,
                _ => return Err("config.yaml is not a mapping".into()),
            }
        }
        _ => Mapping::new(),
    };
    apply(&mut map, section, name, value);
    let text = serde_yaml_ng::to_string(&map).map_err(|e| e.to_string())?;
    if path.is_file() {
        std::fs::copy(&path, path.with_extension("yaml.bak")).map_err(|e| e.to_string())?;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(path)
}

pub fn save_vendor(name: &str, vendor: &Vendor) -> Result<PathBuf, String> {
    update_yaml(
        "vendors",
        name,
        Some(serde_yaml_ng::to_value(vendor).map_err(|e| e.to_string())?),
    )
}

pub fn save_role(name: &str, role: &Role) -> Result<PathBuf, String> {
    update_yaml(
        "roles",
        name,
        Some(serde_yaml_ng::to_value(role).map_err(|e| e.to_string())?),
    )
}

pub fn delete_entry(section: &str, name: &str) -> Result<PathBuf, String> {
    match section {
        "vendors" | "roles" => update_yaml(section, name, None),
        _ => Err("only vendors and roles can be deleted".into()),
    }
}

#[cfg(test)]
mod tests {
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("shepherd-native-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_skill_is_created_from_the_template_then_saved_and_validated() {
        let dir = scratch("skills");
        let (root, user) = (dir.join("proj"), dir.join("user"));
        std::fs::create_dir_all(&root).unwrap();
        // names: the rule of the binary, and nothing that already exists
        assert!(create_skill_in(&user, "Bad Name", &root).is_err());
        assert!(
            create_skill_in(&user, "debug", &root).is_err(),
            "a default is taken"
        );
        let file = create_skill_in(&user, "my-skill", &root).unwrap();
        assert_eq!(file, user.join("my-skill/SKILL.md"));
        // the template still has placeholders, but it is a skill: saving good text reports no problems
        let good = "---\nname: my-skill\ndescription: Use when testing - it checks things.\n---\n\n# my-skill\n\n1. do it\n";
        assert!(save_skill(&file, good).unwrap().is_empty());
        assert!(std::fs::read_to_string(&file).unwrap().contains("do it"));
        // broken frontmatter is written (the text is the user's) and reported
        let problems = save_skill(&file, "no frontmatter at all").unwrap();
        assert!(!problems.is_empty(), "{problems:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_default_skill_is_customized_by_copying_it() {
        let dir = scratch("customize");
        let (root, user) = (dir.join("proj"), dir.join("user"));
        std::fs::create_dir_all(&root).unwrap();
        let file = customize_skill_in(&root, &user, "debug").unwrap();
        assert_eq!(file, user.join("debug/SKILL.md"));
        assert!(std::fs::read_to_string(&file)
            .unwrap()
            .contains("name: debug"));
        assert!(customize_skill_in(&root, &user, "nope").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn topics_and_messages_are_read_from_what_the_server_answers() {
        let list = serde_json::json!({"type":"shepherd_topic_list","topics":[
            {"name":"orders","partitions":2,"messages_count":3},
            {"name":"work","partitions":1,"messages_count":0,"paused":true}]});
        let groups = serde_json::json!({"type":"shepherd_consumer_groups","groups":[
            {"topic":"work","group":"specialist","members":["specialist-1"],"committed_offsets":{},
             "lag_by_partition":{"0":2}}]});
        let rows = parse_topics(&list, &groups);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (
                rows[0].name.as_str(),
                rows[0].partitions,
                rows[0].messages_count
            ),
            ("orders", 2, 3)
        );
        assert!(rows[0].groups.is_empty() && !rows[0].paused);
        assert!(rows[1].paused);
        assert_eq!(rows[1].groups[0].lag_by_partition.get(&0), Some(&2));
        assert!(parse_topics(&serde_json::json!({}), &serde_json::json!({})).is_empty());

        let reply = serde_json::json!({"messages":[
            {"id":1,"ts":"2026-11-09T15:13:16Z","from":"a","kind":"msg","content":"one","topic":"orders","partition":0,"offset":0},
            {"id":2,"ts":"2026-11-09T15:13:17Z","from":"b","kind":"msg","content":"two","topic":"orders","partition":1,"offset":0},
            {"id":3,"from":"c","kind":"msg","content":"three"}]});
        let last_two = parse_messages(&reply, 2);
        assert_eq!(
            last_two
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            ["two", "three"]
        );
    }

    use super::*;

    #[test]
    fn saving_one_entry_keeps_the_rest_and_a_backup() {
        let _env = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("native-models-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("shepherd")).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::fs::write(
            dir.join("shepherd/config.yaml"),
            "vendors:\n  old:\n    command: old-agent\nroles:\n  r1:\n    vendor: old\n",
        )
        .unwrap();

        let vendor = Vendor {
            command: "my-agent".into(),
            default_model: "m1".into(),
            ..Vendor::default()
        };
        let path = save_vendor("mine", &vendor).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("old-agent") && text.contains("my-agent") && text.contains("r1"),
            "other entries survive: {text}"
        );
        assert!(
            path.with_extension("yaml.bak").is_file(),
            "a backup is kept"
        );

        let role = Role {
            vendor: "mine".into(),
            worktree: true,
            ..Role::default()
        };
        save_role("worker", &role).unwrap();
        delete_entry("vendors", "old").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("old-agent") && text.contains("worker"),
            "{text}"
        );

        assert!(save_vendor("bad name!", &vendor).is_err());
        assert!(delete_entry("patterns", "x").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_the_users_and_the_projects_skills_can_be_deleted() {
        let dir = scratch("delete");
        let (root, user) = (dir.join("proj"), dir.join("user"));
        std::fs::create_dir_all(&root).unwrap();
        // a skill of the user, one of the project, and a default
        let mine = create_skill_in(&user, "mine", &root).unwrap();
        let theirs = create_skill_in(&repo_skills_dir(&root), "theirs", &root).unwrap();
        let refused = delete_skill_in(&root, &user, "debug").unwrap_err();
        assert!(refused.contains("ships with Shepherd"), "{refused}");
        assert!(delete_skill_in(&root, &user, "nope").is_err());

        delete_skill_in(&root, &user, "mine").unwrap();
        assert!(!mine.exists() && !mine.parent().unwrap().exists());
        assert!(theirs.exists(), "the other one is untouched");
        delete_skill_in(&root, &user, "theirs").unwrap();
        assert!(!theirs.exists());

        // customizing a default and deleting the copy brings the original back
        customize_skill_in(&root, &user, "debug").unwrap();
        delete_skill_in(&root, &user, "debug").unwrap();
        assert!(Catalog::load(Some(&root), Some(&user))
            .get("debug")
            .is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
