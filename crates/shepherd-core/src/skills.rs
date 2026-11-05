//! Skills catalog, per-role/per-session effective resolution, session installation, and management.
//!
//! Default skills are embedded directly from the repository `skills/` tree via
//! `include_str!` so Go and Rust share the exact same default skill definitions
//! without manual duplication.

use crate::config::{load_config_files, project_config_path, Config, Vendor};
pub use crate::SkillAdjust;
use serde::{Deserialize, Serialize};
use serde_yaml_ng::{Mapping, Value};
use std::{
    collections::{BTreeMap, HashMap},
    env, fs,
    path::{Path, PathBuf},
};

/// Origin scope of a skill (`Repo > User > Default`).
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Default,
    User,
    Repo,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Default => "default",
            Scope::User => "user",
            Scope::Repo => "repo",
        }
    }
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Resolution level at which a skill was activated for an agent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Global,
    Role,
    Session,
}

impl Level {
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Global => "global",
            Level::Role => "role",
            Level::Session => "session",
        }
    }
}

impl std::fmt::Display for Level {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A discovered skill in the catalog (`Repo > User > Default`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub scope: Scope,
    pub global: bool,
    pub dir: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
    #[serde(skip)]
    raw: String,
    #[serde(skip)]
    files: BTreeMap<String, Vec<u8>>,
}

impl Skill {
    /// Reports whether the skill's `SKILL.md` passed validation without problems.
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }

    /// Returns the full `SKILL.md` contents.
    pub fn body(&self) -> &str {
        &self.raw
    }

    /// Checks whether a relative file (e.g. `"checklist.md"`) exists in the skill folder.
    pub fn has_file(&self, rel: &str) -> bool {
        self.files.contains_key(rel)
    }
}

/// A skill selected for an agent together with the level that enabled it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Resolved {
    pub skill: Skill,
    pub level: Level,
}

struct EmbeddedSkill {
    name: &'static str,
    skill_md: &'static str,
    extra_files: &'static [(&'static str, &'static str)],
}

const EMBEDDED_SKILLS: &[EmbeddedSkill] = &[
    EmbeddedSkill {
        name: "code-review",
        skill_md: include_str!("../assets/skills/code-review/SKILL.md"),
        extra_files: &[(
            "checklist.md",
            include_str!("../assets/skills/code-review/checklist.md"),
        )],
    },
    EmbeddedSkill {
        name: "conventional-commits",
        skill_md: include_str!("../assets/skills/conventional-commits/SKILL.md"),
        extra_files: &[],
    },
    EmbeddedSkill {
        name: "debug",
        skill_md: include_str!("../assets/skills/debug/SKILL.md"),
        extra_files: &[],
    },
    EmbeddedSkill {
        name: "docs-lookup",
        skill_md: include_str!("../assets/skills/docs-lookup/SKILL.md"),
        extra_files: &[],
    },
    EmbeddedSkill {
        name: "implement-task",
        skill_md: include_str!("../assets/skills/implement-task/SKILL.md"),
        extra_files: &[],
    },
    EmbeddedSkill {
        name: "lang-go",
        skill_md: include_str!("../assets/skills/lang-go/SKILL.md"),
        extra_files: &[],
    },
    EmbeddedSkill {
        name: "lang-python",
        skill_md: include_str!("../assets/skills/lang-python/SKILL.md"),
        extra_files: &[],
    },
    EmbeddedSkill {
        name: "lang-ts",
        skill_md: include_str!("../assets/skills/lang-ts/SKILL.md"),
        extra_files: &[],
    },
    EmbeddedSkill {
        name: "plan-tasks",
        skill_md: include_str!("../assets/skills/plan-tasks/SKILL.md"),
        extra_files: &[],
    },
    EmbeddedSkill {
        name: "pr-workflow",
        skill_md: include_str!("../assets/skills/pr-workflow/SKILL.md"),
        extra_files: &[],
    },
    EmbeddedSkill {
        name: "security-review",
        skill_md: include_str!("../assets/skills/security-review/SKILL.md"),
        extra_files: &[],
    },
    EmbeddedSkill {
        name: "shepherd-comms",
        skill_md: include_str!("../assets/skills/shepherd-comms/SKILL.md"),
        extra_files: &[],
    },
    EmbeddedSkill {
        name: "shepherd-events",
        skill_md: include_str!("../assets/skills/shepherd-events/SKILL.md"),
        extra_files: &[],
    },
    EmbeddedSkill {
        name: "write-tests",
        skill_md: include_str!("../assets/skills/write-tests/SKILL.md"),
        extra_files: &[],
    },
];

/// Merged skill catalog (`Repo > User > Default`).
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    by_name: BTreeMap<String, Skill>,
}

impl Catalog {
    /// Merges three sources in increasing priority: embedded defaults, `user_dir`,
    /// and `<root>/.shepherd/skills` (when `root` is provided and non-empty).
    pub fn load(root: Option<&Path>, user_dir: Option<&Path>) -> Catalog {
        let mut cat = Catalog::default();
        cat.load_embedded();
        if let Some(u) = user_dir {
            if !u.as_os_str().is_empty() {
                cat.scan_dir(u, Scope::User);
            }
        }
        if let Some(r) = root {
            if !r.as_os_str().is_empty() {
                let d = repo_skills_dir(r);
                cat.scan_dir(&d, Scope::Repo);
            }
        }
        cat
    }

