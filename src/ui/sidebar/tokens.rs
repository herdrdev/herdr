use super::{
    canonical_sidebar_agent_identity, compact_agent_identity, gate_override_label,
    title_repeats_agent_identity, AgentPanelEntry, AgentPanelEntryData, AgentPanelIdentity,
    AgentPanelLocalTarget, DEFAULT_THREAD_TITLE,
};
use crate::config::{
    AgentSidebarToken, AgentsSidebarConfig, SidebarTokenStyle, SpaceSidebarToken,
    SpacesSidebarConfig,
};

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedToken {
    pub kind: ResolvedTokenKind,
    pub style: SidebarTokenStyle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResolvedTokenKind {
    StateIcon,
    StateText(String),
    RequiredStateText(String),
    Workspace(String),
    Tab(String),
    Pane(String),
    Agent(String),
    TerminalTitle(String),
    Branch(String),
    GitStatus { ahead: usize, behind: usize },
    Custom(String),
}

impl ResolvedToken {
    fn new(kind: ResolvedTokenKind, style: SidebarTokenStyle) -> Self {
        Self { kind, style }
    }

    #[cfg(test)]
    pub(super) fn unstyled(kind: ResolvedTokenKind) -> Self {
        Self::new(kind, SidebarTokenStyle::default())
    }
}

pub(super) fn agent_rows(
    config: &AgentsSidebarConfig,
    entry: &AgentPanelEntry,
    state_text: &str,
) -> Vec<Vec<ResolvedToken>> {
    config
        .rows_for_agent(entry.agent)
        .iter()
        .filter_map(|row| {
            let resolved = row
                .iter()
                .filter_map(|configured| {
                    let (token, style) = configured.parts();
                    let kind = match token {
                        AgentSidebarToken::StateIcon => Some(ResolvedTokenKind::StateIcon),
                        AgentSidebarToken::StateText => {
                            Some(ResolvedTokenKind::StateText(state_text.to_string()))
                        }
                        AgentSidebarToken::Workspace => {
                            Some(ResolvedTokenKind::Workspace(entry.primary_label.clone()))
                        }
                        AgentSidebarToken::Tab => {
                            entry.primary_tab_label.clone().map(ResolvedTokenKind::Tab)
                        }
                        AgentSidebarToken::Pane => {
                            entry.pane_label.clone().map(ResolvedTokenKind::Pane)
                        }
                        AgentSidebarToken::Agent => canonical_sidebar_agent_identity(entry)
                            .map(str::to_string)
                            .map(ResolvedTokenKind::Agent),
                        AgentSidebarToken::TerminalTitle => entry
                            .terminal_title
                            .clone()
                            .map(ResolvedTokenKind::TerminalTitle),
                        AgentSidebarToken::TerminalTitleStripped => entry
                            .terminal_title_stripped
                            .clone()
                            .map(ResolvedTokenKind::TerminalTitle),
                        AgentSidebarToken::Custom(name) => entry
                            .tokens
                            .get(name)
                            .cloned()
                            .map(ResolvedTokenKind::Custom),
                        AgentSidebarToken::Styled { .. } => None,
                    }?;
                    Some(ResolvedToken::new(kind, style))
                })
                .collect::<Vec<_>>();
            (!resolved.is_empty()).then_some(resolved)
        })
        .collect()
}

/// Worklist rows point at a thread in the tree, so they ignore the configured
/// multi-row template. The compact agent identity disambiguates otherwise
/// identical titles without repeating a provider or CLI version.
pub(super) fn worklist_row(entry: &AgentPanelEntry) -> Vec<Vec<ResolvedToken>> {
    let visible_terminal_title =
        |title: String| (!title_repeats_agent_identity(entry, &title)).then_some(title);
    let title = entry
        .terminal_title_stripped
        .clone()
        .and_then(&visible_terminal_title)
        .or_else(|| {
            entry
                .terminal_title
                .clone()
                .and_then(&visible_terminal_title)
        })
        .or_else(|| {
            (!entry.pane_label_is_agent_identity)
                .then(|| entry.pane_label.clone())
                .flatten()
        })
        .or_else(|| {
            (!entry.tab_label_leads_with_agent)
                .then(|| entry.primary_tab_label.clone())
                .flatten()
        })
        // Keep the row identifiable when every live title source is absent;
        // the stable thread fallback is more useful here than a bare state dot.
        .or_else(|| Some(DEFAULT_THREAD_TITLE.to_string()));
    let mut row = vec![ResolvedToken::new(
        ResolvedTokenKind::StateIcon,
        SidebarTokenStyle::default(),
    )];
    if let Some(title) = title {
        let identity = compact_agent_identity(entry, &title).map(str::to_string);
        row.push(ResolvedToken::new(
            ResolvedTokenKind::Tab(title),
            SidebarTokenStyle::default(),
        ));
        if let Some(identity) = identity {
            row.push(ResolvedToken::new(
                ResolvedTokenKind::Agent(identity),
                SidebarTokenStyle::default(),
            ));
        }
    }
    if entry.usage_limited {
        row.push(ResolvedToken::new(
            ResolvedTokenKind::RequiredStateText(gate_override_label(entry)),
            SidebarTokenStyle::default(),
        ));
    }
    vec![row]
}

#[allow(dead_code)]
pub(super) struct SpaceTokenContext<'a> {
    pub workspace: &'a str,
    pub branch: Option<&'a str>,
    pub state_text: &'a str,
    pub ahead_behind: Option<(usize, usize)>,
    pub tokens: &'a std::collections::HashMap<String, String>,
    pub suppress_git_details: bool,
}

