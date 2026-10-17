//! 绘制层：搜索行、列表行、空态、滚动条、页脚，以及标题栏的绘制。
//!
//! 这里只做 GDI 与排版，不做状态决策 —— 命中测试在 `nav`，按钮命中在 `chrome`。

use super::*;

impl PopupState {
    pub(super) fn paint(&mut self, target: HDC) {
        let mut rc = rect(0, 0, 0, 0);
        unsafe { GetClientRect(self.hwnd, &mut rc) };
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        if w <= 0 || h <= 0 {
            return;
        }

        let need_new = match &self.mem {
            Some(mem) => mem.size() != (w, h),
            None => true,
        };
        if need_new {
            match &mut self.mem {
                Some(mem) => {
                    mem.resize(w, h);
                }
                None => self.mem = MemDc::new(w, h),
            }
        }

        // 先把内存 DC 取出来（take），这样在 render 里可以自由使用 &mut self，
        // 画完再放回去 —— 直接持有 &self.mem 会和 &mut self 冲突。
        let Some(mut mem) = self.mem.take() else {
            // 内存 DC 建不出来（极端情况）就直接画到屏幕，宁可闪也别白屏。
            self.render(target, w, h);
            return;
        };
        if !mem.resize(w, h) {
            self.mem = Some(mem);
            self.render(target, w, h);
            return;
        }
        let dc = mem.dc();
        self.render(dc, w, h);
        mem.blit_to(target);
        self.mem = Some(mem);
    }

    pub(super) fn render(&mut self, dc: HDC, w: i32, h: i32) {
        let paper = rect(0, 0, w, h);
        gdi::fill(dc, &paper, color::PAPER);

        self.render_title(dc, w);
        self.render_header(dc, w);
        self.render_list(dc, w, h);
        self.render_footer(dc, w, h);

        // 外框：WS_POPUP 没有非客户区，全靠这一圈发丝线跟桌面分开。
        // 有 DWM 圆角时按同样半径画圆角框，否则直角的 1px 线会从圆角窗口的四角探出来。
        if self.rounded {
            let r = (self.metrics.dpi as i32 / 12).max(6);
            gdi::round_frame(dc, &rect(0, 0, w - 1, h - 1), color::RULE_STRONG, r);
        } else {
            gdi::frame(dc, &paper, color::RULE_STRONG);
        }
    }

    /// 主窗口标题栏：标题 + 设置 + 关闭。弹窗模式（title_h == 0）什么都不画。
    ///
    /// 不用系统标题栏（WS_CAPTION）：那套框架和这份排版语言完全不搭。
    /// 拖动靠 WM_NCHITTEST 返回 HTCAPTION 实现，成本只有几行。
    fn render_title(&self, dc: HDC, w: i32) {
        let m = self.metrics;
        if m.title_h == 0 {
            return;
        }
        let title_rc = rect(m.pad, 0, w - 2 * m.title_h, m.title_h);
        gdi::text(
            dc,
            self.fonts.ui,
            color::INK,
            &title_rc,
            crate::APP_TITLE,
            false,
        );
        gdi::text(
            dc,
            self.fonts.meta,
            color::INK2,
            &self.settings_rect(),
            "设置",
            true,
        );
        gdi::text(
            dc,
            self.fonts.ui,
            color::INK3,
            &self.close_rect(),
            "✕",
            true,
        );
        gdi::hline(dc, 0, w, m.title_h - 1, color::RULE);
    }

    fn close_rect(&self) -> RECT {
        let m = self.metrics;
        rect(m.width - m.title_h, 0, m.width, m.title_h)
    }

    fn settings_rect(&self) -> RECT {
        let m = self.metrics;
        rect(m.width - 2 * m.title_h, 0, m.width - m.title_h, m.title_h)
    }

    /// 顶部：一个等宽的字段名 + 一行输入 + 一条实线。
    ///
    /// 没有输入框边框、没有浅灰底色 —— 那会让顶部看起来像"控制面板"。
    /// 这里要的是"在纸上写一行字"，所以只有文字和它下面的那条地面线。
    /// 顶部输入行的**文字带**（y 与高度）。标签、输入文字、光标、EDIT 子窗口
    /// 全部按这一条带摆放，才能保证字号不同也依然是同一个基线。
    fn input_line(&self, dc: HDC) -> (i32, i32) {
        let m = self.metrics;
        let h = gdi::text_height(dc, self.fonts.ui).max(m.fs_preview);
        (m.title_h + (m.header_h - h) / 2, h)
    }