    fn load_embedded(&mut self) {
        for entry in EMBEDDED_SKILLS {
            let mut files = BTreeMap::new();
            files.insert("SKILL.md".to_owned(), entry.skill_md.as_bytes().to_vec());
            for (rel, content) in entry.extra_files {
                files.insert((*rel).to_owned(), content.as_bytes().to_vec());
            }
            let mut skill = parse_skill_md(entry.skill_md, entry.name, files);
            skill.scope = Scope::Default;
            skill.global = false;
            skill.dir = format!("embedded:{}", entry.name);
            self.by_name.insert(entry.name.to_owned(), skill);
        }
    }

    fn scan_dir(&mut self, root: &Path, scope: Scope) {
        for global in [false, true] {
            let base = if global {
                root.join("_global")
            } else {
                root.to_path_buf()
            };
            let Ok(entries) = fs::read_dir(&base) else {
                continue;
            };
            let mut dirs: Vec<_> = entries
                .filter_map(Result::ok)
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .collect();
            dirs.sort_by_key(|e| e.file_name());
            for entry in dirs {
                let dir_name = entry.file_name().to_string_lossy().into_owned();
                if dir_name.starts_with('_') {
                    continue;
                }
                let sub = entry.path();
                if !sub.join("SKILL.md").is_file() {
                    continue;
                }
                let Ok(files) = read_dir_files(&sub) else {
                    continue;
                };
                let raw = files
                    .get("SKILL.md")
                    .map(|b| String::from_utf8_lossy(b).into_owned())
                    .unwrap_or_default();
                let mut skill = parse_skill_md(&raw, &dir_name, files);
                skill.scope = scope;
                skill.global = global;
                skill.dir = sub.display().to_string();
                self.by_name.insert(dir_name, skill);
            }
        }
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.by_name.get(name)
    }

    /// Returns every skill sorted by name.
    pub fn all(&self) -> Vec<Skill> {
        self.by_name.values().cloned().collect()
    }

    /// Skills installed in every agent: `_global/` folders plus `config.skills.global`.
    pub fn global_names(&self, cfg: &Config) -> Vec<String> {
        let mut out = Vec::new();
        for s in self.by_name.values() {
            if s.global {
                out.push(s.name.clone());
            }
        }
        for n in &cfg.skills.global {
            if !out.contains(n) {
                out.push(n.clone());
            }
        }
        out
    }

    /// Returns the ordered skills an agent of `role_name` gets: globals first, then the role's
    /// list, minus disabled ones, with language auto-activation for code-implementing roles.
    pub fn effective(
        &self,
        cfg: &Config,
        role_name: &str,
        langs: &[String],
    ) -> (Vec<Skill>, Vec<String>) {
        let (resolved, problems) =
            self.effective_for(cfg, role_name, langs, &SkillAdjust::default());
        let skills = resolved.into_iter().map(|r| r.skill).collect();
        (skills, problems)
    }

    /// `effective` plus the per-session `SkillAdjust` layer of one agent: skills added for the
    /// session replace a same-named skill of any level, and skills removed for the session are
    /// dropped whatever level they came from.
    pub fn effective_for(
        &self,
        cfg: &Config,
        role_name: &str,
        langs: &[String],
        adj: &SkillAdjust,
    ) -> (Vec<Resolved>, Vec<String>) {
        let Some(role) = cfg.roles.get(role_name) else {
            return (Vec::new(), vec![format!("unknown role {role_name:?}")]);
        };

        struct Entry {
            name: String,
            level: Level,
        }

        let mut names = Vec::new();
        for n in self.global_names(cfg) {
            names.push(Entry {
                name: n,
                level: Level::Global,
            });
        }
        let has_lang = role
            .skills
            .iter()
            .any(|n| n.starts_with("lang-") || n == "implement-task");
        for n in &role.skills {
            if let Some(lang) = n.strip_prefix("lang-") {
                if !langs.iter().any(|l| l == lang) {
                    continue;
                }
            }
            names.push(Entry {
                name: n.clone(),
                level: Level::Role,
            });
        }
        if has_lang {
            for l in langs {
                names.push(Entry {
                    name: format!("lang-{l}"),
                    level: Level::Role,
                });
            }
        }
        for n in &adj.add {
            names.push(Entry {
                name: n.clone(),
                level: Level::Session,
            });
        }

        // One entry per name: the session add if there is one (it replaces the role's), else the first mention.
        let mut keep: HashMap<String, usize> = HashMap::new();
        for (i, e) in names.iter().enumerate() {
            match keep.get(&e.name) {
                None => {
                    keep.insert(e.name.clone(), i);
                }
                Some(&j) if e.level == Level::Session && names[j].level != Level::Session => {
                    keep.insert(e.name.clone(), i);
                }
                _ => {}
            }
        }

        let mut out = Vec::new();
        let mut problems = Vec::new();
        for (i, e) in names.iter().enumerate() {
            if keep.get(&e.name) != Some(&i)
                || cfg.skills.disabled.contains(&e.name)
                || adj.remove.contains(&e.name)
            {
                continue;
            }
            match self.get(&e.name) {
                None => {
                    if e.level == Level::Session {
                        problems.push(format!("session: skill {:?} not found", e.name));
                    } else if !e.name.starts_with("lang-") {
                        problems.push(format!("role {role_name:?}: skill {:?} not found", e.name));
                    }
                }
                Some(s) if !s.ok() => {
                    problems.push(format!(
                        "skill {:?} is broken: {}",
                        e.name,
                        s.problems.join("; ")
                    ));
                }
                Some(s) => {
                    out.push(Resolved {
                        skill: s.clone(),
                        level: e.level,
                    });
                }
            }
        }
        (out, problems)
    }
}