#[allow(dead_code)]
pub(super) fn space_rows(
    config: &SpacesSidebarConfig,
    context: SpaceTokenContext<'_>,
) -> Vec<Vec<ResolvedToken>> {
    config
        .rows
        .iter()
        .filter_map(|row| {
            let resolved = row
                .iter()
                .filter_map(|configured| {
                    let (token, style) = configured.parts();
                    let kind = match token {
                        SpaceSidebarToken::StateIcon => Some(ResolvedTokenKind::StateIcon),
                        SpaceSidebarToken::StateText => {
                            Some(ResolvedTokenKind::StateText(context.state_text.to_string()))
                        }
                        SpaceSidebarToken::Workspace => {
                            Some(ResolvedTokenKind::Workspace(context.workspace.to_string()))
                        }
                        SpaceSidebarToken::Branch if !context.suppress_git_details => context
                            .branch
                            .map(|branch| ResolvedTokenKind::Branch(branch.to_string())),
                        SpaceSidebarToken::Branch => None,
                        SpaceSidebarToken::GitStatus if !context.suppress_git_details => context
                            .ahead_behind
                            .filter(|(ahead, behind)| *ahead > 0 || *behind > 0)
                            .map(|(ahead, behind)| ResolvedTokenKind::GitStatus { ahead, behind }),
                        SpaceSidebarToken::GitStatus => None,
                        SpaceSidebarToken::Custom(name) => context
                            .tokens
                            .get(name)
                            .cloned()
                            .map(ResolvedTokenKind::Custom),
                        SpaceSidebarToken::Styled { .. } => None,
                    }?;
                    Some(ResolvedToken::new(kind, style))
                })
                .collect::<Vec<_>>();
            (!resolved.is_empty()).then_some(resolved)
        })
        .collect()
}