    fn render_header(&self, dc: HDC, w: i32) {
        let m = self.metrics;
        let (text_top, text_h) = self.input_line(dc);
        // 标签和输入**同字号、同一条文字带**：11px 等宽的"搜索"挨着 15px 的输入
        // 文字会像两个东西拼在一起；光统一字号还不够，基线也要用同一套计算。
        let label_rc = rect(m.pad, text_top, m.pad + m.mark + 4, text_top + text_h);
        gdi::text(dc, self.fonts.ui, color::INK3, &label_rc, "搜索", false);
        // 预览模式：子窗口（EDIT）不参与离屏渲染，这里代画输入框内容 ——
        // 空字符串就画一根光标，让预览图能表达"这里可以打字"。
        if let Some(text) = &self.preview_text {
            let input_left = m.pad + m.mark + 4;
            if text.is_empty() {
                gdi::fill(
                    dc,
                    &rect(input_left, text_top, input_left + 1, text_top + text_h),
                    color::INK,
                );
            } else {
                let input_rc = rect(input_left, text_top, w - m.pad, text_top + text_h);
                gdi::text(dc, self.fonts.ui, color::INK, &input_rc, text, false);
            }
        }
        // 字段的"地面"线：贯穿整个宽度，让顶部像一条真实的横栏。
        gdi::hline(dc, 0, w, m.list_top() - 1, color::RULE_STRONG);
    }

    /// 元信息行：置顶标记 · 类型补充 · 来源进程 · 使用次数。
    fn meta_text(&self, row: &ItemRow) -> String {
        let mut parts: Vec<String> = Vec::new();
        if row.pinned {
            parts.push("置顶".to_string());
        }
        match row.kind {
            Kind::Files if row.file_count > 0 => parts.push(format!("{} 个文件", row.file_count)),
            Kind::Image if row.img_w > 0 && row.img_h > 0 => {
                parts.push(format!("{} × {}", row.img_w, row.img_h))
            }
            _ => {}
        }
        if let Some(src) = &row.source_exe {
            parts.push(src.clone());
        }
        if row.use_count > 0 {
            parts.push(format!("用过 {} 次", row.use_count + 1));
        }
        if parts.is_empty() {
            "文本".to_string()
        } else {
            parts.join(" · ")
        }
    }

    fn render_list(&mut self, dc: HDC, w: i32, h: i32) {
        let m = self.metrics;
        if self.rows.is_empty() {
            self.render_empty(dc, w, h);
            return;
        }

        let (start, end) = self.nav.range(&m);
        let now = crate::core::store::now_ms();
        let text_left = m.text_left();
        let text_right = m.text_right();

        for idx in start..end {
            let row = self.rows[idx].clone();
            let rc = row_rect(&m, self.nav.top, idx);
            let selected = idx == self.nav.selected;
            let hovered = self.hover == Some(idx);

            if selected {
                // 选中不改变底色（整行仍是纸白），只在最左侧画一道墨条。
                // 反相黑块会压掉文字，浅灰底等于"另一种底色" —— 你两个都不要。
                let edge = (m.dpi as i32 / 32).max(3);
                gdi::fill(dc, &rect(0, rc.top, edge, rc.bottom), color::SEL_EDGE);
            } else if hovered {
                gdi::fill(dc, &rc, color::HOVER);
            }

            // 选中不再改变文字颜色：墨色始终是墨色。
            let fg = color::INK;
            let fg2 = color::INK2;
            // 时间是唯一随选中微微加深的元素（三级灰→二级灰），作为第二重线索。
            let fg3 = if selected { color::INK2 } else { color::INK3 };
            let mark_color = color::INK3;

            // 置顶不再画几何标记：它和选中墨条会挤在同一个外边距里互相干扰，
            // 现在写进元信息行（"置顶 · chrome.exe · …"），用文字表达。

            // 左侧标记区：图片=缩略图，文件=文件夹轮廓，文本=**留空**。
            // 文本行空着是刻意的：十行里八个"文本"方块只是噪声，眼睛要先把它们滤掉。
            let mark_top = rc.top + (m.row_h - m.mark) / 2;
            let mark_rc = rect(m.pad, mark_top, m.pad + m.mark, mark_top + m.mark);
            match row.kind {
                Kind::Image => {
                    if self.thumbs.contains(row.id) {
                        self.thumbs.draw(dc, row.id, &mark_rc);
                    } else {
                        // 缩略图还没到：先画一个空框占位，并懒加载请求一张。
                        gdi::frame(dc, &mark_rc, mark_color);
                        if !self.requested_thumbs.contains(&row.id) {
                            self.requested_thumbs.insert(row.id);
                            unsafe {
                                PostMessageW(self.owner, WM_POPUP_NEED_THUMB, 0, row.id as isize)
                            };
                        }
                    }
                }
                Kind::Files => draw_folder(dc, &mark_rc, mark_color),
                Kind::Text => {}
            }

            // 第一行：预览 + 右对齐的等宽时间（时间列因此形成一条干净的竖线）
            let line_h = gdi::text_height(dc, self.fonts.ui).max(m.fs_preview);
            let meta_h = gdi::text_height(dc, self.fonts.meta).max(m.fs_meta);
            let content_h = line_h + 4 + meta_h;
            let top = rc.top + (m.row_h - content_h) / 2;

            let time = timefmt::relative_short(now, row.last_used_at);
            let time_w = gdi::text_width(dc, self.fonts.mono, &time);
            let time_rc = rect(text_right - time_w, top, text_right, top + line_h);
            let preview_right = (text_right - time_w - m.gap).max(text_left + m.gap);
            let preview_rc = rect(text_left, top, preview_right, top + line_h);
            gdi::text(dc, self.fonts.ui, fg, &preview_rc, &row.preview, false);
            gdi::text(dc, self.fonts.mono, fg3, &time_rc, &time, false);

            // 第二行：元信息
            let meta = self.meta_text(&row);
            let meta_rc = rect(
                text_left,
                top + line_h + 4,
                text_right,
                top + line_h + 4 + meta_h,
            );
            gdi::text(dc, self.fonts.meta, fg2, &meta_rc, &meta, false);

            // 行分隔：从外边距到外边距的发丝线（选中行是浅灰底，发丝线照样看得见）
            if idx + 1 < end {
                gdi::hline(dc, m.pad, w - m.pad, rc.bottom - 1, color::RULE);
            }
        }

        self.render_scrollbar(dc, w, h);
    }