#[derive(Default, Deserialize)]
struct Frontmatter {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
}

fn parse_skill_md(raw: &str, dir_name: &str, files: BTreeMap<String, Vec<u8>>) -> Skill {
    let mut s = Skill {
        name: dir_name.to_owned(),
        description: String::new(),
        scope: Scope::Default,
        global: false,
        dir: String::new(),
        problems: Vec::new(),
        raw: raw.to_owned(),
        files,
    };
    let txt = raw.replace("\r\n", "\n");
    if !txt.starts_with("---\n") {
        s.problems.push("SKILL.md has no frontmatter".into());
        return s;
    }
    let Some(end) = txt[4..].find("\n---") else {
        s.problems.push("frontmatter is not closed".into());
        return s;
    };
    let fm: Frontmatter = match serde_yaml_ng::from_str(&txt[4..4 + end]) {
        Ok(fm) => fm,
        Err(err) => {
            s.problems.push(format!("frontmatter: {err}"));
            return s;
        }
    };
    s.description = fm.description.trim().to_owned();
    if fm.name.is_empty() {
        s.problems.push("frontmatter has no name".into());
    } else if fm.name != dir_name {
        s.problems.push(format!(
            "name {:?} does not match folder {:?}",
            fm.name, dir_name
        ));
    } else if !name_ok(&fm.name) {
        s.problems
            .push("name must be lowercase letters, digits and dashes".into());
    }
    if s.description.is_empty() {
        s.problems.push("frontmatter has no description".into());
    }
    s
}

/// Validates a skill folder found outside the catalog (e.g. for `skills add`).
/// Returns the skill named after its frontmatter so a cloned repo's folder name does not matter.
pub fn inspect(dir: &Path) -> Skill {
    let files = read_dir_files(dir).unwrap_or_default();
    let raw = files
        .get("SKILL.md")
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default();
    let mut name = String::new();
    let txt = raw.replace("\r\n", "\n");
    if let Some(rest) = txt.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            if let Ok(fm) = serde_yaml_ng::from_str::<Frontmatter>(&rest[..end]) {
                name = fm.name;
            }
        }
    }
    let mut skill = parse_skill_md(&raw, &name, files);
    skill.dir = dir.display().to_string();
    skill
}

/// Reports whether an installed copy of a skill no longer matches the catalog's `SKILL.md`
/// (or lost it).
pub fn edited(installed_dir: &Path, skill: &Skill) -> bool {
    match fs::read_to_string(installed_dir.join("SKILL.md")) {
        Ok(content) => content != skill.body(),
        Err(_) => true,
    }
}

