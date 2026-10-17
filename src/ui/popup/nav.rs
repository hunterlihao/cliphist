//! 列表导航与命中测试。
//!
//! 这一层**不碰 HWND**：翻页、选择、可见行数、以及“点在哪一行”的几何计算，
//! 全部是纯函数，可以直接单测。

use super::*;

/// 列表导航状态（纯数据，便于单测）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListNav {
    pub total: usize,
    pub selected: usize,
    /// 顶部行索引
    pub top: usize,
}

impl ListNav {
    pub fn new(total: usize) -> Self {
        Self {
            total,
            selected: 0,
            top: 0,
        }
    }

    /// 可视行数。
    pub fn visible(&self, m: &Metrics) -> usize {
        m.visible_rows() as usize
    }

    pub fn max_top(&self, m: &Metrics) -> usize {
        self.total.saturating_sub(self.visible(m))
    }

    pub fn clamp(&mut self, m: &Metrics) {
        if self.total == 0 {
            self.selected = 0;
            self.top = 0;
            return;
        }
        self.selected = self.selected.min(self.total - 1);
        self.top = self.top.min(self.max_top(m));
        self.ensure_visible(m);
    }

    pub fn ensure_visible(&mut self, m: &Metrics) {
        let vis = self.visible(m);
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + vis {
            self.top = self.selected + 1 - vis;
        }
        self.top = self.top.min(self.max_top(m));
    }

    pub fn select(&mut self, m: &Metrics, idx: usize) {
        self.selected = idx.min(self.total.saturating_sub(1));
        self.ensure_visible(m);
    }

    pub fn move_by(&mut self, m: &Metrics, delta: i32) {
        if self.total == 0 {
            return;
        }
        let cur = self.selected as i32;
        let next = (cur + delta).clamp(0, self.total as i32 - 1);
        self.selected = next as usize;
        self.ensure_visible(m);
    }

    pub fn page(&mut self, m: &Metrics, pages: i32) {
        let step = self.visible(m) as i32 * pages;
        self.move_by(m, step);
    }

    pub fn scroll_rows(&mut self, m: &Metrics, rows: i32) {
        let next = (self.top as i32 + rows).clamp(0, self.max_top(m) as i32);
        self.top = next as usize;
        // 滚动后让选择落在可视区内（不改变选择值本身）
        let vis = self.visible(m);
        if self.selected < self.top {
            self.selected = self.top;
        } else if self.selected >= self.top + vis {
            self.selected = (self.top + vis - 1).min(self.total.saturating_sub(1));
        }
    }

    /// 当前可视区间的起止（左闭右开）。
    pub fn range(&self, m: &Metrics) -> (usize, usize) {
        let start = self.top.min(self.total);
        let end = (self.top + self.visible(m)).min(self.total);
        (start, end)
    }
}

/// 点击 y 坐标 → 行号。
pub fn row_at(m: &Metrics, top: usize, y: i32) -> Option<usize> {
    if y < m.list_top() {
        return None;
    }
    let list_h = m.list_height();
    if y >= m.list_top() + list_h {
        return None;
    }
    let idx = top + ((y - m.list_top()) / m.row_h) as usize;
    Some(idx)
}

/// 单行矩形。
pub fn row_rect(m: &Metrics, top: usize, index: usize) -> RECT {
    let y0 = m.list_top() + (index as i32 - top as i32) * m.row_h;
    rect(0, y0, m.width, y0 + m.row_h)
}

/// 键盘动作（由窗口过程翻译成消息）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    None,
    /// 选中第 N 条（Ctrl+1..9）
    PickIndex(usize),
    Choose {
        clipboard_only: bool,
    },
    Context,
    Pin,
    Delete,
    Close,
    Consumed,
}

/// 键盘处理（纯逻辑，可单测）：返回动作 + 是否需要重绘。
pub fn handle_key(
    nav: &mut ListNav,
    m: &Metrics,
    vk: usize,
    ctrl: bool,
    shift: bool,
) -> (KeyAction, bool) {
    match vk {
        VK_ESCAPE => (KeyAction::Close, false),
        VK_RETURN => (
            KeyAction::Choose {
                clipboard_only: shift,
            },
            false,
        ),
        VK_UP => {
            nav.move_by(m, -1);
            (KeyAction::Consumed, true)
        }
        VK_DOWN => {
            nav.move_by(m, 1);
            (KeyAction::Consumed, true)
        }
        VK_PRIOR => {
            nav.page(m, -1);
            (KeyAction::Consumed, true)
        }
        VK_NEXT => {
            nav.page(m, 1);
            (KeyAction::Consumed, true)
        }
        VK_HOME => {
            nav.select(m, 0);
            (KeyAction::Consumed, true)
        }
        VK_END => {
            nav.select(m, nav.total.saturating_sub(1));
            (KeyAction::Consumed, true)
        }
        0x31..=0x39 if ctrl => {
            // Ctrl+1..9 直选
            let idx = vk - 0x31;
            if idx < nav.total {
                nav.select(m, idx);
                (KeyAction::PickIndex(idx), true)
            } else {
                (KeyAction::Consumed, false)
            }
        }
        0x50 if ctrl => (KeyAction::Pin, false), // Ctrl+P 置顶
        0x44 if ctrl => (KeyAction::Delete, false), // Ctrl+D 删除
        _ => (KeyAction::None, false),
    }
}
