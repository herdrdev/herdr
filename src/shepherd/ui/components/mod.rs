pub mod confirm_stripe;
pub mod detail_view;
pub mod empty_state;
pub mod inline_form;
pub mod list_view;
pub mod status_bar;

#[allow(unused_imports)]
pub use confirm_stripe::{render_confirm_stripe, ConfirmStripeHits};
#[allow(unused_imports)]
pub use detail_view::{
    render_detail_view, AlertLevel, DetailContentBlock, DetailHeader, DetailViewHits,
};
#[allow(unused_imports)]
pub use empty_state::{render_empty_state, EmptyStateHits};
#[allow(unused_imports)]
pub use inline_form::{render_inline_form, FormField, InlineFormHits, InlineFormState};
#[allow(unused_imports)]
pub use list_view::{put_str, render_list_view, ListViewHits, ListViewItem};
#[allow(unused_imports)]
pub use status_bar::render_status_bar;

use ratatui::{buffer::Buffer, layout::Rect, style::Style};

/// Below this width the list/detail split collapses to a single column.
pub const COMPACT_WIDTH: u16 = 70;

/// What a tab shows in the single-column (compact) layout besides its list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactPanel {
    /// Only the list.
    None,
    /// A form/confirmation replaces the list (full area).
    Replace,
    /// Empty list: header stays, empty state fills the rest.
    Below,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneSplit {
    pub list: Rect,
    pub detail: Option<Rect>,
    pub divider_x: Option<u16>,
}

/// One layout rule for every tab: wide = list | detail, compact = one column
/// where forms, confirmations and empty states are never hidden.
pub fn split_panes(area: Rect, wide_list_w: u16, compact: CompactPanel) -> PaneSplit {
    if area.width >= COMPACT_WIDTH {
        let lw = wide_list_w.min(area.width.saturating_sub(24));
        let list = Rect::new(area.x, area.y, lw, area.height);
        let dx = list.right();
        let detail = Rect::new(
            dx + 1,
            area.y,
            area.right().saturating_sub(dx + 1),
            area.height,
        );
        return PaneSplit {
            list,
            detail: Some(detail),
            divider_x: Some(dx),
        };
    }
    match compact {
        CompactPanel::None => PaneSplit {
            list: area,
            detail: None,
            divider_x: None,
        },
        CompactPanel::Replace => PaneSplit {
            list: Rect::new(area.x, area.y, 0, 0),
            detail: Some(area),
            divider_x: None,
        },
        CompactPanel::Below => {
            let top = 3.min(area.height);
            PaneSplit {
                list: area,
                detail: Some(Rect::new(
                    area.x,
                    area.y + top,
                    area.width,
                    area.height - top,
                )),
                divider_x: None,
            }
        }
    }
}

pub fn compact_panel(sub_active: bool, list_empty: bool) -> CompactPanel {
    if sub_active {
        CompactPanel::Replace
    } else if list_empty {
        CompactPanel::Below
    } else {
        CompactPanel::None
    }
}

pub fn draw_divider(buffer: &mut Buffer, area: Rect, x: u16, style: Style) {
    for y in area.y..area.bottom() {
        buffer[(x, y)].set_char('│').set_style(style);
    }
}
