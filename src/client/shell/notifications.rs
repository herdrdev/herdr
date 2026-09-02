use super::*;
use ratatui::{
    style::Color,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

fn render_mobile_notice_banner(
    buffer: &mut Buffer,
    area: Rect,
    title: &str,
    body: Option<&str>,
    dot_color: Color,
    offset_for_warning: bool,
    palette: &Palette,
) -> Rect {
    if area.is_empty() {
        return Rect::default();
    }
    let warning_offset = u16::from(offset_for_warning);
    let y = area.y
        + area
            .height
            .saturating_sub(1u16.saturating_add(warning_offset));
    let rect = Rect::new(area.x, y, area.width, 1);
    let background = palette.surface0;
    Clear.render(rect, buffer);
    buffer.set_style(rect, Style::default().bg(background));
    let mut x = rect.x;
    for (text, style) in [
        (" ", Style::default().bg(background)),
        ("●", Style::default().fg(dot_color).bg(background)),
        (" ", Style::default().bg(background)),
        (
            title,
            Style::default()
                .fg(palette.text)
                .bg(background)
                .add_modifier(Modifier::BOLD),
        ),
    ] {
        x = super::render::put_segment(buffer, x, rect.y, rect.right(), text, style);
    }
    if let Some(body) = body.filter(|body| !body.is_empty()) {
        x = super::render::put_segment(
            buffer,
            x,
            rect.y,
            rect.right(),
            " · ",
            Style::default().fg(palette.overlay0).bg(background),
        );
        super::render::put_text(
            buffer,
            x,
            rect.y,
            rect.right().saturating_sub(x),
            body,
            Style::default().fg(palette.overlay0).bg(background),
        );
    }
    rect
}

pub(super) fn render_mobile_notification_banner(
    buffer: &mut Buffer,
    area: Rect,
    notification: &ClientVisibleNotification,
    offset_for_warning: bool,
    palette: &Palette,
) -> Rect {
    let event = &notification.event;
    let title = match event.kind {
        SemanticNotificationKind::NeedsAttention => event
            .title
            .strip_suffix(" needs attention")
            .map(|agent| format!("{agent} waiting"))
            .unwrap_or_else(|| event.title.clone()),
        SemanticNotificationKind::Finished => event
            .title
            .strip_suffix(" finished")
            .map(|agent| format!("{agent} done"))
            .unwrap_or_else(|| event.title.clone()),
        SemanticNotificationKind::UpdateInstalled => "update ready".to_owned(),
        SemanticNotificationKind::Custom => event.title.clone(),
    };
    let dot_color = match event.kind {
        SemanticNotificationKind::NeedsAttention => palette.red,
        SemanticNotificationKind::Finished => palette.blue,
        SemanticNotificationKind::UpdateInstalled | SemanticNotificationKind::Custom => {
            palette.accent
        }
    };
    render_mobile_notice_banner(
        buffer,
        area,
        &title,
        event.body.as_deref(),
        dot_color,
        offset_for_warning,
        palette,
    )
}

pub(super) fn render_mobile_endpoint_notice_banner(
    buffer: &mut Buffer,
    area: Rect,
    notice: &ClientVisibleEndpointNotice,
    offset_for_warning: bool,
    palette: &Palette,
) -> Rect {
    render_mobile_notice_banner(
        buffer,
        area,
        &notice.title,
        Some(&notice.body),
        palette.red,
        offset_for_warning,
        palette,
    )
}

fn render_notification_card(
    buffer: &mut Buffer,
    area: Rect,
    title: &str,
    body: &str,
    position: crate::config::ToastHerdrPosition,
    top_offset: u16,
    dot_color: Color,
    palette: &Palette,
) -> Rect {
    if area.is_empty() {
        return Rect::default();
    }
    let content_width = unicode_width::UnicodeWidthStr::width(title)
        .max(unicode_width::UnicodeWidthStr::width(body))
        .saturating_add(6);
    let width = u16::try_from(content_width)
        .unwrap_or(u16::MAX)
        .min(area.width);
    let height: u16 = if body.is_empty() { 3 } else { 4 }.min(area.height);
    let x = match position {
        crate::config::ToastHerdrPosition::TopLeft
        | crate::config::ToastHerdrPosition::BottomLeft => area.x,
        crate::config::ToastHerdrPosition::TopRight
        | crate::config::ToastHerdrPosition::BottomRight => area.right().saturating_sub(width),
    };
    let max_y = area.bottom().saturating_sub(height).max(area.y);
    let y = match position {
        crate::config::ToastHerdrPosition::TopLeft
        | crate::config::ToastHerdrPosition::TopRight => area.y.saturating_add(top_offset),
        crate::config::ToastHerdrPosition::BottomLeft
        | crate::config::ToastHerdrPosition::BottomRight => area
            .bottom()
            .saturating_sub(height.saturating_add(top_offset)),
    }
    .clamp(area.y, max_y);
    let rect = Rect::new(x, y, width, height);
    Clear.render(rect, buffer);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.overlay0))
        .style(Style::default().bg(palette.panel_bg));
    let inner = block.inner(rect);
    block.render(rect, buffer);
    Paragraph::new(Line::from(vec![
        Span::styled("●", Style::default().fg(dot_color)),
        Span::raw(" "),
        Span::styled(
            title,
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        ),
    ]))
    .render(Rect::new(inner.x, inner.y, inner.width, 1), buffer);
    if !body.is_empty() && inner.height > 1 {
        Paragraph::new(Line::from(Span::styled(
            body,
            Style::default().fg(palette.overlay0),
        )))
        .render(
            Rect::new(
                inner.x.saturating_add(2),
                inner.y + 1,
                inner.width.saturating_sub(2),
                1,
            ),
            buffer,
        );
    }
    rect
}

