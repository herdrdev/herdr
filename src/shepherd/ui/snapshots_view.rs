//! Snapshot timeline (F11c): pure rendering. Takes `&ShepherdSnapshotsState`, only draws.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use super::components::{
    compact_panel,
    confirm_stripe::{render_confirm_stripe, ConfirmStripeHits},
    detail_view::{
        render_detail_view, AlertLevel, DetailContentBlock, DetailHeader, DetailViewHits,
    },
    draw_divider,
    empty_state::{render_empty_state, EmptyStateHits},
    inline_form::{render_inline_form, InlineFormHits},
    list_view::{put_str, render_list_view, text_width, ListViewHits, ListViewItem},
    split_panes,
};
use super::snapshots::{
    DiffMode, FileSelector, ShepherdSnapshotsState, SnapSub, SnapshotButton, SnapshotSource,
};
use crate::app::state::Palette;
use shepherd_core::snapshot::{format_utc, SnapshotInfo, SnapshotReason};

/// Files listed in the summary of a snapshot before collapsing into "… y N más".
const MAX_LISTED_FILES: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileButton {
    Restore,
    ToggleAll,
    Cancel,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdSnapshotsHits {
    pub list_hits: ListViewHits,
    /// Detail buttons; the index is the position in `SnapshotButton::ALL`.
    pub detail_hits: Option<DetailViewHits>,
    pub form_hits: Option<InlineFormHits>,
    pub stripe_hits: Option<ConfirmStripeHits>,
    pub empty_hits: Option<EmptyStateHits>,
    pub file_rows: Vec<(Rect, usize)>,
    pub file_buttons: Vec<(Rect, FileButton)>,
    /// `[ Capturar ahora (c) ]` in the detail header.
    pub header_capture: Option<Rect>,
    /// Whole detail pane (wheel scrolls it).
    pub detail_rect: Option<Rect>,
}

/// "hace 5 min" style age; `now < created` (clock skew) reads as "ahora".
pub fn rel_time(now: u64, created: u64) -> String {
    let d = now.saturating_sub(created);
    match d {
        0..=4 => "ahora".into(),
        5..=59 => format!("hace {d}s"),
        60..=3599 => format!("hace {}m", d / 60),
        3600..=86_399 => format!("hace {}h", d / 3600),
        _ => format!("hace {}d", d / 86_400),
    }
}

pub fn reason_color(reason: SnapshotReason, palette: &Palette) -> ratatui::style::Color {
    match reason {
        SnapshotReason::Manual => palette.accent,
        SnapshotReason::Idle | SnapshotReason::Done => palette.green,
        SnapshotReason::Periodic => palette.overlay0,
        SnapshotReason::PreRestore => palette.yellow,
        _ => palette.red,
    }
}

fn status_letter(status: &str) -> &'static str {
    match status {
        "added" => "A",
        "deleted" => "D",
        "type-changed" => "T",
        _ => "M",
    }
}

fn list_item(
    s: &SnapshotInfo,
    source: SnapshotSource,
    now: u64,
    palette: &Palette,
) -> ListViewItem {
    let when = rel_time(now, s.created);
    let title = match source {
        SnapshotSource::Agent => format!("#{:<3} {when}", s.n),
        SnapshotSource::Finished => format!("{} {when}", s.id),
    };
    ListViewItem {
        title,
        badge: Some((
            format!("[{}]", s.reason.as_str()),
            Style::default().fg(reason_color(s.reason, palette)),
        )),
        meta: Some(format!("{}f +{} −{}", s.files, s.added, s.removed)),
    }
}

/// Lays `labels` out left to right, wrapping into rows; returns rects (row-major) and rows used.
fn draw_buttons(
    buffer: &mut Buffer,
    area: Rect,
    y0: u16,
    labels: &[(String, Style)],
) -> (Vec<Rect>, u16) {
    let mut rects = Vec::new();
    let (mut x, mut y) = (area.x, y0);
    for (text, style) in labels {
        let w = text_width(text);
        if x > area.x && x + w > area.right() {
            x = area.x;
            y += 1;
        }
        if y >= area.bottom() || x + w > area.right() {
            rects.push(Rect::new(x, y, 0, 0));
            continue;
        }
        put_str(buffer, x, y, w, text, *style);
        rects.push(Rect::new(x, y, w, 1));
        x += w + 2;
    }
    (rects, y - y0 + 1)
}

