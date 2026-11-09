//! Gallery view rendering for browsing, filtering, and previewing patterns.

use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use super::diagram::{generate_topology_diagram, truncate_line};
use super::editor::render_pattern_editor;
use super::state::{PatternSubAction, ShepherdPatternsHits, ShepherdPatternsSectionState};
use crate::app::state::Palette;
use crate::shepherd::ui::components::{
    confirm_stripe::render_confirm_stripe,
    detail_view::{render_detail_view, AlertLevel, DetailContentBlock, DetailHeader},
    empty_state::render_empty_state,
    inline_form::{render_inline_form, FormField, InlineFormState},
    list_view::{render_list_view, ListViewItem},
    {compact_panel, draw_divider, split_panes},
};
use shepherd_core::patterns::PatternSource;

pub fn render_patterns_section(
    buffer: &mut Buffer,
    area: Rect,
    state: &ShepherdPatternsSectionState,
    palette: &Palette,
) -> ShepherdPatternsHits {
    let mut hits = ShepherdPatternsHits::default();
    if area.width < 20 || area.height < 5 {
        return hits;
    }

    let list_width = if area.width >= 100 {
        (area.width * 30 / 100).max(26)
    } else if area.width >= 70 {
        (area.width * 34 / 100).max(24)
    } else {
        24
    };

    let filtered_indices = state.filtered_indices();
    let list_items: Vec<ListViewItem> = filtered_indices
        .iter()
        .map(|&idx| {
            let p = &state.patterns[idx];
            let badge_color = match p.source {
                PatternSource::Project => palette.green,
                PatternSource::Global => palette.yellow,
                PatternSource::Builtin => palette.overlay0,
            };
            let badge_text = format!("[{}]", p.source.as_str().to_uppercase());
            let meta = format!("{} ({} ag.)", p.kind.as_str(), p.agents_count);
            ListViewItem {
                title: p.name.clone(),
                badge: Some((badge_text, Style::default().fg(badge_color))),
                meta: Some(meta),
            }
        })
        .collect();

    let filter_tabs = ["All", "Proyecto", "Global", "Plantillas"];
    let split = split_panes(
        area,
        list_width,
        compact_panel(state.has_sub_action(), list_items.is_empty()),
    );
    let list_area = split.list;
    hits.list_hits = render_list_view(
        buffer,
        list_area,
        &list_items,
        state.selected,
        state.list_scroll,
        Some((&filter_tabs, state.filter_index)),
        Some(("+ New", "n")),
        palette,
    );

    if let Some(detail_area) = split.detail {
        if let Some(sep_x) = split.divider_x {
            draw_divider(
                buffer,
                area,
                sep_x,
                Style::default().fg(palette.surface_dim),
            );
        }
        let detail_w = detail_area.width;

        if let Some(ref sub) = state.sub_action {
            match sub {
                PatternSubAction::ConfirmDelete { name, source } => {
                    let warn = format!(
                        "Permanently delete pattern '{}' with scope {}? This cannot be undone.",
                        name,
                        source.as_str()
                    );
                    hits.stripe_hits = Some(render_confirm_stripe(
                        buffer,
                        detail_area,
                        &warn,
                        "Confirm deletion (d)",
                        "Cancel (Esc)",
                        true,
                        palette,
                    ));
                    return hits;
                }
                PatternSubAction::NewFromTemplate(st) => {
                    let inline_form = InlineFormState {
                        title: "NEW PATTERN FROM TEMPLATE".into(),
                        fields: vec![
                            FormField {
                                label: format!(
                                    "Base template (Space to change): [{}]",
                                    st.selected_kind().as_str()
                                ),
                                value: st.selected_kind().as_str().into(),
                                placeholder: "".into(),
                            },
                            FormField {
                                label: "New pattern name:".into(),
                                value: st.name.clone(),
                                placeholder: "mi-patron".into(),
                            },
                            FormField {
                                label: format!(
                                    "Save scope (Space to toggle): [{}]",
                                    if st.is_global {
                                        "Global (~/.config)"
                                    } else {
                                        "Proyecto (.shepherd)"
                                    }
                                ),
                                value: if st.is_global { "Global" } else { "Proyecto" }.into(),
                                placeholder: "".into(),
                            },
                        ],
                        active_field: st.active_field,
                        error: None,
                        success: None,
                    };
                    hits.new_from_template_hits = Some(render_inline_form(
                        buffer,
                        detail_area,
                        &inline_form,
                        "Create and Edit (Enter)",
                        "Cancel (Esc)",
                        !st.name.trim().is_empty(),
                        palette,
                    ));
                    return hits;
                }
                PatternSubAction::Duplicate(st) => {
                    let inline_form = InlineFormState {
                        title: format!("DUPLICATE PATTERN: {}", st.source_name),
                        fields: vec![
                            FormField {
                                label: "New pattern name:".into(),
                                value: st.name.clone(),
                                placeholder: "copy-name".into(),
                            },
                            FormField {
                                label: format!(
                                    "Save scope (Space to toggle): [{}]",
                                    if st.is_global {
                                        "Global (~/.config)"
                                    } else {
                                        "Proyecto (.shepherd)"
                                    }
                                ),
                                value: if st.is_global { "Global" } else { "Proyecto" }.into(),
                                placeholder: "".into(),
                            },
                        ],
                        active_field: st.active_field,
                        error: None,
                        success: None,
                    };
                    hits.duplicate_hits = Some(render_inline_form(
                        buffer,
                        detail_area,
                        &inline_form,
                        "Duplicate and Edit (Enter)",
                        "Cancel (Esc)",
                        !st.name.trim().is_empty(),
                        palette,
                    ));
                    return hits;
                }
                PatternSubAction::Edit(editor) => {
                    hits.editor_hits =
                        Some(render_pattern_editor(buffer, detail_area, editor, palette));
                    return hits;
                }
                PatternSubAction::Run(st) => {
                    let inline_form = InlineFormState {
                        title: format!("RUN PATTERN: {}", st.pattern_name),
                        fields: vec![
                            FormField {
                                label: "Objective or task (optional):".into(),
                                value: st.task.clone(),
                                placeholder: "e.g. Implement a metrics endpoint".into(),
                            },
                            FormField {
                                label: "Isolated worktree (optional):".into(),
                                value: st.worktree.clone(),
                                placeholder: "p.ej. /tmp/worktree-f8d".into(),
                            },
                        ],
                        active_field: st.active_field,
                        error: None,
                        success: None,
                    };
                    hits.run_form_hits = Some(render_inline_form(
                        buffer,
                        detail_area,
                        &inline_form,
                        "Start Run (Enter)",
                        "Cancel (Esc)",
                        true,
                        palette,
                    ));
                    return hits;
                }
            }
        }

        if let Some(detail) = &state.current_detail {
            let error_count = detail.problems.iter().filter(|p| p.is_error()).count();
            let _warn_count = detail.problems.iter().filter(|p| !p.is_error()).count();
            let validity_badge = if error_count == 0 {
                ("[✓ Valid]".to_string(), Style::default().fg(palette.green))
            } else {
                (
                    format!("[✗ {} errores]", error_count),
                    Style::default().fg(palette.red),
                )
            };

            let src_badge = (
                format!("[{}]", detail.source.as_str().to_uppercase()),
                Style::default().fg(match detail.source {
                    PatternSource::Project => palette.green,
                    PatternSource::Global => palette.yellow,
                    PatternSource::Builtin => palette.overlay0,
                }),
            );

            let kind_badge = (
                format!("[{}]", detail.pattern.kind.as_str()),
                Style::default().fg(palette.accent),
            );

            let meta = format!(
                "Topology: {} | Agents: {} | Topics: {}{}",
                detail.pattern.kind.as_str(),
                detail.pattern.agents.len(),
                detail.pattern.topics.len(),
                detail
                    .path
                    .as_deref()
                    .map(|p| format!(" | {}", p))
                    .unwrap_or_default()
            );

            let is_valid = error_count == 0;
            let header = DetailHeader {
                title: format!("PATTERN: {}", detail.pattern.name),
                badges: vec![src_badge, kind_badge, validity_badge],
                metadata_line: Some(meta),
                actions: vec![
                    ("Edit".into(), Some("e".into())),
                    ("Duplicar".into(), Some("c".into())),
                    ("Validar".into(), Some("v".into())),
                    ("Delete".into(), Some("d".into())),
                    ("Ejecutar".into(), Some("r".into())),
                ],
            };

            let mut blocks = Vec::new();

            blocks.push(DetailContentBlock::Heading("DESCRIPTION".into()));
            blocks.push(DetailContentBlock::Paragraph(
                detail.pattern.description.clone(),
            ));

            blocks.push(DetailContentBlock::Heading(
                "TOPOLOGY DIAGRAM (ASCII)".into(),
            ));
            let diag_lines =
                generate_topology_diagram(&detail.pattern, detail_w.saturating_sub(4) as usize);
            blocks.push(DetailContentBlock::Preformatted(diag_lines));

            blocks.push(DetailContentBlock::Heading(format!(
                "AGENTES ({})",
                detail.pattern.agents.len()
            )));
            let mut agent_entries = Vec::new();
            for a in &detail.pattern.agents {
                let mut desc = format!("ID: {} | Role: {}", a.id, a.role);
                if a.count > 1 {
                    desc.push_str(&format!(" (x{})", a.count));
                }
                if let Some(ref v) = a.vendor {
                    desc.push_str(&format!(" | vendor: {}", v));
                }
                if let Some(ref m) = a.model {
                    desc.push_str(&format!(" | model: {}", m));
                }
                if let Some(l) = a.level {
                    desc.push_str(&format!(" | nivel: {}", l));
                }
                if let Some(ref g) = a.group {
                    desc.push_str(&format!(" | grupo: {}", g));
                }
                if !a.publishes.is_empty() {
                    desc.push_str(&format!(" | publica: [{}]", a.publishes.join(", ")));
                }
                if !a.consumes.is_empty() {
                    desc.push_str(&format!(" | consume: [{}]", a.consumes.join(", ")));
                }
                if let Some(ref p) = a.prompt {
                    let prompt_snippet = truncate_line(p, 40);
                    desc.push_str(&format!(" | persona: \"{}\"", prompt_snippet));
                }
                agent_entries.push(desc);
            }
            blocks.push(DetailContentBlock::BulletList(agent_entries));

            blocks.push(DetailContentBlock::Heading(format!(
                "TOPICS ({})",
                detail.pattern.topics.len()
            )));
            let mut topic_entries = Vec::new();
            for t in &detail.pattern.topics {
                let mut desc = format!("{}: {} partition(s)", t.name, t.partitions);
                if let Some(ref k) = t.key {
                    desc.push_str(&format!(" | clave: {}", k));
                }
                if let Some(ref f) = t.filter {
                    desc.push_str(&format!(" | filtro: {}", f));
                }
                topic_entries.push(desc);
            }
            blocks.push(DetailContentBlock::BulletList(topic_entries));

            if !detail.problems.is_empty() {
                blocks.push(DetailContentBlock::Heading("VALIDATION DIAGNOSTICS".into()));
                for p in &detail.problems {
                    blocks.push(DetailContentBlock::Notice {
                        text: format!("[{}] {}: {}", p.severity, p.path, p.message),
                        level: if p.is_error() {
                            AlertLevel::Warning
                        } else {
                            AlertLevel::Info
                        },
                    });
                }
            }

            let bottom_actions = [
                ("Edit", "e", true),
                ("Duplicar", "c", false),
                ("Validar", "v", false),
                ("Delete", "d", false),
                ("Ejecutar", "r", is_valid),
            ];

            hits.detail_hits = Some(render_detail_view(
                buffer,
                detail_area,
                &header,
                &blocks,
                state.detail_scroll,
                &bottom_actions,
                palette,
            ));
        } else if list_items.is_empty() {
            hits.empty_hits = Some(render_empty_state(
                buffer,
                detail_area,
                "NO PATTERNS",
                "No patterns available with this filter.\nCreate your first multi-agent pattern from a template.",
                &[("+ New from Template", Some("n"))],
                palette,
            ));
        }
    }

    hits
}