pub(super) fn separator(previous: &ResolvedToken, current: &ResolvedToken) -> &'static str {
    if matches!(previous.kind, ResolvedTokenKind::StateIcon)
        || matches!(current.kind, ResolvedTokenKind::GitStatus { .. })
    {
        " "
    } else {
        " · "
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentSidebarToken, SpaceSidebarToken};
    use crate::detect::AgentState;

    fn entry() -> AgentPanelEntry {
        AgentPanelEntry::new(
            AgentPanelIdentity::Local(AgentPanelLocalTarget {
                ws_idx: 0,
                tab_idx: 0,
                pane_id: crate::layout::PaneId::from_raw(1),
            }),
            AgentPanelEntryData {
                usage_limited: false,
                primary_label: "repo".into(),
                space_label: String::new(),
                primary_tab_label: None,
                tab_has_custom_name: false,
                tab_label_leads_with_agent: false,
                pane_label: None,
                pane_label_is_agent_identity: false,
                terminal_title: None,
                terminal_title_stripped: None,
                agent_label: Some("pi".into()),
                agent_kind_label: Some("pi".into()),
                agent: Some(crate::detect::Agent::Pi),
                agent_context: Some(crate::detect::Agent::Pi),
                has_agent: true,
                foreground_process_name: None,
                prio: false,
                starred: false,
                state: AgentState::Working,
                attention_tier: None,
                open_blockers: false,
                completion_tier: None,
                active_subagents: None,
                model_letter: None,
                waiting_on_agents: false,
                working_while_blocked: false,
                holds_shell: false,
                gate_count: 0,
                seen: true,
                done_since: None,
                stale: false,
                reported_at: None,
                last_agent_state_change_seq: None,
                activity_at: None,
                state_labels: std::collections::HashMap::new(),
                tokens: std::collections::HashMap::new(),
                tab_first_pane: false,
                remote_host: None,
            },
        )
    }

    #[test]
    fn missing_custom_tokens_elide_rows_and_separators() {
        let entry = entry();
        let config = AgentsSidebarConfig {
            rows: vec![
                vec![
                    AgentSidebarToken::StateIcon,
                    AgentSidebarToken::Custom("missing".into()),
                ],
                vec![AgentSidebarToken::Custom("missing".into())],
                vec![AgentSidebarToken::Agent],
            ],
            ..Default::default()
        };

        let rows = agent_rows(&config, &entry, "working");

        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            vec![ResolvedToken::unstyled(ResolvedTokenKind::StateIcon)]
        );
        assert_eq!(
            rows[1],
            vec![ResolvedToken::unstyled(ResolvedTokenKind::Agent(
                "pi".into()
            ))]
        );
    }

    #[test]
    fn worklist_rows_are_one_line_of_title_and_compact_agent_identity() {
        let mut entry = entry();
        entry.terminal_title_stripped = Some("Review herdr context enrichment".into());
        entry.agent_label = Some("codex".into());

        let rows = worklist_row(&entry);

        assert_eq!(
            rows,
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::StateIcon),
                ResolvedToken::unstyled(ResolvedTokenKind::Tab(
                    "Review herdr context enrichment".into()
                )),
                ResolvedToken::unstyled(ResolvedTokenKind::Agent("pi".into())),
            ]]
        );
    }

    #[test]
    fn usage_limited_worklist_row_keeps_the_configured_state_label() {
        let mut entry = entry();
        entry.state = AgentState::Blocked;
        entry.usage_limited = true;
        entry.terminal_title_stripped = Some("Wait for plan reset".into());
        entry.state_labels.insert("usage".into(), "limit".into());

        assert_eq!(
            worklist_row(&entry),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::StateIcon),
                ResolvedToken::unstyled(ResolvedTokenKind::Tab("Wait for plan reset".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::Agent("pi".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::RequiredStateText("limit".into())),
            ]]
        );
    }

    #[test]
    fn known_suffixless_agent_uses_canonical_identity_instead_of_version() {
        let mut entry = entry();
        entry.agent = Some(crate::detect::Agent::Gemini);
        entry.agent_context = entry.agent;
        entry.agent_kind_label = Some("gemini".into());
        entry.agent_label = Some("0.9.3".into());
        entry.primary_tab_label = Some("0.9.3".into());
        entry.pane_label = Some("0.9.3".into());
        entry.pane_label_is_agent_identity = true;
        entry.tab_label_leads_with_agent = true;
        let config = AgentsSidebarConfig {
            rows: vec![vec![AgentSidebarToken::Agent]],
            ..Default::default()
        };

        assert_eq!(
            agent_rows(&config, &entry, "working"),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Agent(
                "gemini".into()
            ))]]
        );
        assert_eq!(
            worklist_row(&entry),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::StateIcon),
                ResolvedToken::unstyled(ResolvedTokenKind::Tab("New Thread".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::Agent("gemini".into())),
            ]]
        );
    }

    #[test]
    fn worklist_identity_compares_against_final_visible_title() {
        let mut entry = entry();
        entry.agent = Some(crate::detect::Agent::Codex);
        entry.agent_context = entry.agent;
        entry.agent_kind_label = Some("codex".into());
        entry.agent_label = Some("2.1.245".into());
        entry.terminal_title_stripped = Some("2.1.245".into());
        entry.primary_tab_label = Some("Codex".into());
        entry.tab_label_leads_with_agent = false;

        assert_eq!(
            worklist_row(&entry),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::StateIcon),
                ResolvedToken::unstyled(ResolvedTokenKind::Tab("Codex".into())),
            ]]
        );
    }

    #[test]
    fn a_titleless_worklist_row_uses_the_default_thread_title() {
        let rows = worklist_row(&entry());
        assert_eq!(
            rows,
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::StateIcon),
                ResolvedToken::unstyled(ResolvedTokenKind::Tab("New Thread".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::Agent("pi".into())),
            ]]
        );
    }

    #[test]
    fn state_text_and_arbitrary_values_are_independent_tokens() {
        let mut entry = entry();
        entry
            .tokens
            .insert("summary".into(), "reviewing auth".into());
        let config = AgentsSidebarConfig {
            rows: vec![vec![
                AgentSidebarToken::StateText,
                AgentSidebarToken::Custom("summary".into()),
            ]],
            ..Default::default()
        };

        assert_eq!(
            agent_rows(&config, &entry, "deep in the mines"),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::StateText("deep in the mines".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::Custom("reviewing auth".into())),
            ]]
        );
    }

    #[test]
    fn terminal_title_builtins_are_distinct_from_custom_tokens() {
        let mut entry = entry();
        entry.terminal_title = Some("⠋ raw title".into());
        entry.terminal_title_stripped = Some("raw title".into());
        entry
            .tokens
            .insert("terminal_title".into(), "custom title".into());
        let config = AgentsSidebarConfig {
            rows: vec![vec![
                AgentSidebarToken::TerminalTitle,
                AgentSidebarToken::TerminalTitleStripped,
                AgentSidebarToken::Custom("terminal_title".into()),
            ]],
            ..Default::default()
        };

        assert_eq!(
            agent_rows(&config, &entry, "working"),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::TerminalTitle("⠋ raw title".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::TerminalTitle("raw title".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::Custom("custom title".into())),
            ]]
        );
    }

    #[test]
    fn known_agent_override_replaces_default_rows() {
        let mut config = AgentsSidebarConfig {
            rows: vec![vec![AgentSidebarToken::Workspace]],
            ..Default::default()
        };
        config
            .rows_by_agent
            .insert("pi".into(), vec![vec![AgentSidebarToken::Agent]]);
        let mut pi = entry();
        pi.agent_label = Some("2.1.245".into());

        assert_eq!(
            agent_rows(&config, &pi, "working"),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Agent(
                "pi".into()
            ))]]
        );

        pi.agent = None;
        assert_eq!(
            agent_rows(&config, &pi, "working"),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Workspace(
                "repo".into()
            ))]]
        );
    }

    #[test]
    fn dotted_unknown_agent_names_remain_valid_agent_tokens() {
        for label in ["3.5", "2026.08", "2026.08.26"] {
            let mut custom = entry();
            custom.agent = None;
            custom.agent_kind_label = None;
            custom.agent_label = Some(label.into());
            let config = AgentsSidebarConfig {
                rows: vec![vec![AgentSidebarToken::Agent]],
                ..Default::default()
            };

            assert_eq!(
                agent_rows(&config, &custom, "working"),
                vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Agent(
                    label.into()
                ))]]
            );
        }
    }

    #[test]
    fn dotted_manual_title_remains_visible_for_known_agent() {
        let mut entry = entry();
        entry.agent = Some(crate::detect::Agent::Codex);
        entry.agent_context = entry.agent;
        entry.agent_kind_label = Some("codex".into());
        entry.agent_label = Some("2.1.245".into());
        entry.primary_tab_label = Some("2026.08.26".into());
        entry.tab_label_leads_with_agent = false;

        assert_eq!(
            worklist_row(&entry),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::StateIcon),
                ResolvedToken::unstyled(ResolvedTokenKind::Tab("2026.08.26".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::Agent("cx".into())),
            ]]
        );
    }

    #[test]
    fn grouped_children_suppress_all_builtin_git_details() {
        let config = SpacesSidebarConfig::default();

        assert_eq!(
            space_rows(
                &config,
                SpaceTokenContext {
                    workspace: "feature",
                    branch: Some("worktree/feature"),
                    state_text: "idle",
                    ahead_behind: Some((2, 1)),
                    tokens: &std::collections::HashMap::new(),
                    suppress_git_details: true,
                },
            ),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::StateIcon),
                ResolvedToken::unstyled(ResolvedTokenKind::Workspace("feature".into())),
            ]]
        );
    }

    #[test]
    fn workspace_custom_token_can_replace_git_specific_details() {
        let tokens = std::collections::HashMap::from([("jj_status".into(), "2 changes".into())]);
        let config = SpacesSidebarConfig {
            rows: vec![vec![SpaceSidebarToken::Custom("jj_status".into())]],
            ..Default::default()
        };

        assert_eq!(
            space_rows(
                &config,
                SpaceTokenContext {
                    workspace: "repo",
                    branch: None,
                    state_text: "idle",
                    ahead_behind: None,
                    tokens: &tokens,
                    suppress_git_details: false,
                },
            ),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Custom(
                "2 changes".into()
            ))]]
        );
    }
}
