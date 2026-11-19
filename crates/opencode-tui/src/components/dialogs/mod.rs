use ratatui::layout::Rect;
use std::cell::RefCell;

mod agent_select;
mod alert;
mod command_palette;
mod confirm;
mod fork;
mod help;
mod mcp;
mod model_select;
mod prompt_stash;
mod provider;
mod session_export;
mod session_list;
mod session_rename;
mod skill_list;
mod status;
mod subagent;
mod tag;
mod theme_list;
mod timeline;

const DIALOG_VIEWPORT_MARGIN_X: u16 = 2;
const DIALOG_VIEWPORT_MARGIN_Y: u16 = 1;
const DIALOG_INNER_PADDING_X: u16 = 1;
const DIALOG_INNER_PADDING_Y: u16 = 0;

pub(super) fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let viewport = inset_rect(area, DIALOG_VIEWPORT_MARGIN_X, DIALOG_VIEWPORT_MARGIN_Y);
    if viewport.width == 0 || viewport.height == 0 {
        return area;
    }

    let dialog_width = width.min(viewport.width).max(1);
    let dialog_height = height.min(viewport.height).max(1);

    Rect {
        x: viewport.x + viewport.width.saturating_sub(dialog_width) / 2,
        y: viewport.y + viewport.height.saturating_sub(dialog_height) / 2,
        width: dialog_width,
        height: dialog_height,
    }
}

pub(super) fn dialog_inner(area: Rect) -> Rect {
    inset_rect(area, DIALOG_INNER_PADDING_X, DIALOG_INNER_PADDING_Y)
}

/// ダイアログの描画ジオメトリを記録し、クリック位置を項目インデックスへ変換する。
///
/// Dialog `render` methods borrow `&self`, so the geometry is kept behind
/// interior mutability. A click inside the dialog frame is consumed (and, when
/// it lands on a list row, translated into that row's item index); a click on
/// the surrounding backdrop is left for the app to close the top dialog.
#[derive(Default, Debug)]
pub(super) struct DialogClickMap {
    area: RefCell<Option<Rect>>,
    list: RefCell<Option<(Rect, usize, usize)>>,
}

impl DialogClickMap {
    /// Record the full frame of the dialog, borders included.
    pub(super) fn set_area(&self, area: Rect) {
        *self.area.borrow_mut() = Some(area);
    }

    /// Record the list widget rectangle, its rendered scroll offset and item count.
    pub(super) fn set_list(&self, area: Rect, offset: usize, count: usize) {
        *self.list.borrow_mut() = Some((area, offset, count));
    }

    pub(super) fn contains(&self, col: u16, row: u16) -> bool {
        self.area.borrow().is_some_and(|area| {
            col >= area.x
                && col < area.x.saturating_add(area.width)
                && row >= area.y
                && row < area.y.saturating_add(area.height)
        })
    }

    pub(super) fn item_at(&self, col: u16, row: u16) -> Option<usize> {
        let (area, offset, count) = (*self.list.borrow())?;
        if col < area.x
            || col >= area.x.saturating_add(area.width)
            || row < area.y
            || row >= area.y.saturating_add(area.height)
        {
            return None;
        }
        let index = offset + usize::from(row - area.y);
        (index < count).then_some(index)
    }
}

fn inset_rect(area: Rect, padding_x: u16, padding_y: u16) -> Rect {
    if area.width == 0 || area.height == 0 {
        return area;
    }

    let twice_x = padding_x.saturating_mul(2);
    let twice_y = padding_y.saturating_mul(2);
    if area.width <= twice_x || area.height <= twice_y {
        return area;
    }

    Rect {
        x: area.x.saturating_add(padding_x),
        y: area.y.saturating_add(padding_y),
        width: area.width.saturating_sub(twice_x),
        height: area.height.saturating_sub(twice_y),
    }
}

pub use agent_select::{Agent, AgentSelectDialog};
pub use alert::AlertDialog;
pub use command_palette::CommandPalette;
pub use confirm::ConfirmDialog;
pub use fork::{ForkDialog, ForkEntry};
pub use help::HelpDialog;
pub use mcp::{McpDialog, McpItem};
pub use model_select::{Model, ModelSelectDialog};
pub use prompt_stash::{PromptStashDialog, StashItem};
pub use provider::{Provider, ProviderDialog, ProviderStatus};
pub use session_export::SessionExportDialog;
pub use session_list::{DeleteState as SessionDeleteState, SessionItem, SessionListDialog};
pub use session_rename::SessionRenameDialog;
pub use skill_list::SkillListDialog;
pub use status::{StatusDialog, StatusLine};
pub use subagent::{SubagentDialog, SubagentInfo, SubagentMessage};
pub use tag::{Tag, TagDialog};
pub use theme_list::{ThemeListDialog, ThemeOption};
pub use timeline::{TimelineDialog, TimelineEntry};

#[cfg(test)]
mod click_map_tests {
    use super::*;

    #[test]
    fn click_map_distinguishes_items_from_dialog_backdrop() {
        let map = DialogClickMap::default();
        let frame = Rect::new(10, 4, 40, 12);
        let list = Rect::new(12, 6, 36, 8);
        map.set_area(frame);
        map.set_list(list, 0, 3);

        assert_eq!(map.item_at(12, 6), Some(0));
        assert_eq!(map.item_at(12, 8), Some(2));
        assert_eq!(map.item_at(12, 9), None);
        assert_eq!(map.item_at(11, 6), None);
        assert!(map.contains(10, 4));
        assert!(!map.contains(9, 4));
        assert!(!map.contains(50, 4));
    }

    #[test]
    fn click_map_accounts_for_the_rendered_scroll_offset() {
        let map = DialogClickMap::default();
        map.set_area(Rect::new(0, 0, 20, 10));
        map.set_list(Rect::new(1, 2, 18, 3), 5, 9);

        assert_eq!(map.item_at(1, 2), Some(5));
        assert_eq!(map.item_at(1, 4), Some(7));
        assert_eq!(map.item_at(1, 5), None);
    }
}
