//! Editing a pattern: the operations behind the mouse editor (add, connect, rename, delete, duplicate, save).
//! Pure data, so every rule is tested without a window.

use shepherd_core::patterns::{self, Pattern, PatternAgent, PatternSource, Topic};
use std::path::{Path, PathBuf};

/// What is selected in the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sel {
    Agent(String),
    Topic(String),
}

impl Sel {
    /// The key a graph node carries (`agent:id` / `topic:name`).
    pub fn key(&self) -> String {
        match self {
            Sel::Agent(id) => format!("agent:{id}"),
            Sel::Topic(name) => format!("topic:{name}"),
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key.split_once(':')? {
            ("agent", id) => Some(Sel::Agent(id.to_owned())),
            ("topic", name) => Some(Sel::Topic(name.to_owned())),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentField {
    Id,
    Role,
    Vendor,
    Model,
    Count,
    Level,
    Group,
}

#[derive(Debug, Clone)]
pub struct Editor {
    pub pattern: Pattern,
    pub source: PatternSource,
    /// A pattern that was never saved under this name (a duplicate or a fresh one).
    pub is_new: bool,
    pub dirty: bool,
}

fn unique(base: &str, taken: impl Fn(&str) -> bool) -> String {
    (1..)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !taken(candidate))
        .unwrap_or_else(|| base.to_owned())
}

impl Editor {
    pub fn open(pattern: Pattern, source: PatternSource) -> Self {
        Self {
            pattern,
            source,
            is_new: false,
            dirty: false,
        }
    }

    /// Built-in templates cannot be saved over: duplicate them first.
    pub fn read_only(&self) -> bool {
        self.source == PatternSource::Builtin && !self.is_new
    }

    fn touch(&mut self) {
        self.dirty = true;
    }

    pub fn add_agent(&mut self) -> Sel {
        let id = unique("agent", |c| self.pattern.agents.iter().any(|a| a.id == c));
        self.pattern.agents.push(PatternAgent {
            id: id.clone(),
            role: "coder".into(),
            ..PatternAgent::default()
        });
        self.touch();
        Sel::Agent(id)
    }

    pub fn add_topic(&mut self) -> Sel {
        let name = unique("topic", |c| self.pattern.topics.iter().any(|t| t.name == c));
        self.pattern.topics.push(Topic {
            name: name.clone(),
            partitions: 1,
            key: None,
            filter: None,
        });
        self.touch();
        Sel::Topic(name)
    }

    /// Agent → topic means "publishes", topic → agent means "consumes"; anything else is not a connection.
    pub fn connect(&mut self, from: &Sel, to: &Sel) -> Result<(), String> {
        match (from, to) {
            (Sel::Agent(agent), Sel::Topic(topic)) => {
                let a = self
                    .pattern
                    .agents
                    .iter_mut()
                    .find(|a| &a.id == agent)
                    .ok_or("unknown agent")?;
                if !a.publishes.contains(topic) {
                    a.publishes.push(topic.clone());
                }
            }
            (Sel::Topic(topic), Sel::Agent(agent)) => {
                let a = self
                    .pattern
                    .agents
                    .iter_mut()
                    .find(|a| &a.id == agent)
                    .ok_or("unknown agent")?;
                if !a.consumes.contains(topic) {
                    a.consumes.push(topic.clone());
                }
            }
            (Sel::Agent(_), Sel::Agent(_)) => {
                return Err("agents talk through topics: connect an agent to a topic".into())
            }
            (Sel::Topic(_), Sel::Topic(_)) => {
                return Err("a topic connects to agents, not to another topic".into())
            }
        }
        self.touch();
        Ok(())
    }

    pub fn disconnect(&mut self, from: &Sel, to: &Sel) {
        match (from, to) {
            (Sel::Agent(agent), Sel::Topic(topic)) => self
                .pattern
                .agents
                .iter_mut()
                .filter(|a| &a.id == agent)
                .for_each(|a| a.publishes.retain(|t| t != topic)),
            (Sel::Topic(topic), Sel::Agent(agent)) => self
                .pattern
                .agents
                .iter_mut()
                .filter(|a| &a.id == agent)
                .for_each(|a| a.consumes.retain(|t| t != topic)),
            _ => {}
        }
        self.touch();
    }

    /// Removes the node and every arrow that touched it.
    pub fn delete(&mut self, sel: &Sel) {
        match sel {
            Sel::Agent(id) => self.pattern.agents.retain(|a| &a.id != id),
            Sel::Topic(name) => {
                self.pattern.topics.retain(|t| &t.name != name);
                for a in &mut self.pattern.agents {
                    a.publishes.retain(|t| t != name);
                    a.consumes.retain(|t| t != name);
                }
            }
        }
        self.touch();
    }

    /// Renames an agent or topic (a topic's arrows follow it). Fails when the name is taken or empty.
    pub fn rename(&mut self, sel: &Sel, new_name: &str) -> Result<Sel, String> {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            return Err("the name cannot be empty".into());
        }
        match sel {
            Sel::Agent(old) => {
                if new_name != old && self.pattern.agents.iter().any(|a| a.id == new_name) {
                    return Err(format!("there is already an agent called {new_name}"));
                }
                self.pattern
                    .agents
                    .iter_mut()
                    .filter(|a| &a.id == old)
                    .for_each(|a| a.id = new_name.to_owned());
                self.touch();
                Ok(Sel::Agent(new_name.to_owned()))
            }
            Sel::Topic(old) => {
                if new_name != old && self.pattern.topics.iter().any(|t| t.name == new_name) {
                    return Err(format!("there is already a topic called {new_name}"));
                }
                self.pattern
                    .topics
                    .iter_mut()
                    .filter(|t| &t.name == old)
                    .for_each(|t| t.name = new_name.to_owned());
                for a in &mut self.pattern.agents {
                    for t in a
                        .publishes
                        .iter_mut()
                        .chain(a.consumes.iter_mut())
                        .filter(|t| *t == old)
                    {
                        *t = new_name.to_owned();
                    }
                }
                self.touch();
                Ok(Sel::Topic(new_name.to_owned()))
            }
        }
    }

    pub fn set_agent_field(
        &mut self,
        id: &str,
        field: AgentField,
        value: &str,
    ) -> Result<(), String> {
        let value = value.trim();
        let optional = |v: &str| (!v.is_empty()).then(|| v.to_owned());
        let agent = self
            .pattern
            .agents
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or("unknown agent")?;
        match field {
            AgentField::Id => return self.rename(&Sel::Agent(id.to_owned()), value).map(|_| ()),
            AgentField::Role => agent.role = value.to_owned(),
            AgentField::Vendor => agent.vendor = optional(value),
            AgentField::Model => agent.model = optional(value),
            AgentField::Group => agent.group = optional(value),
            AgentField::Count => {
                agent.count = value
                    .parse::<u32>()
                    .ok()
                    .filter(|n| *n >= 1)
                    .ok_or("count is a number, 1 or more")?
            }
            AgentField::Level => {
                agent.level = if value.is_empty() {
                    None
                } else {
                    Some(value.parse::<u32>().map_err(|_| "level is a number")?)
                }
            }
        }
        self.touch();
        Ok(())
    }

    pub fn set_start(&mut self, id: &str, start: bool) {
        self.pattern
            .agents
            .iter_mut()
            .filter(|a| a.id == id)
            .for_each(|a| a.start = start);
        self.touch();
    }

    pub fn set_partitions(&mut self, topic: &str, value: &str) -> Result<(), String> {
        let n = value
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|n| *n >= 1)
            .ok_or("partitions is a number, 1 or more")?;
        self.pattern
            .topics
            .iter_mut()
            .filter(|t| t.name == topic)
            .for_each(|t| t.partitions = n);
        self.touch();
        Ok(())
    }

    /// A copy under a new name, to be saved as a new pattern.
    pub fn duplicate(&self, new_name: &str) -> Editor {
        let mut pattern = self.pattern.clone();
        pattern.name = new_name.to_owned();
        Editor {
            pattern,
            source: PatternSource::Global,
            is_new: true,
            dirty: true,
        }
    }

    /// Writes the pattern. A new one never overwrites another; a built-in one is never written.
    pub fn save(&mut self, project_root: &Path) -> Result<PathBuf, String> {
        if self.read_only() {
            return Err("built-in patterns are read-only: duplicate it first".into());
        }
        patterns::validate_pattern_name(&self.pattern.name).map_err(|e| e.to_string())?;
        let dir = if self.source == PatternSource::Project && !self.is_new {
            patterns::project_patterns_dir(project_root)
        } else {
            patterns::global_patterns_dir_with(None)
        };
        if self.is_new && dir.join(format!("{}.yaml", self.pattern.name)).exists() {
            return Err(format!(
                "a pattern called {} already exists",
                self.pattern.name
            ));
        }
        let path = patterns::save_pattern_to_dir(&self.pattern, &dir).map_err(|e| e.to_string())?;
        if self.is_new {
            self.source = if dir == patterns::project_patterns_dir(project_root) {
                PatternSource::Project
            } else {
                PatternSource::Global
            };
        }
        self.is_new = false;
        self.dirty = false;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shepherd_core::patterns::get_builtin_pattern;

    fn editor() -> Editor {
        Editor::open(
            get_builtin_pattern("blackboard").unwrap(),
            PatternSource::Project,
        )
    }

    #[test]
    fn nodes_get_unique_names_and_connect_only_in_the_two_valid_directions() {
        let mut e = editor();
        let a = e.add_agent();
        let b = e.add_agent();
        assert_ne!(a, b);
        let t = e.add_topic();
        e.connect(&a, &t).unwrap();
        e.connect(&t, &b).unwrap();
        e.connect(&a, &t).unwrap(); // twice is still once
        let agent = |id: &str| {
            e.pattern
                .agents
                .iter()
                .find(|x| x.id == id)
                .unwrap()
                .clone()
        };
        let (Sel::Agent(ai), Sel::Agent(bi), Sel::Topic(ti)) = (&a, &b, &t) else {
            panic!()
        };
        assert_eq!(agent(ai).publishes, vec![ti.clone()]);
        assert_eq!(agent(bi).consumes, vec![ti.clone()]);
        assert!(e.connect(&a, &b).is_err());
        assert!(e.connect(&t, &t).is_err());
        assert!(e.dirty);
    }

    #[test]
    fn deleting_or_renaming_a_topic_keeps_every_arrow_consistent() {
        let mut e = editor();
        let topic = Sel::Topic("blackboard".into());
        let renamed = e.rename(&topic, "ideas").unwrap();
        assert_eq!(renamed, Sel::Topic("ideas".into()));
        assert!(e
            .pattern
            .agents
            .iter()
            .all(|a| !a.publishes.contains(&"blackboard".to_owned())
                && !a.consumes.contains(&"blackboard".to_owned())));
        assert!(e
            .pattern
            .agents
            .iter()
            .any(|a| a.publishes.contains(&"ideas".to_owned())));
        e.delete(&renamed);
        assert!(e.pattern.topics.is_empty());
        assert!(e
            .pattern
            .agents
            .iter()
            .all(|a| a.publishes.is_empty() && a.consumes.is_empty()));
    }

    #[test]
    fn names_and_numbers_are_checked() {
        let mut e = editor();
        let first = e.pattern.agents[0].id.clone();
        let second = e.pattern.agents[1].id.clone();
        assert!(
            e.rename(&Sel::Agent(first.clone()), &second).is_err(),
            "a taken name"
        );
        assert!(
            e.rename(&Sel::Agent(first.clone()), "  ").is_err(),
            "an empty name"
        );
        assert!(e.set_agent_field(&first, AgentField::Count, "0").is_err());
        assert!(e.set_agent_field(&first, AgentField::Count, "3").is_ok());
        assert_eq!(e.pattern.agents[0].count, 3);
        assert!(e.set_agent_field(&first, AgentField::Vendor, "").is_ok());
        assert_eq!(
            e.pattern.agents[0].vendor, None,
            "an empty optional field is cleared"
        );
        assert!(e.set_partitions("blackboard", "x").is_err());
        assert!(e.set_partitions("blackboard", "4").is_ok());
    }

    #[test]
    fn builtin_is_read_only_until_duplicated_and_a_new_pattern_never_overwrites() {
        let _env = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("native-edit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("XDG_CONFIG_HOME", dir.join("cfg"));
        let root = dir.join("proj");
        std::fs::create_dir_all(&root).unwrap();

        let mut builtin = Editor::open(
            get_builtin_pattern("blackboard").unwrap(),
            PatternSource::Builtin,
        );
        assert!(builtin.save(&root).unwrap_err().contains("read-only"));

        let mut copy = builtin.duplicate("mi_pizarra");
        let path = copy.save(&root).unwrap();
        assert!(path.exists());
        assert!(!copy.dirty && !copy.is_new);

        let mut again = builtin.duplicate("mi_pizarra");
        assert!(
            again.save(&root).unwrap_err().contains("already exists"),
            "a new pattern does not overwrite"
        );
        assert!(builtin.duplicate("invalid name!").save(&root).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn selection_keys_round_trip() {
        for sel in [Sel::Agent("a-1".into()), Sel::Topic("orders".into())] {
            assert_eq!(Sel::from_key(&sel.key()), Some(sel));
        }
        assert_eq!(Sel::from_key("nonsense"), None);
    }
}