pub(super) fn render_visible_notification(
    buffer: &mut Buffer,
    area: Rect,
    notification: &ClientVisibleNotification,
    default_position: crate::config::ToastHerdrPosition,
    top_offset: u16,
    palette: &Palette,
) -> Rect {
    let event = &notification.event;
    let dot_color = match event.kind {
        SemanticNotificationKind::NeedsAttention => palette.red,
        SemanticNotificationKind::Finished => palette.blue,
        SemanticNotificationKind::UpdateInstalled | SemanticNotificationKind::Custom => {
            palette.accent
        }
    };
    render_notification_card(
        buffer,
        area,
        &event.title,
        event.body.as_deref().unwrap_or_default(),
        event.position.unwrap_or(default_position),
        top_offset,
        dot_color,
        palette,
    )
}

pub(super) fn render_endpoint_notice(
    buffer: &mut Buffer,
    area: Rect,
    notice: &ClientVisibleEndpointNotice,
    top_offset: u16,
    palette: &Palette,
) -> Rect {
    render_notification_card(
        buffer,
        area,
        &notice.title,
        &notice.body,
        crate::config::ToastHerdrPosition::TopRight,
        top_offset,
        match notice.key.kind {
            ClientEndpointNoticeKind::Unsupported | ClientEndpointNoticeKind::Rejected => {
                palette.red
            }
            ClientEndpointNoticeKind::Timeout | ClientEndpointNoticeKind::Unavailable => {
                palette.yellow
            }
        },
        palette,
    )
}