/// Checks whether `n` is a valid skill name (`^[a-z0-9][a-z0-9-]*$`).
pub fn name_ok(n: &str) -> bool {
    let mut chars = n.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Returns the string list a toggle key refers to in a loaded config
/// (`"skills.global"`, `"skills.disabled"`, `"roles.<role>.skills"`).
pub fn list_in(c: &Config, key: &str) -> Option<Vec<String>> {
    match key {
        "skills.global" => Some(c.skills.global.clone()),
        "skills.disabled" => Some(c.skills.disabled.clone()),
        _ => {
            let role_name = key.strip_prefix("roles.")?.strip_suffix(".skills")?;
            c.roles.get(role_name).map(|r| r.skills.clone())
        }
    }
}

/// Adds (`on == true`) or removes (`on == false`) `name` in the string list at `key` of the
/// config file at `cfg_path` (`"skills.global"`, `"skills.disabled"`, `"roles.<role>.skills"`).
///
/// `current` is the list to start from. It is idempotent. `dir` is the repo directory the merged
/// config is loaded for. If the merged config does not end up with `name` present (`on`) or
/// absent (`!on`) because another layer overrides or shows through, `cfg_path` is restored and
/// an error is returned.
pub fn toggle(
    cfg_path: &Path,
    key: &str,
    name: &str,
    on: bool,
    current: &[String],
    dir: &Path,
) -> Result<(), String> {
    let mut list: Vec<String> = current.to_vec();
    if on {
        if !list.iter().any(|x| x == name) {
            list.push(name.to_owned());
        }
    } else {
        list.retain(|x| x != name);
    }

    let old = fs::read(cfg_path).ok();
    let write_res = if list.is_empty() {
        unset_yaml_key(cfg_path, key)
    } else {
        set_yaml_string_list(cfg_path, key, &list)
    };
    write_res?;

    let global_path = crate::config::global_config_path();
    let project_path = project_config_path(dir);
    let paths = if cfg_path != global_path && cfg_path != project_path {
        vec![cfg_path.to_path_buf(), project_path]
    } else {
        vec![global_path, project_path]
    };

    if let Ok(merged) = load_config_files(&paths) {
        let got = list_in(&merged, key).unwrap_or_default();
        if got.iter().any(|x| x == name) != on {
            match old {
                Some(bytes) => {
                    let _ = fs::write(cfg_path, bytes);
                }
                None => {
                    let _ = fs::remove_file(cfg_path);
                }
            }
            let state = if on { "still lacks" } else { "still has" };
            return Err(format!(
                "no effect: {key} {state} {name:?} because another layer (the built-in defaults, your user config or the project config) decides it; {} was left as it was",
                cfg_path.display()
            ));
        }
    }
    Ok(())
}

fn set_yaml_string_list(cfg_path: &Path, key: &str, list: &[String]) -> Result<(), String> {
    let mut root = read_yaml_mapping(cfg_path)?;
    let parts: Vec<&str> = key.split('.').collect();
    if parts.is_empty() {
        return Err("empty config key".into());
    }
    set_in_mapping(
        &mut root,
        &parts,
        Value::Sequence(list.iter().cloned().map(Value::String).collect()),
    );
    write_yaml_mapping(cfg_path, &root)
}

fn unset_yaml_key(cfg_path: &Path, key: &str) -> Result<(), String> {
    if !cfg_path.exists() {
        return Ok(());
    }
    let mut root = read_yaml_mapping(cfg_path)?;
    let parts: Vec<&str> = key.split('.').collect();
    unset_in_mapping(&mut root, &parts);
    write_yaml_mapping(cfg_path, &root)
}

fn read_yaml_mapping(cfg_path: &Path) -> Result<Mapping, String> {
    match fs::read(cfg_path) {
        Ok(bytes) if !bytes.is_empty() => {
            let val: Value = serde_yaml_ng::from_slice(&bytes)
                .map_err(|e| format!("{}: {e}", cfg_path.display()))?;
            match val {
                Value::Mapping(m) => Ok(m),
                Value::Null => Ok(Mapping::new()),
                _ => Err(format!(
                    "{}: root must be a YAML mapping",
                    cfg_path.display()
                )),
            }
        }
        Ok(_) => Ok(Mapping::new()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Mapping::new()),
        Err(e) => Err(e.to_string()),
    }
}

fn write_yaml_mapping(cfg_path: &Path, root: &Mapping) -> Result<(), String> {
    if let Some(parent) = cfg_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let out = serde_yaml_ng::to_string(&Value::Mapping(root.clone())).map_err(|e| e.to_string())?;
    fs::write(cfg_path, out).map_err(|e| e.to_string())
}

fn set_in_mapping(map: &mut Mapping, parts: &[&str], value: Value) {
    let key = Value::String(parts[0].to_owned());
    if parts.len() == 1 {
        map.insert(key, value);
        return;
    }
    if !matches!(map.get(&key), Some(Value::Mapping(_))) {
        map.insert(key.clone(), Value::Mapping(Mapping::new()));
    }
    if let Some(Value::Mapping(child)) = map.get_mut(&key) {
        set_in_mapping(child, &parts[1..], value);
    }
}

fn unset_in_mapping(map: &mut Mapping, parts: &[&str]) {
    if parts.is_empty() {
        return;
    }
    let key = Value::String(parts[0].to_owned());
    if parts.len() == 1 {
        map.remove(&key);
        return;
    }
    if let Some(Value::Mapping(child)) = map.get_mut(&key) {
        unset_in_mapping(child, &parts[1..]);
    }
}

/// Creates `<dir>/<name>/SKILL.md` from a template and returns its path.
pub fn new_skill(dir: &Path, name: &str) -> Result<PathBuf, String> {
    if !name_ok(name) {
        return Err("skill names are lowercase letters, digits and dashes".into());
    }
    let d = dir.join(name);
    if d.exists() {
        return Err(format!("{} already exists", d.display()));
    }
    let tpl = format!(
        "---\nname: {name}\ndescription: Use when <situation that should trigger this skill> - <what it makes the agent do>.\n---\n\n# {name}\n\n1. <step>\n2. <step>\n"
    );
    fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    let p = d.join("SKILL.md");
    fs::write(&p, tpl).map_err(|e| e.to_string())?;
    Ok(p)
}

/// Returns the validation problems of one `SKILL.md` on disk (the parent folder name is the skill name).
pub fn validate(skill_md: &Path) -> Vec<String> {
    if !skill_md.is_file() {
        return vec!["SKILL.md is missing".into()];
    }
    let Some(dir) = skill_md.parent() else {
        return vec!["SKILL.md has no parent directory".into()];
    };
    let dir_name = dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let files = read_dir_files(dir).unwrap_or_default();
    let raw = files
        .get("SKILL.md")
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default();
    parse_skill_md(&raw, &dir_name, files).problems
}

/// Makes a skill editable in `user_dir`: an embedded default is copied into `user_dir`
/// (keeping `_global/` for global ones); a user or repo skill is returned as-is.
/// Returns the `SKILL.md` path to edit.
pub fn override_skill(cat: &Catalog, name: &str, user_dir: &Path) -> Result<PathBuf, String> {
    let Some(s) = cat.get(name) else {
        return Err(format!("unknown skill {name:?}"));
    };
    if s.scope != Scope::Default {
        return Ok(Path::new(&s.dir).join("SKILL.md"));
    }
    let dst = if s.global {
        user_dir.join("_global").join(name)
    } else {
        user_dir.join(name)
    };
    copy_skill(s, &dst)?;
    Ok(dst.join("SKILL.md"))
}

/// Describes one agent session for skill installation.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub vendor: Vendor,
    pub session_dir: PathBuf,
    pub workdir: PathBuf,
    pub skills: Vec<Skill>,
    pub workspace: String,
    pub repo_docs: Vec<String>,
}

/// What the spawner adds to the vendor command line and initial prompt after staging session skills.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Launch {
    pub args: Vec<String>,
    pub prompt: String,
    pub files: Vec<String>,
    pub notes: Vec<String>,
}