fn render_file_selector(
    buffer: &mut Buffer,
    area: Rect,
    sel: &FileSelector,
    error: Option<&str>,
    palette: &Palette,
    hits: &mut ShepherdSnapshotsHits,
) {
    if area.width < 12 || area.height < 6 {
        return;
    }
    put_str(
        buffer,
        area.x,
        area.y,
        area.width,
        &format!("RESTAURAR ARCHIVOS DE {}", sel.id),
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    put_str(
        buffer,
        area.x,
        area.y + 1,
        area.width,
        &"─".repeat(area.width as usize),
        Style::default().fg(palette.surface_dim),
    );

    // Footer: hint/error line + buttons (wrapped).
    let checked = sel.files.iter().filter(|f| f.checked).count();
    let labels = [
        (
            format!("[ Restaurar {checked} seleccionado(s) (Enter) ]"),
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        ),
        (
            "[ Marcar/Desmarcar todos (a) ]".to_string(),
            Style::default().fg(palette.overlay0),
        ),
        (
            "[ Cancelar (Esc) ]".to_string(),
            Style::default().fg(palette.overlay0),
        ),
    ];
    // Probe the number of button rows without drawing (scratch buffer).
    let mut scratch = Buffer::empty(Rect::new(0, 0, area.width, 8));
    let (_, btn_rows) = draw_buttons(&mut scratch, Rect::new(0, 0, area.width, 8), 0, &labels);
    let footer_h = (btn_rows + 2).min(area.height.saturating_sub(3));
    let footer_y = area.bottom() - footer_h;
    let (msg, msg_style) = match error {
        Some(e) => (e.to_string(), Style::default().fg(palette.red)),
        None if sel.not_restorable > 0 => (
            format!(
                "Espacio: marcar | ↑/↓: mover | {} archivo(s) nuevo(s) no se restauran",
                sel.not_restorable
            ),
            Style::default().fg(palette.overlay0),
        ),
        None => (
            "Espacio: marcar | ↑/↓: mover | a: todos".to_string(),
            Style::default().fg(palette.overlay0),
        ),
    };
    put_str(buffer, area.x, footer_y, area.width, &msg, msg_style);
    let (rects, _) = draw_buttons(
        buffer,
        Rect::new(area.x, footer_y + 1, area.width, footer_h.saturating_sub(1)),
        footer_y + 1,
        &labels,
    );
    for (r, b) in rects.into_iter().zip([
        FileButton::Restore,
        FileButton::ToggleAll,
        FileButton::Cancel,
    ]) {
        if r.width > 0 {
            hits.file_buttons.push((r, b));
        }
    }

    // Rows
    let top = area.y + 2;
    let visible = footer_y.saturating_sub(top) as usize;
    if visible == 0 {
        return;
    }
    let start = if sel.cursor < visible {
        0
    } else {
        sel.cursor + 1 - visible
    };
    for (row, idx) in (start..sel.files.len().min(start + visible)).enumerate() {
        let f = &sel.files[idx];
        let y = top + row as u16;
        let on_cursor = idx == sel.cursor;
        let bg = if on_cursor {
            palette.surface0
        } else {
            palette.panel_bg
        };
        for x in area.x..area.right() {
            let c = &mut buffer[(x, y)];
            c.set_char(' ');
            c.set_bg(bg);
        }
        hits.file_rows
            .push((Rect::new(area.x, y, area.width, 1), idx));
        let mark = if f.checked { "[x]" } else { "[ ]" };
        let line = format!(
            "{} {mark} {} {}  +{} −{}",
            if on_cursor { "▸" } else { " " },
            status_letter(&f.status),
            f.path,
            f.added,
            f.removed
        );
        let color = if f.checked {
            palette.green
        } else {
            palette.text
        };
        put_str(
            buffer,
            area.x,
            y,
            area.width,
            &line,
            Style::default().fg(color).bg(bg),
        );
    }
}

fn detail_blocks(state: &ShepherdSnapshotsState, snap: &SnapshotInfo) -> Vec<DetailContentBlock> {
    let mut blocks = Vec::new();
    if let Some(b) = &state.busy {
        blocks.push(DetailContentBlock::Notice {
            text: format!("⏳ Trabajando… ({b})"),
            level: AlertLevel::Info,
        });
    }
    if let Some(e) = &state.error {
        blocks.push(DetailContentBlock::Notice {
            text: format!("Error: {e}"),
            level: AlertLevel::Warning,
        });
    }
    if let Some(n) = &state.notice {
        blocks.push(DetailContentBlock::Notice {
            text: n.clone(),
            level: AlertLevel::Success,
        });
    }

    blocks.push(DetailContentBlock::Heading(
        "RESUMEN (cambios sobre el snapshot anterior):".into(),
    ));
    match state.summary_for_selected() {
        None => blocks.push(DetailContentBlock::Paragraph("Cargando resumen…".into())),
        Some(sum) => {
            blocks.push(DetailContentBlock::Paragraph(format!(
                "{} archivo(s) cambiado(s): +{} −{}",
                sum.files.len(),
                sum.added,
                sum.removed
            )));
            let mut items: Vec<String> = sum
                .files
                .iter()
                .take(MAX_LISTED_FILES)
                .map(|f| {
                    if f.binary {
                        format!("{}  {} (binario)", status_letter(&f.status), f.path)
                    } else {
                        format!(
                            "{}  {}  +{} −{}",
                            status_letter(&f.status),
                            f.path,
                            f.added,
                            f.removed
                        )
                    }
                })
                .collect();
            if sum.files.len() > MAX_LISTED_FILES {
                items.push(format!("… y {} más", sum.files.len() - MAX_LISTED_FILES));
            }
            if !items.is_empty() {
                blocks.push(DetailContentBlock::BulletList(items));
            }
        }
    }
    if !snap.omitted.is_empty() {
        blocks.push(DetailContentBlock::Heading(
            "ARCHIVOS OMITIDOS (demasiado grandes):".into(),
        ));
        blocks.push(DetailContentBlock::BulletList(
            snap.omitted
                .iter()
                .map(|o| format!("{} ({}, {} bytes)", o.path, o.reason, o.size))
                .collect(),
        ));
    }

    match state.diff_for_selected() {
        None => {
            blocks.push(DetailContentBlock::Heading("DIFF:".into()));
            blocks.push(DetailContentBlock::Paragraph(
                "Pulsa «Ver diff vs anterior» (d) o «Ver diff vs actual» (D) para ver el patch."
                    .into(),
            ));
        }
        Some(d) => {
            let what = match d.mode {
                DiffMode::Previous => "anterior",
                DiffMode::Current => "estado actual",
            };
            blocks.push(DetailContentBlock::Heading(format!(
                "DIFF vs {what}: {} archivo(s), +{} −{}",
                d.files, d.added, d.removed
            )));
            if d.hidden > 0 {
                blocks.push(DetailContentBlock::Notice {
                    text: format!(
                        "Diff truncado: se muestran {} líneas, faltan {}. Usa `shepherd snapshot diff {} --full` para verlo entero.",
                        d.lines.len(),
                        d.hidden,
                        d.id
                    ),
                    level: AlertLevel::Warning,
                });
            }
            if d.lines.is_empty() {
                blocks.push(DetailContentBlock::Paragraph("(sin diferencias)".into()));
            } else {
                blocks.push(DetailContentBlock::Diff(d.lines.clone()));
            }
        }
    }
    blocks
}

pub fn render_snapshots_section(
    buffer: &mut Buffer,
    area: Rect,
    state: &ShepherdSnapshotsState,
    palette: &Palette,
) -> ShepherdSnapshotsHits {
    let mut hits = ShepherdSnapshotsHits::default();
    if area.width < 10 || area.height < 4 {
        return hits;
    }
    let wide_list_w = (area.width * 38 / 100).max(26);
    let items: Vec<ListViewItem> = state
        .current()
        .iter()
        .map(|s| list_item(s, state.source, state.now, palette))
        .collect();
    let tab_labels: Vec<String> = vec![
        if state.agent_id.is_empty() {
            "Del agente".to_string()
        } else {
            format!("De {}", state.agent_id)
        },
        "Finalizados".to_string(),
    ];
    let tab_refs: Vec<&str> = tab_labels.iter().map(String::as_str).collect();
    let active_tab = match state.source {
        SnapshotSource::Agent => 0,
        SnapshotSource::Finished => 1,
    };

    let split = split_panes(
        area,
        wide_list_w,
        compact_panel(
            state.has_sub_action() || state.detail_open,
            items.is_empty(),
        ),
    );
    hits.list_hits = render_list_view(
        buffer,
        split.list,
        &items,
        state.selected,
        state.list_scroll,
        Some((&tab_refs, active_tab)),
        Some(("Capturar ahora", "c")),
        palette,
    );
    let Some(detail_area) = split.detail else {
        return hits;
    };
    hits.detail_rect = Some(detail_area);
    if let Some(x) = split.divider_x {
        draw_divider(buffer, area, x, Style::default().fg(palette.surface_dim));
    }

    match &state.sub {
        SnapSub::Prune(form) => {
            hits.form_hits = Some(render_inline_form(
                buffer,
                detail_area,
                form,
                "Podar",
                "Cancelar",
                true,
                palette,
            ));
            return hits;
        }
        SnapSub::Files(sel) => {
            render_file_selector(
                buffer,
                detail_area,
                sel,
                state.error.as_deref(),
                palette,
                &mut hits,
            );
            return hits;
        }
        SnapSub::ConfirmFiles { id, paths } => {
            let msg = format!(
                "¿Restaurar {} archivo(s) desde {id}? Antes se guardará el estado actual como snapshot «pre-restore».",
                paths.len()
            );
            hits.stripe_hits = Some(render_confirm_stripe(
                buffer,
                detail_area,
                &msg,
                "Confirmar Restaurar",
                "Cancelar",
                true,
                palette,
            ));
            return hits;
        }
        SnapSub::ConfirmAll(id) => {
            let msg = format!(
                "¿Restaurar TODO el worktree desde {id}? Se sobrescriben los cambios locales; los archivos que no están en el snapshot se conservan. Antes se guardará el estado actual como «pre-restore»."
            );
            hits.stripe_hits = Some(render_confirm_stripe(
                buffer,
                detail_area,
                &msg,
                "Confirmar Restaurar Todo",
                "Cancelar",
                true,
                palette,
            ));
            return hits;
        }
        SnapSub::ConfirmDelete(id) => {
            let msg = format!(
                "¿Borrar el snapshot {id}? No se puede deshacer (los demás snapshots no cambian)."
            );
            hits.stripe_hits = Some(render_confirm_stripe(
                buffer,
                detail_area,
                &msg,
                "Confirmar Borrar",
                "Cancelar",
                true,
                palette,
            ));
            return hits;
        }
        SnapSub::None => {}
    }

    let Some(snap) = state.selected_snapshot() else {
        let (title, message, actions): (&str, String, Vec<(&str, Option<&str>)>) = match state
            .source
        {
            SnapshotSource::Agent => (
                "AÚN NO HAY SNAPSHOTS",
                "Aún no hay snapshots — se toman solos al quedar inactivo, o pulsa Capturar ahora."
                    .into(),
                vec![("Capturar ahora", Some("c")), ("Volver", Some("Esc"))],
            ),
            SnapshotSource::Finished => (
                "NO HAY SNAPSHOTS DE AGENTES FINALIZADOS",
                "Cuando un agente termina o se limpia, sus snapshots siguen aquí hasta que los podes.".into(),
                vec![("Volver", Some("Esc"))],
            ),
        };
        let mut msg = message;
        if let Some(b) = &state.busy {
            msg.push_str(&format!("\n⏳ Trabajando… ({b})"));
        }
        if let Some(e) = &state.error {
            msg.push_str(&format!("\nError: {e}"));
        }
        if let Some(n) = &state.notice {
            msg.push_str(&format!("\n{n}"));
        }
        hits.empty_hits = Some(render_empty_state(
            buffer,
            detail_area,
            title,
            &msg,
            &actions,
            palette,
        ));
        return hits;
    };

    let color = reason_color(snap.reason, palette);
    let header = DetailHeader {
        title: format!("SNAPSHOT: {}", snap.id),
        badges: vec![(
            format!("[{}]", snap.reason.as_str()),
            Style::default().fg(color),
        )],
        metadata_line: Some(format!(
            "{} UTC ({}) | Anterior: {} | {} arch. +{} −{}",
            format_utc(snap.created),
            rel_time(state.now, snap.created),
            snap.prev.as_deref().unwrap_or("—"),
            snap.files,
            snap.added,
            snap.removed
        )),
        actions: if state.agent_id.is_empty() {
            Vec::new()
        } else {
            vec![("Capturar ahora".to_string(), Some("c".to_string()))]
        },
    };
    let blocks = detail_blocks(state, snap);
    let buttons: Vec<(&str, &str, bool)> = SnapshotButton::ALL
        .iter()
        .map(|b| {
            let (l, s) = b.label();
            (l, s, *b == SnapshotButton::DiffPrevious)
        })
        .collect();
    let dh = render_detail_view(
        buffer,
        detail_area,
        &header,
        &blocks,
        state.detail_scroll,
        &buttons,
        palette,
    );
    hits.header_capture = dh.header_actions.first().map(|(r, _)| *r);
    hits.detail_hits = Some(dh);
    hits
}