impl ClientShellState {
    pub(super) fn focus_visible_notification(&mut self, outcome: &mut ClientShellInput) {
        let Some(notification) = self.visible_notification.take() else {
            return;
        };
        outcome.repaint = true;
        if let Some(pane_id) = notification.event.pane_id {
            self.push_endpoint_method(
                crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget { pane_id }),
                outcome,
            );
        }
    }

    pub(crate) fn receive_notification(
        &mut self,
        event: SemanticNotification,
        now: std::time::Instant,
    ) -> (Vec<ClientShellNotificationEffect>, bool) {
        let delay = if event.kind == SemanticNotificationKind::Custom {
            0
        } else {
            self.config.toast_delay_seconds
        };
        let deadline = now
            .checked_add(std::time::Duration::from_secs(delay))
            .unwrap_or(now);
        let cleared_visible = event.pane_id.as_deref().is_some_and(|pane_id| {
            self.visible_notification
                .as_ref()
                .is_some_and(|visible| visible.event.pane_id.as_deref() == Some(pane_id))
        });
        if let Some(pane_id) = event.pane_id.as_deref() {
            self.pending_notifications
                .retain(|pending| pending.event.pane_id.as_deref() != Some(pane_id));
            if cleared_visible {
                self.visible_notification = None;
            }
        }
        self.pending_notifications.push(ClientPendingNotification {
            event,
            deadline,
            validate_state: delay > 0,
        });
        let (effects, repaint) = self.tick_notifications(now);
        (effects, repaint || cleared_visible)
    }

    pub(crate) fn tick_notifications(
        &mut self,
        now: std::time::Instant,
    ) -> (Vec<ClientShellNotificationEffect>, bool) {
        let mut repaint = false;
        if self
            .visible_notification
            .as_ref()
            .is_some_and(|visible| now >= visible.deadline)
        {
            self.visible_notification = None;
            repaint = true;
        }
        if self
            .visible_endpoint_notice
            .as_ref()
            .is_some_and(|visible| now >= visible.deadline)
        {
            self.visible_endpoint_notice = None;
            repaint = true;
        }

        let pending = std::mem::take(&mut self.pending_notifications);
        let mut effects = Vec::new();
        for pending in pending {
            if pending.deadline > now {
                self.pending_notifications.push(pending);
                continue;
            }
            if pending.validate_state && !self.notification_still_current(&pending.event) {
                continue;
            }
            let target_active = self.notification_target_is_active(&pending.event);
            let suppress_external = target_active && self.outer_focused != Some(false);
            if let Some(sound) = pending.event.sound {
                let suppress_sound =
                    pending.event.kind == SemanticNotificationKind::Finished && suppress_external;
                if !suppress_sound {
                    effects.push(ClientShellNotificationEffect::Sound {
                        sound: match sound {
                            SemanticNotificationSound::Done => crate::sound::Sound::Done,
                            SemanticNotificationSound::Request => crate::sound::Sound::Request,
                        },
                        agent: pending.event.agent.clone(),
                    });
                }
            }

            match self.config.toast_delivery {
                crate::config::ToastDelivery::Off => {}
                crate::config::ToastDelivery::Herdr if !target_active => {
                    let duration = match pending.event.kind {
                        SemanticNotificationKind::NeedsAttention => 8,
                        SemanticNotificationKind::Finished => 5,
                        SemanticNotificationKind::UpdateInstalled => 3,
                        SemanticNotificationKind::Custom => 5,
                    };
                    self.visible_notification = Some(ClientVisibleNotification {
                        event: pending.event,
                        deadline: now + std::time::Duration::from_secs(duration),
                    });
                    repaint = true;
                }
                crate::config::ToastDelivery::Herdr => {}
                crate::config::ToastDelivery::Terminal if !suppress_external => {
                    effects.push(ClientShellNotificationEffect::Terminal {
                        title: pending.event.title,
                        body: pending.event.body,
                    });
                }
                crate::config::ToastDelivery::System if !suppress_external => {
                    effects.push(ClientShellNotificationEffect::System {
                        title: pending.event.title,
                        body: pending.event.body,
                    });
                }
                crate::config::ToastDelivery::Terminal | crate::config::ToastDelivery::System => {}
            }
        }
        (effects, repaint)
    }

    fn notification_target_is_active(&self, event: &SemanticNotification) -> bool {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return false;
        };
        if let Some(tab_id) = event.tab_id.as_deref() {
            return snapshot.focused_tab_id.as_deref() == Some(tab_id);
        }
        event.workspace_id.as_deref().is_some_and(|workspace_id| {
            snapshot.focused_workspace_id.as_deref() == Some(workspace_id)
        })
    }

    fn notification_still_current(&self, event: &SemanticNotification) -> bool {
        let Some(pane_id) = event.pane_id.as_deref() else {
            return true;
        };
        let Some(agent) = self.snapshot.as_deref().and_then(|snapshot| {
            snapshot
                .agents
                .iter()
                .find(|agent| agent.pane_id == pane_id)
        }) else {
            return false;
        };
        match event.kind {
            SemanticNotificationKind::NeedsAttention => {
                agent.agent_status == crate::api::schema::AgentStatus::Blocked
            }
            SemanticNotificationKind::Finished => matches!(
                agent.agent_status,
                crate::api::schema::AgentStatus::Idle | crate::api::schema::AgentStatus::Done
            ),
            SemanticNotificationKind::UpdateInstalled | SemanticNotificationKind::Custom => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notification() -> ClientVisibleNotification {
        ClientVisibleNotification {
            event: SemanticNotification {
                kind: SemanticNotificationKind::Custom,
                title: "notice".into(),
                body: None,
                sound: None,
                agent: None,
                workspace_id: None,
                tab_id: None,
                pane_id: None,
                position: None,
            },
            deadline: std::time::Instant::now(),
        }
    }

    #[test]
    fn mobile_notification_is_a_bottom_banner_with_released_title() {
        let palette = crate::app::client_palette_from_config(&Config::default());
        let mut notification = notification();
        notification.event.kind = SemanticNotificationKind::NeedsAttention;
        notification.event.title = "pi needs attention".into();
        notification.event.body = Some("workspace · tab 1".into());
        let area = Rect::new(0, 0, 44, 20);
        let mut buffer = Buffer::empty(area);
        for cell in &mut buffer.content {
            cell.set_symbol("X");
        }
        let rect =
            render_mobile_notification_banner(&mut buffer, area, &notification, true, &palette);
        assert_eq!(rect, Rect::new(0, 18, 44, 1));
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("pi waiting"));
        assert!(text.contains("workspace · tab 1"));
        assert!(buffer.content[18 * 44..19 * 44]
            .iter()
            .all(|cell| cell.symbol() != "X"));
    }

    #[test]
    fn notification_rect_stays_inside_short_nonzero_area() {
        let palette = crate::app::client_palette_from_config(&Config::default());
        for height in [1, 2] {
            let area = Rect::new(3, 4, 8, height);
            for position in [
                crate::config::ToastHerdrPosition::TopRight,
                crate::config::ToastHerdrPosition::BottomRight,
            ] {
                let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 10));
                let rect = render_visible_notification(
                    &mut buffer,
                    area,
                    &notification(),
                    position,
                    1,
                    &palette,
                );
                assert!(rect.y >= area.y);
                assert!(rect.bottom() <= area.bottom());
            }
        }
    }
}