/// Stages the skills for one session and returns how to hand them to the vendor:
/// `claude --plugin-dir`, `codex -c developer_instructions=...`, a worktree file,
/// or the initial prompt. Unknown combinations degrade to the prompt with a note.
pub fn install(plan: Plan) -> Result<Launch, String> {
    let mut launch = Launch::default();
    fs::create_dir_all(&plan.session_dir).map_err(|e| e.to_string())?;

    let sk = &plan.vendor.skills;
    if sk.mode == "native" && sk.style == "claude-plugin" {
        let plugin = plan.session_dir.join("plugin");
        write_plugin(&plugin, &plan.skills)?;
        launch.args = vec!["--plugin-dir".into(), plugin.display().to_string()];
        let txt = header(&plan);
        if !txt.is_empty() {
            launch.args.push("--append-system-prompt".into());
            launch.args.push(txt);
        }
        let instructions = compose(&plan, &plugin.join("skills"));
        write_file(&plan.session_dir.join("INSTRUCTIONS.md"), &instructions)?;
        return Ok(launch);
    }

    let dir = plan.session_dir.join("skills");
    for s in &plan.skills {
        copy_skill(s, &dir.join(&s.name))?;
    }
    let text = compose(&plan, &dir);
    write_file(&plan.session_dir.join("INSTRUCTIONS.md"), &text)?;

    match (sk.mode.as_str(), sk.style.as_str()) {
        ("instructions", "codex-developer") => {
            let b = serde_json::to_string(&text).map_err(|e| e.to_string())?;
            launch.args = vec!["-c".into(), format!("developer_instructions={b}")];
        }
        ("instructions", "worktree-file") => {
            let name = if sk.file.is_empty() {
                "AGENTS.md"
            } else {
                &sk.file
            };
            let target = plan.workdir.join(name);
            if target.exists() {
                launch.notes.push(format!(
                    "{name} already exists in the repo; not overwritten, using the initial prompt"
                ));
                launch.prompt = text;
            } else {
                write_file(&target, &text)?;
                launch.files = vec![target.display().to_string()];
            }
        }
        _ => {
            if sk.mode != "prompt" {
                launch.notes.push(format!(
                    "skills mode {:?} style {:?} has no verified mechanism; using the initial prompt",
                    sk.mode, sk.style
                ));
            }
            launch.prompt = text;
        }
    }
    Ok(launch)
}

fn header(plan: &Plan) -> String {
    let mut b = String::new();
    if !plan.workspace.is_empty() {
        b.push_str("# Workspace instructions (apply to every agent here)\n\n");
        b.push_str(&plan.workspace);
        b.push_str("\n\n");
    }
    if !plan.repo_docs.is_empty() {
        b.push_str("Also read and follow the repository's own instruction files: ");
        b.push_str(&plan.repo_docs.join(", "));
        b.push_str(".\n");
    }
    b.trim().to_owned()
}

/// Renders instructions text: `workspace.md` first, repo docs, then the skill index with real paths.
pub fn compose(plan: &Plan, skills_dir: &Path) -> String {
    let mut b = String::new();
    let h = header(plan);
    if !h.is_empty() {
        b.push_str(&h);
        b.push_str("\n\n");
    }
    if !plan.skills.is_empty() {
        b.push_str("# Skills\n\nBefore starting a task, read the SKILL.md of every skill below that matches what you are doing, and follow it.\n\n");
        for s in &plan.skills {
            let path = skills_dir.join(&s.name).join("SKILL.md");
            b.push_str(&format!(
                "- **{}** ({}): {}\n",
                s.name,
                path.display(),
                s.description
            ));
        }
    }
    format!("{}\n", b.trim())
}

fn write_plugin(dir: &Path, skills: &[Skill]) -> Result<(), String> {
    let manifest = serde_json::json!({
        "name": "shepherd-session",
        "version": "0.0.0",
        "description": "Skills selected by shepherd for this agent session",
        "author": { "name": "shepherd" }
    });
    let text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    write_file(&dir.join(".claude-plugin/plugin.json"), &text)?;
    for s in skills {
        copy_skill(s, &dir.join("skills").join(&s.name))?;
    }
    Ok(())
}

fn write_file(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(path, content).map_err(|e| e.to_string())
}

/// Copies a skill folder to `dst` (replacing it if present).
pub fn copy_skill(skill: &Skill, dst: &Path) -> Result<(), String> {
    if dst.exists() {
        fs::remove_dir_all(dst).map_err(|e| e.to_string())?;
    }
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for (rel, bytes) in &skill.files {
        let target = dst.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&target, bytes).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn read_dir_files(root: &Path) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut out = BTreeMap::new();
    read_dir_files_rec(root, root, &mut out)?;
    Ok(out)
}

fn read_dir_files_rec(
    base: &Path,
    current: &Path,
    out: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    for entry in fs::read_dir(current).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_dir() {
            read_dir_files_rec(base, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(base)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            let data = fs::read(&path).map_err(|e| e.to_string())?;
            out.insert(rel, data);
        }
    }
    Ok(())
}

/// User-level skills directory (`$XDG_CONFIG_HOME/shepherd/skills` or `~/.config/shepherd/skills`).
pub fn user_skills_dir() -> PathBuf {
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        if !xdg.trim().is_empty() {
            return PathBuf::from(xdg).join("shepherd/skills");
        }
    }
    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".config/shepherd/skills")
}

/// Project-level skills directory (`<root>/.shepherd/skills`).
pub fn repo_skills_dir(root: &Path) -> PathBuf {
    root.join(".shepherd/skills")
}

