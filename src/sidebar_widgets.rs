use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::api::schema::{SidebarWidgetInfo, SidebarWidgetSlot};

pub(crate) const MAX_SIDEBAR_WIDGETS: usize = 8;
pub(crate) const MAX_SIDEBAR_WIDGET_LINES: usize = 4;
pub(crate) const MAX_SIDEBAR_WIDGET_TEXT_CHARS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SidebarWidgetEntry {
    title: String,
    right: Option<String>,
    lines: Vec<String>,
    expires_at: Option<Instant>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SidebarWidgetStore {
    entries: BTreeMap<(SidebarWidgetSlot, String), SidebarWidgetEntry>,
}

pub(crate) fn sanitize_widget_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_SIDEBAR_WIDGET_TEXT_CHARS));
    let mut prev_space = false;
    for ch in raw.chars() {
        let mapped = if ch == '\n' || ch == '\r' || ch == '\t' {
            ' '
        } else if ch.is_control() {
            continue;
        } else {
            ch
        };
        if mapped == ' ' {
            if prev_space || out.is_empty() {
                continue;
            }
            prev_space = true;
        } else {
            prev_space = false;
        }
        out.push(mapped);
        if out.chars().count() >= MAX_SIDEBAR_WIDGET_TEXT_CHARS {
            break;
        }
    }
    out.trim_end().to_owned()
}

impl SidebarWidgetStore {
    pub(crate) fn report(
        &mut self,
        slot: SidebarWidgetSlot,
        source: String,
        title: String,
        right: Option<String>,
        lines: Vec<String>,
        ttl: Option<Duration>,
        now: Instant,
    ) -> Result<bool, &'static str> {
        let title = sanitize_widget_text(&title);
        if title.is_empty() {
            return Err("sidebar widget title must not be empty");
        }
        let right = right
            .map(|value| sanitize_widget_text(&value))
            .filter(|value| !value.is_empty());
        let lines: Vec<String> = lines
            .into_iter()
            .take(MAX_SIDEBAR_WIDGET_LINES)
            .map(|line| sanitize_widget_text(&line))
            .collect();
        let key = (slot, source);
        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_SIDEBAR_WIDGETS {
            return Err("sidebar widget limit reached");
        }
        let expires_at = ttl.and_then(|ttl| now.checked_add(ttl));
        let entry = SidebarWidgetEntry {
            title,
            right,
            lines,
            expires_at,
        };
        if self.entries.get(&key) == Some(&entry) {
            return Ok(false);
        }
        self.entries.insert(key, entry);
        Ok(true)
    }

    pub(crate) fn clear(&mut self, slot: SidebarWidgetSlot, source: &str) -> bool {
        self.entries.remove(&(slot, source.to_owned())).is_some()
    }

    pub(crate) fn next_expiry(&self) -> Option<Instant> {
        self.entries
            .values()
            .filter_map(|entry| entry.expires_at)
            .min()
    }

    pub(crate) fn expire_at(&mut self, now: Instant) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|_, entry| entry.expires_at.is_none_or(|deadline| deadline > now));
        self.entries.len() != before
    }

    pub(crate) fn snapshot(&self) -> Vec<SidebarWidgetInfo> {
        self.entries
            .iter()
            .map(|((slot, source), entry)| SidebarWidgetInfo {
                source: source.clone(),
                slot: *slot,
                title: entry.title.clone(),
                right: entry.right.clone(),
                lines: entry.lines.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_update_clear_and_line_cap() {
        let mut store = SidebarWidgetStore::default();
        let now = Instant::now();

        let changed = store
            .report(
                SidebarWidgetSlot::Top,
                "drover".into(),
                "drover".into(),
                Some("2/5".into()),
                vec![
                    "line 1".into(),
                    "line 2".into(),
                    "line 3".into(),
                    "line 4".into(),
                    "line 5 should be dropped".into(),
                ],
                None,
                now,
            )
            .unwrap();
        assert!(changed);

        let widgets = store.snapshot();
        assert_eq!(widgets.len(), 1);
        assert_eq!(widgets[0].source, "drover");
        assert_eq!(widgets[0].title, "drover");
        assert_eq!(widgets[0].right.as_deref(), Some("2/5"));
        assert_eq!(widgets[0].lines.len(), MAX_SIDEBAR_WIDGET_LINES);
        assert_eq!(widgets[0].lines[3], "line 4");

        // Identical report is a no-op.
        let changed = store
            .report(
                SidebarWidgetSlot::Top,
                "drover".into(),
                "drover".into(),
                Some("2/5".into()),
                vec![
                    "line 1".into(),
                    "line 2".into(),
                    "line 3".into(),
                    "line 4".into(),
                ],
                None,
                now,
            )
            .unwrap();
        assert!(!changed);

        assert!(store.clear(SidebarWidgetSlot::Top, "drover"));
        assert!(!store.clear(SidebarWidgetSlot::Top, "drover"));
        assert!(store.snapshot().is_empty());
    }

    #[test]
    fn ttl_expiry_removes_due_widgets_only() {
        let mut store = SidebarWidgetStore::default();
        let now = Instant::now();

        store
            .report(
                SidebarWidgetSlot::Top,
                "short".into(),
                "short".into(),
                None,
                vec!["a".into()],
                Some(Duration::from_secs(5)),
                now,
            )
            .unwrap();
        store
            .report(
                SidebarWidgetSlot::Top,
                "long".into(),
                "long".into(),
                None,
                vec!["b".into()],
                Some(Duration::from_secs(30)),
                now,
            )
            .unwrap();

        assert_eq!(store.next_expiry(), Some(now + Duration::from_secs(5)));
        assert!(!store.expire_at(now + Duration::from_secs(4)));
        assert_eq!(store.snapshot().len(), 2);

        assert!(store.expire_at(now + Duration::from_secs(5)));
        let remaining = store.snapshot();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].source, "long");
        assert_eq!(store.next_expiry(), Some(now + Duration::from_secs(30)));
    }

    #[test]
    fn enforces_widget_count_limit_and_non_empty_title() {
        let mut store = SidebarWidgetStore::default();
        let now = Instant::now();

        assert!(store
            .report(
                SidebarWidgetSlot::Top,
                "bad".into(),
                "   \n\t ".into(),
                None,
                vec![],
                None,
                now,
            )
            .is_err());

        for idx in 0..MAX_SIDEBAR_WIDGETS {
            store
                .report(
                    SidebarWidgetSlot::Top,
                    format!("src-{idx}"),
                    format!("title-{idx}"),
                    None,
                    vec![],
                    None,
                    now,
                )
                .unwrap();
        }
        assert!(store
            .report(
                SidebarWidgetSlot::Top,
                "overflow".into(),
                "overflow".into(),
                None,
                vec![],
                None,
                now,
            )
            .is_err());
    }
}
