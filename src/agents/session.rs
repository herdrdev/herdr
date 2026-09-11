//! Session selection and closed argv construction for loaded resume profiles.

pub(crate) use super::source::ResumeDefinition as SessionProfile;
use super::source::{ReferenceKind, ResumeStrategy};

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
