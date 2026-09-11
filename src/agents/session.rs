//! Session selection and closed argv construction for loaded resume profiles.

pub(crate) use super::source::ResumeDefinition as SessionProfile;
use super::source::{ReferenceKind, ResumeOptionsDefinition, ResumeStrategy};

pub(crate) const MAX_RESUME_OPTION_ARGS: usize = 128;
pub(crate) const MAX_RESUME_OPTION_BYTES: usize = 16 * 1024;
const MAX_RESUME_OPTION_VALUE_BYTES: usize = 4096;

pub(crate) fn deserialize_resume_options<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<String>, D::Error> {
    struct OptionsVisitor;
    impl<'de> serde::de::Visitor<'de> for OptionsVisitor {
        type Value = Vec<String>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("bounded resume arguments")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut args = Vec::new();
            let mut bytes = 0;
            while let Some(arg) = seq.next_element::<String>()? {
                bytes += arg.len();
                if args.len() >= MAX_RESUME_OPTION_ARGS
                    || bytes > MAX_RESUME_OPTION_BYTES
                    || arg.len() > MAX_RESUME_OPTION_VALUE_BYTES
                    || arg.contains('\0')
                {
                    return Err(serde::de::Error::custom("resume options exceed limits"));
                }
                args.push(arg);
            }
            Ok(args)
        }
    }
    deserializer.deserialize_seq(OptionsVisitor)
}

pub(crate) fn reserved_resume_option(name: &str) -> bool {
    matches!(
        name,
        "-r" | "--resume"
            | "--session"
            | "--session-id"
            | "--thread"
            | "--conversation"
            | "--continue"
    )
}

impl ResumeOptionsDefinition {
    pub(crate) fn argument_count(&self, args: &[String]) -> usize {
        let Some(arg) = args.first() else { return 0 };
        if reserved_resume_option(arg.split_once('=').map_or(arg.as_str(), |(name, _)| name)) {
            return 0;
        }
        if self.flags.contains(arg) {
            1
        } else if let Some((name, value)) = arg.split_once('=') {
            usize::from(!value.is_empty() && self.options.iter().any(|option| option == name))
        } else if self.options.contains(arg)
            && args.get(1).is_some_and(|value| !value.starts_with('-'))
        {
            2
        } else {
            0
        }
    }

    pub(crate) fn filter(&self, args: &[String]) -> Vec<String> {
        let mut kept = Vec::new();
        let mut bytes = 0;
        let mut index = 0;
        while let Some(arg) = args.get(index) {
            if arg == "--" {
                break;
            }
            let name = arg.split_once('=').map_or(arg.as_str(), |(name, _)| name);
            if reserved_resume_option(name) {
                index += 1;
                if name == arg
                    && name != "--continue"
                    && args.get(index).is_some_and(|value| !value.starts_with('-'))
                {
                    index += 1;
                }
                continue;
            }
            let count = self.argument_count(&args[index..]);
            if count == 0 {
                index += 1;
                continue;
            }
            let group = &args[index..index + count];
            let group_bytes: usize = group.iter().map(String::len).sum();
            if kept.len() + count > MAX_RESUME_OPTION_ARGS
                || bytes + group_bytes > MAX_RESUME_OPTION_BYTES
                || group.iter().any(|value| {
                    value.len() > MAX_RESUME_OPTION_VALUE_BYTES || value.contains('\0')
                })
            {
                return Vec::new();
            }
            kept.extend_from_slice(group);
            bytes += group_bytes;
            index += count;
        }
        kept
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn resume_options_keep_declared_choices_not_prompts_or_session_selectors() {
        let policy = ResumeOptionsDefinition {
            flags: strings(&["--yolo", "--continue"]),
            options: strings(&["--model", "--resume", "-r"]),
        };
        assert_eq!(
            policy.filter(&strings(&[
                "--yolo",
                "--model",
                "model name",
                "--resume",
                "old",
                "--resume=old",
                "-r",
                "old",
                "--continue",
                "--unknown",
                "prompt",
                "--model=other",
            ])),
            strings(&["--yolo", "--model", "model name", "--model=other"])
        );
        assert!(policy
            .filter(&strings(&["--model", "--", "--yolo"]))
            .is_empty());
        assert_eq!(
            policy.filter(&strings(&["--model", ""])),
            strings(&["--model", ""])
        );
    }

    #[test]
    fn resume_options_bounds_never_truncate_a_pair_or_keep_lossy_values() {
        let policy = ResumeOptionsDefinition {
            flags: vec![],
            options: strings(&["--model"]),
        };
        for value in [
            "x".repeat(MAX_RESUME_OPTION_VALUE_BYTES + 1),
            "nul\0value".into(),
        ] {
            assert!(policy.filter(&["--model".into(), value]).is_empty());
        }
        let args = strings(&["--model", "ok"].repeat(MAX_RESUME_OPTION_ARGS));
        assert!(policy.filter(&args).is_empty());
        let mut args = vec!["long prompt ".repeat(MAX_RESUME_OPTION_BYTES)];
        args.extend(strings(&["--model", "ok"]));
        assert_eq!(policy.filter(&args), strings(&["--model", "ok"]));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportReferencePreference {
    IdOnly,
    AbsolutePathThenId,
}

impl SessionProfile {
    pub(crate) fn accepts_id(&self) -> bool {
        self.accepted_references.contains(&ReferenceKind::Id)
    }

    pub(crate) fn accepts_path(&self) -> bool {
        self.accepted_references.contains(&ReferenceKind::Path)
    }

    pub(crate) fn report_preference(&self) -> ReportReferencePreference {
        match self.preferred_reference {
            ReferenceKind::Id => ReportReferencePreference::IdOnly,
            ReferenceKind::Path => ReportReferencePreference::AbsolutePathThenId,
        }
    }

    pub(crate) fn argv(&self, executable: &str, value: &str) -> Vec<String> {
        match self.strategy {
            ResumeStrategy::SeparateFlag | ResumeStrategy::Subcommand => {
                vec![executable.into(), self.token.clone(), value.into()]
            }
            ResumeStrategy::JoinedFlag => vec![executable.into(), format!("{}{value}", self.token)],
        }
    }
}