/// Reads `<root>/.shepherd/workspace.md` if present (matching `workspace.Instructions(root)` in Go).
pub fn workspace_instructions(root: &Path) -> String {
    fs::read_to_string(root.join(".shepherd/workspace.md"))
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

/// Returns instruction files already in the repo (`AGENTS.md`, `CLAUDE.md`, `GEMINI.md`,
/// `.github/copilot-instructions.md`), referenced without editing.
pub fn repo_instruction_docs(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for f in [
        "AGENTS.md",
        "CLAUDE.md",
        "GEMINI.md",
        ".github/copilot-instructions.md",
    ] {
        if root.join(f).is_file() {
            out.push(f.to_owned());
        }
    }
    out
}

/// Auto-detects default verification commands and language short names (`"go"`, `"rust"`,
/// `"ts"`, `"python"`) from repository markers, matching `config.DetectChecks` in Go.
pub fn detect_checks(root: &Path) -> (Vec<String>, Vec<String>) {
    let mut checks = Vec::new();
    let mut langs = Vec::new();
    let has = |rel: &str| root.join(rel).exists();
    if has("go.mod") {
        checks.push("go test ./...".into());
        checks.push("go vet ./...".into());
        langs.push("go".into());
    }
    if has("Cargo.toml") {
        checks.push("cargo test".into());
        langs.push("rust".into());
    }
    if has("package.json") {
        checks.push("npm test".into());
        langs.push("ts".into());
    }
    if has("pyproject.toml") || has("pytest.ini") {
        checks.push("pytest".into());
        langs.push("python".into());
    }
    (checks, langs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "shepherd-skills-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_skill(dir: &Path, name: &str, desc: &str) {
        let p = dir.join(name);
        fs::create_dir_all(&p).unwrap();
        fs::write(
            p.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {desc}\n---\nbody of {desc}\n"),
        )
        .unwrap();
    }

    #[test]
    fn embedded_skills_match_repository_skills_directory_without_drift() {
        let repo_skills = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/skills");
        let mut disk_dirs: Vec<String> = fs::read_dir(&repo_skills)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        disk_dirs.sort();

        let embedded_names: Vec<String> =
            EMBEDDED_SKILLS.iter().map(|s| s.name.to_owned()).collect();
        assert_eq!(
            embedded_names, disk_dirs,
            "embedded skill list must match skills/ directories on disk"
        );

        for entry in EMBEDDED_SKILLS {
            let disk_md =
                fs::read_to_string(repo_skills.join(entry.name).join("SKILL.md")).unwrap();
            assert_eq!(
                entry.skill_md, disk_md,
                "embedded SKILL.md for {} drifted from disk",
                entry.name
            );
            for (rel, content) in entry.extra_files {
                let disk_extra =
                    fs::read_to_string(repo_skills.join(entry.name).join(rel)).unwrap();
                assert_eq!(
                    *content, disk_extra,
                    "embedded extra file {rel} for {} drifted from disk",
                    entry.name
                );
            }
        }
    }

    #[test]
    fn default_skills_are_valid_and_referenced_by_default_roles() {
        let user = temp_dir("valid-user");
        let c = Catalog::load(None, Some(&user));
        for s in c.all() {
            assert!(s.ok(), "{}: {:?}", s.name, s.problems);
            assert!(
                s.body().len() >= 400,
                "{} looks like an empty template",
                s.name
            );
        }
        let defaults = Config::default();
        for (role_name, role) in &defaults.roles {
            for n in &role.skills {
                assert!(
                    c.get(n).is_some(),
                    "role {role_name} references missing skill {n}"
                );
            }
        }
        let cr = c.get("code-review").unwrap();
        assert!(
            cr.has_file("checklist.md"),
            "code-review/checklist.md missing"
        );
        fs::remove_dir_all(user).unwrap();
    }

    #[test]
    fn priority_repo_over_user_over_default() {
        let root = temp_dir("prio-root");
        let user = temp_dir("prio-user");
        write_skill(&user, "debug", "user debug");
        write_skill(&user, "only-user", "u");
        write_skill(&repo_skills_dir(&root), "debug", "repo debug");

        let c = Catalog::load(Some(&root), Some(&user));
        let debug = c.get("debug").unwrap();
        assert_eq!(debug.scope, Scope::Repo);
        assert_eq!(debug.description, "repo debug");

        let only_user = c.get("only-user").unwrap();
        assert_eq!(only_user.scope, Scope::User);

        let cr = c.get("code-review").unwrap();
        assert_eq!(cr.scope, Scope::Default);

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(user).unwrap();
    }

    #[test]
    fn globals_and_broken_skills() {
        let root = temp_dir("glob-root");
        let user = temp_dir("glob-user");
        let repo = repo_skills_dir(&root);
        write_skill(&repo.join("_global"), "api-conventions", "api rules");
        fs::create_dir_all(repo.join("bad")).unwrap();
        fs::write(repo.join("bad/SKILL.md"), "no frontmatter").unwrap();
        write_skill(&repo, "wrong-name", "x");
        fs::write(
            repo.join("wrong-name/SKILL.md"),
            "---\nname: other\ndescription: d\n---\n",
        )
        .unwrap();

        let c = Catalog::load(Some(&root), Some(&user));
        let mut cfg = Config::default();
        cfg.roles
            .get_mut("docs")
            .unwrap()
            .skills
            .extend(["bad".into(), "ghost".into()]);
        let (got, problems) = c.effective(&cfg, "docs", &[]);
        assert_eq!(
            got.first().map(|s| s.name.as_str()),
            Some("api-conventions")
        );
        let joined = problems.join("|");
        assert!(
            joined.contains("bad") && joined.contains("ghost"),
            "{problems:?}"
        );
        assert!(!c.get("wrong-name").unwrap().ok());

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(user).unwrap();
    }

    #[test]
    fn language_auto_activation() {
        let user = temp_dir("lang-user");
        let c = Catalog::load(None, Some(&user));
        let mut cfg = Config::default();
        let has = |ss: &[Skill], n: &str| ss.iter().any(|s| s.name == n);

        let (ts, _) = c.effective(&cfg, "coder", &["ts".into()]);
        assert!(!has(&ts, "lang-go") && has(&ts, "lang-ts"));

        let (none, p) = c.effective(&cfg, "coder", &[]);
        assert!(!has(&none, "lang-go") && p.is_empty());

        let (rev, _) = c.effective(&cfg, "reviewer", &["go".into()]);
        assert!(!has(&rev, "lang-go"));

        cfg.skills.disabled = vec!["conventional-commits".into()];
        let (co, _) = c.effective(&cfg, "coder", &["go".into()]);
        assert!(!has(&co, "conventional-commits") && has(&co, "lang-go"));

        fs::remove_dir_all(user).unwrap();
    }

    #[test]
    fn effective_for_levels_and_session_adjustments() {
        let user = temp_dir("sess-user");
        write_skill(&user.join("_global"), "g1", "global one");
        write_skill(&user, "r1", "role one");
        write_skill(&user, "s1", "session one");
        let mut cfg = Config::default();
        cfg.roles.get_mut("coder").unwrap().skills = vec!["r1".into()];
        let c = Catalog::load(None, Some(&user));

        let by_name = |rs: &[Resolved]| -> HashMap<String, Level> {
            rs.iter().map(|r| (r.skill.name.clone(), r.level)).collect()
        };

        let (rs, problems) = c.effective_for(
            &cfg,
            "coder",
            &[],
            &SkillAdjust {
                add: vec!["s1".into()],
                remove: Vec::new(),
            },
        );
        assert!(problems.is_empty());
        let got = by_name(&rs);
        assert_eq!(got.get("g1"), Some(&Level::Global));
        assert_eq!(got.get("r1"), Some(&Level::Role));
        assert_eq!(got.get("s1"), Some(&Level::Session));

        // Session add replaces same name with Level::Session once
        let (rs, _) = c.effective_for(
            &cfg,
            "coder",
            &[],
            &SkillAdjust {
                add: vec!["r1".into()],
                remove: Vec::new(),
            },
        );
        let r1_entries: Vec<_> = rs.iter().filter(|r| r.skill.name == "r1").collect();
        assert_eq!(r1_entries.len(), 1);
        assert_eq!(r1_entries[0].level, Level::Session);

        // Session remove drops global and role skills
        let (rs, _) = c.effective_for(
            &cfg,
            "coder",
            &[],
            &SkillAdjust {
                add: Vec::new(),
                remove: vec!["g1".into(), "r1".into()],
            },
        );
        let got = by_name(&rs);
        assert!(!got.contains_key("g1"));
        assert!(!got.contains_key("r1"));

        // Unknown session add is reported as problem
        let (rs, problems) = c.effective_for(
            &cfg,
            "coder",
            &[],
            &SkillAdjust {
                add: vec!["nope".into()],
                remove: Vec::new(),
            },
        );
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("nope"));
        assert!(!by_name(&rs).contains_key("nope"));

        // Edited detection
        let s1 = c.get("s1").unwrap();
        let copy_dir = temp_dir("sess-copy");
        copy_skill(s1, &copy_dir).unwrap();
        assert!(!edited(&copy_dir, s1));
        fs::write(
            copy_dir.join("SKILL.md"),
            "---\nname: s1\ndescription: x\n---\nchanged\n",
        )
        .unwrap();
        assert!(edited(&copy_dir, s1));
        let empty_dir = temp_dir("sess-empty");
        assert!(edited(&empty_dir, s1));

        fs::remove_dir_all(user).unwrap();
        fs::remove_dir_all(copy_dir).unwrap();
        fs::remove_dir_all(empty_dir).unwrap();
    }

    fn make_plan(mode: &str, style: &str) -> (Plan, PathBuf, PathBuf) {
        let user = temp_dir("plan-user");
        let c = Catalog::load(None, Some(&user));
        let (sk, _) = c.effective(&Config::default(), "reviewer", &[]);
        fs::remove_dir_all(user).unwrap();
        let mut v = Vendor {
            command: "x".into(),
            ..Vendor::default()
        };
        v.skills.mode = mode.into();
        v.skills.style = style.into();
        let sess_parent = temp_dir("plan-sess");
        let work = temp_dir("plan-work");
        let plan = Plan {
            vendor: v,
            session_dir: sess_parent.join("sess"),
            workdir: work.clone(),
            skills: sk,
            workspace: "NEVER TOUCH prod".into(),
            repo_docs: vec!["AGENTS.md".into()],
        };
        (plan, sess_parent, work)
    }

    #[test]
    fn install_supports_claude_plugin_codex_developer_worktree_file_and_prompt() {
        // 1. Native claude-plugin
        let (p, sess_parent, work) = make_plan("native", "claude-plugin");
        let l = install(p).unwrap();
        assert_eq!(l.args[0], "--plugin-dir");
        assert!(l.args.join(" ").contains("NEVER TOUCH prod"));
        let plugin_dir = PathBuf::from(&l.args[1]);
        for f in [
            ".claude-plugin/plugin.json",
            "skills/code-review/SKILL.md",
            "skills/code-review/checklist.md",
        ] {
            assert!(plugin_dir.join(f).exists(), "missing {f}");
        }
        assert_eq!(fs::read_dir(&work).unwrap().count(), 0);
        fs::remove_dir_all(sess_parent).unwrap();
        fs::remove_dir_all(work).unwrap();

        // 2. Codex developer_instructions
        let (p, sess_parent, work) = make_plan("instructions", "codex-developer");
        let l = install(p).unwrap();
        assert_eq!(l.args[0], "-c");
        assert!(l.args[1].starts_with("developer_instructions=\""));
        let body = &l.args[1];
        assert!(body.find("NEVER TOUCH").unwrap() < body.find("code-review").unwrap());
        assert!(body.contains("AGENTS.md") && body.contains("SKILL.md"));
        assert_eq!(fs::read_dir(&work).unwrap().count(), 0);
        fs::remove_dir_all(sess_parent).unwrap();
        fs::remove_dir_all(work).unwrap();

        // 3. Worktree file never overwrites existing repo file
        let (p, sess_parent, work) = make_plan("instructions", "worktree-file");
        let l = install(p).unwrap();
        assert_eq!(l.files.len(), 1);
        assert!(l.files[0].ends_with("AGENTS.md"));
        fs::remove_dir_all(sess_parent).unwrap();
        fs::remove_dir_all(work).unwrap();

        let (p2, sess_parent2, work2) = make_plan("instructions", "worktree-file");
        fs::write(work2.join("AGENTS.md"), "OURS").unwrap();
        let l2 = install(p2).unwrap();
        assert_eq!(fs::read_to_string(work2.join("AGENTS.md")).unwrap(), "OURS");
        assert!(l2.files.is_empty());
        assert!(!l2.prompt.is_empty());
        assert!(!l2.notes.is_empty());
        fs::remove_dir_all(sess_parent2).unwrap();
        fs::remove_dir_all(work2).unwrap();

        // 4. Prompt & degradation
        for (mode, style) in [("prompt", ""), ("native", "unknown"), ("instructions", "")] {
            let (p, sess_parent, work) = make_plan(mode, style);
            let l = install(p).unwrap();
            assert!(l.prompt.contains("NEVER TOUCH"));
            assert!(l.args.is_empty());
            fs::remove_dir_all(sess_parent).unwrap();
            fs::remove_dir_all(work).unwrap();
        }
    }

    #[test]
    fn manage_new_validate_override_and_toggle() {
        let dir = temp_dir("manage-dir");
        for bad in ["", "../x", "A", "a b", "-a", "_g", "a/b"] {
            assert!(new_skill(&dir, bad).is_err(), "{bad:?} should be rejected");
        }
        let p = new_skill(&dir, "my-skill").unwrap();
        assert_eq!(p, dir.join("my-skill/SKILL.md"));
        assert!(validate(&p).is_empty());
        assert!(new_skill(&dir, "my-skill")
            .unwrap_err()
            .contains("already exists"));

        // Broken frontmatter validation
        let bad_dir = dir.join("bad");
        fs::create_dir_all(&bad_dir).unwrap();
        let bad_md = bad_dir.join("SKILL.md");
        fs::write(&bad_md, "no frontmatter\n").unwrap();
        assert!(!validate(&bad_md).is_empty());
        fs::write(&bad_md, "---\nname: other\ndescription: d\n---\nx\n").unwrap();
        assert!(validate(&bad_md).join(" ").contains("folder"));
        assert!(!validate(&dir.join("missing/SKILL.md")).is_empty());

        // Override copies embedded once
        let user = temp_dir("override-user");
        let c = Catalog::load(None, Some(&user));
        let ov_path = override_skill(&c, "code-review", &user).unwrap();
        assert_eq!(ov_path, user.join("code-review/SKILL.md"));
        assert!(user.join("code-review/checklist.md").exists());
        fs::write(
            &ov_path,
            "---\nname: code-review\ndescription: mine\n---\nmine\n",
        )
        .unwrap();
        let c2 = Catalog::load(None, Some(&user));
        let ov_path2 = override_skill(&c2, "code-review", &user).unwrap();
        assert_eq!(ov_path2, ov_path);
        assert!(fs::read_to_string(&ov_path).unwrap().contains("mine"));
        assert!(override_skill(&c2, "nope", &user).is_err());

        // Toggle idempotency, removing last item, and refusing no-effect writes
        let repo = temp_dir("toggle-repo");
        let user_cfg = user.join("config.yaml");
        toggle(&user_cfg, "skills.global", "a", true, &[], &repo).unwrap();
        toggle(&user_cfg, "skills.global", "a", true, &["a".into()], &repo).unwrap();
        toggle(&user_cfg, "skills.global", "b", true, &["a".into()], &repo).unwrap();
        let loaded = load_config_files(&[user_cfg.clone(), project_config_path(&repo)]).unwrap();
        assert_eq!(loaded.skills.global, vec!["a", "b"]);

        toggle(
            &user_cfg,
            "skills.global",
            "a",
            false,
            &["a".into(), "b".into()],
            &repo,
        )
        .unwrap();
        toggle(&user_cfg, "skills.global", "b", false, &["b".into()], &repo).unwrap();
        let loaded = load_config_files(&[user_cfg.clone(), project_config_path(&repo)]).unwrap();
        assert!(loaded.skills.global.is_empty());

        // Project layer overrides user layer -> user toggle must fail and roll back
        let proj_cfg = project_config_path(&repo);
        toggle(&proj_cfg, "skills.global", "debug", true, &[], &repo).unwrap();
        let err = toggle(&user_cfg, "skills.global", "write-tests", true, &[], &repo).unwrap_err();
        assert!(err.contains("no effect"), "{err}");
        let raw_user = fs::read_to_string(&user_cfg).unwrap_or_default();
        assert!(!raw_user.contains("write-tests"));

        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(user).unwrap();
        fs::remove_dir_all(repo).unwrap();
    }
}