    /// 空态：一句主句 + 一句等宽说明。空屏幕是"邀请你去用它"，不是一个死胡同。
    ///
    /// 注意：居中必须用**当前窗口高度**（`h`），不能用 `Metrics::list_height()`
    /// —— 后者是"窗口拉到最大时"的列表高度，窗口按内容自适应时会把文字推到窗口外
    /// （这个 bug 正是靠离屏预览图发现的）。
    fn render_empty(&self, dc: HDC, w: i32, h: i32) {
        let m = self.metrics;
        let area = rect(m.pad, m.list_top(), w - m.pad, h - m.footer_h);
        let (title, hint) = if self.query.is_empty() {
            ("还没有历史记录", "复制任意内容后就会出现在这里")
        } else {
            ("没有匹配的记录", "换个关键字试试")
        };
        let h1 = gdi::text_height(dc, self.fonts.ui).max(m.fs_preview);
        let h2 = gdi::text_height(dc, self.fonts.badge).max(m.fs_label);
        let total = h1 + m.gap + h2;
        let top = area.top + (area.bottom - area.top - total) / 2;
        gdi::text(
            dc,
            self.fonts.ui,
            color::INK2,
            &rect(area.left, top, area.right, top + h1),
            title,
            true,
        );
        gdi::text(
            dc,
            self.fonts.badge,
            color::INK3,
            &rect(
                area.left,
                top + h1 + m.gap,
                area.right,
                top + h1 + m.gap + h2,
            ),
            hint,
            true,
        );
    }

    fn render_scrollbar(&self, dc: HDC, w: i32, h: i32) {
        let m = self.metrics;
        let vis = self.nav.visible(&m);
        if self.rows.len() <= vis {
            return;
        }
        let list_top = m.list_top() + 2;
        // 同样用真实窗口高度，而不是"最大高度"。
        let list_h = (h - m.footer_h - m.list_top() - 4).max(8);
        let track_x = w - m.pad / 2 - m.scrollbar_w;
        gdi::fill(
            dc,
            &rect(
                track_x,
                list_top,
                track_x + m.scrollbar_w,
                list_top + list_h,
            ),
            color::RULE,
        );
        let total = self.rows.len().max(1);
        let thumb_h = ((list_h as i64 * vis as i64) / total as i64).max(m.row_h as i64 / 2) as i32;
        let max_top = self.nav.max_top(&m).max(1) as i64;
        let y = list_top + (((list_h - thumb_h) as i64 * self.nav.top as i64) / max_top) as i32;
        gdi::fill(
            dc,
            &rect(track_x, y, track_x + m.scrollbar_w, y + thumb_h),
            color::INK3,
        );
    }

    /// 页脚：左侧按键提示、右侧条目计数，都用等宽小字（安静，但需要时能读到）。
    fn render_footer(&self, dc: HDC, w: i32, h: i32) {
        let m = self.metrics;
        let top = h - m.footer_h;
        if top <= m.list_top() {
            return;
        }
        gdi::hline(dc, 0, w, top, color::RULE);
        // 提示语用界面字体（中文混排更整齐），计数用等宽（数字对齐）。
        let hint = "↑↓ 选择    Enter 粘贴    Shift+Enter 仅复制    Esc 关闭";
        let count = self
            .status
            .clone()
            .unwrap_or_else(|| format!("{} 条", self.rows.len()));
        let cw = gdi::text_width(dc, self.fonts.badge, &count);
        let hint_rc = rect(m.pad, top, (w - m.pad - cw - m.gap).max(m.pad + 40), h);
        gdi::text(dc, self.fonts.meta, color::INK3, &hint_rc, hint, false);
        let c_rc = rect(w - m.pad - cw, top, w - m.pad, h);
        gdi::text(dc, self.fonts.badge, color::INK3, &c_rc, &count, false);
    }
}
