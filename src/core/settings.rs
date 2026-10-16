//! 设置项：全部持久化在 `data.db` 的 `meta` 表里（键值对），不产生任何独立配置文件。

use crate::core::store::Store;
use rusqlite::Result as SqlResult;

/// 粘贴行为。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PasteMode {
    /// 写回剪贴板 + 模拟 Ctrl+V 粘到原窗口。
    AutoPaste = 0,
    /// 只写回剪贴板，用户自己按 Ctrl+V。
    ClipboardOnly = 1,
}

impl PasteMode {
    pub fn from_i64(v: i64) -> PasteMode {
        match v {
            1 => PasteMode::ClipboardOnly,
            _ => PasteMode::AutoPaste,
        }
    }
}

// 热键修饰符常量与 Win32 的 MOD_* 对齐。
pub const MOD_ALT: u32 = 0x0001;
pub const MOD_CONTROL: u32 = 0x0002;
pub const MOD_SHIFT: u32 = 0x0004;
pub const MOD_WIN: u32 = 0x0008;

/// 默认热键：Alt+V。
///
/// 刻意避开三个常见组合：`Win+V` 被系统自带剪贴板历史占用；`Ctrl+Shift+V` 是编辑器里的
/// "粘贴为纯文本"；`Ctrl+Alt+V` 是 Office 的"选择性粘贴"。三者都会被全局热键抢走。
/// 用户若不喜欢 `Alt+V`（部分程序把它当菜单助记键），可在设置里改。
pub const DEFAULT_HOTKEY_MODS: u32 = MOD_ALT;
pub const DEFAULT_HOTKEY_VK: u32 = 0x56; // 'V'

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub hotkey_mods: u32,
    pub hotkey_vk: u32,
    /// 非置顶条目的最大条数。
    pub max_items: i64,
    /// 非置顶条目的最长保留天数（0 = 不限）。
    pub keep_days: i64,
    /// 单条文本/路径的最大字节数，超出不记录。
    pub max_item_bytes: i64,
    /// 图片总量上限（字节），超出按最旧优先删除非置顶图片。
    pub max_image_bytes: i64,
    /// 入库前图片长边上限（超过则降采样）。
    pub max_image_edge: u32,
    /// 入库前图片像素数上限（超过则不记录，避免瞬时几十 MB 的解码缓冲）。
    pub max_image_pixels: i64,
    /// 暂停采集（托盘可切换）。
    pub pause_capture: bool,
    pub paste_mode: PasteMode,
    pub autostart: bool,
    pub capture_images: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey_mods: DEFAULT_HOTKEY_MODS,
            hotkey_vk: DEFAULT_HOTKEY_VK,
            max_items: 500,
            keep_days: 30,
            max_item_bytes: 2 * 1024 * 1024,
            max_image_bytes: 200 * 1024 * 1024,
            max_image_edge: 2560,
            max_image_pixels: 12_000_000,
            pause_capture: false,
            paste_mode: PasteMode::AutoPaste,
            autostart: false,
            capture_images: true,
        }
    }
}

impl Settings {
    /// 把解析出的值收敛到合法区间，避免用户手改 data.db 后把程序弄崩。
    pub fn sanitize(&mut self) {
        if self.hotkey_vk == 0 {
            self.hotkey_vk = DEFAULT_HOTKEY_VK;
        }
        if self.hotkey_mods == 0 {
            self.hotkey_mods = DEFAULT_HOTKEY_MODS;
        }
        self.max_items = self.max_items.clamp(0, 100_000);
        self.keep_days = self.keep_days.clamp(0, 3650);
        self.max_item_bytes = self.max_item_bytes.clamp(1024, 64 * 1024 * 1024);
        self.max_image_bytes = self.max_image_bytes.clamp(0, 8 * 1024 * 1024 * 1024);
        self.max_image_edge = self.max_image_edge.clamp(64, 8192);
        self.max_image_pixels = self.max_image_pixels.clamp(10_000, 200_000_000);
    }

    pub fn to_pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            ("hotkey.mods", self.hotkey_mods.to_string()),
            ("hotkey.vk", self.hotkey_vk.to_string()),
            ("retention.max_items", self.max_items.to_string()),
            ("retention.keep_days", self.keep_days.to_string()),
            ("retention.max_item_bytes", self.max_item_bytes.to_string()),
            (
                "retention.max_image_bytes",
                self.max_image_bytes.to_string(),
            ),
            ("image.max_edge", self.max_image_edge.to_string()),
            ("image.max_pixels", self.max_image_pixels.to_string()),
            ("capture.paused", bool_str(self.pause_capture)),
            ("capture.images", bool_str(self.capture_images)),
            ("paste.mode", (self.paste_mode as i64).to_string()),
            ("startup.autostart", bool_str(self.autostart)),
        ]
    }

    /// 从键值对恢复；非法或缺失的值一律回落到默认值（宁可丢设置，不可崩）。
    pub fn from_pairs(pairs: &[(String, String)]) -> Settings {
        let mut s = Settings::default();
        let get = |k: &str| -> Option<&str> {
            pairs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        if let Some(v) = get("hotkey.mods").and_then(|v| v.parse().ok()) {
            s.hotkey_mods = v;
        }
        if let Some(v) = get("hotkey.vk").and_then(|v| v.parse().ok()) {
            s.hotkey_vk = v;
        }
        if let Some(v) = get("retention.max_items").and_then(|v| v.parse().ok()) {
            s.max_items = v;
        }
        if let Some(v) = get("retention.keep_days").and_then(|v| v.parse().ok()) {
            s.keep_days = v;
        }
        if let Some(v) = get("retention.max_item_bytes").and_then(|v| v.parse().ok()) {
            s.max_item_bytes = v;
        }
        if let Some(v) = get("retention.max_image_bytes").and_then(|v| v.parse().ok()) {
            s.max_image_bytes = v;
        }
        if let Some(v) = get("image.max_edge").and_then(|v| v.parse().ok()) {
            s.max_image_edge = v;
        }
        if let Some(v) = get("image.max_pixels").and_then(|v| v.parse().ok()) {
            s.max_image_pixels = v;
        }
        if let Some(v) = get("capture.paused").and_then(parse_bool) {
            s.pause_capture = v;
        }
        if let Some(v) = get("capture.images").and_then(parse_bool) {
            s.capture_images = v;
        }
        if let Some(v) = get("paste.mode").and_then(|v| v.parse().ok()) {
            s.paste_mode = PasteMode::from_i64(v);
        }
        if let Some(v) = get("startup.autostart").and_then(parse_bool) {
            s.autostart = v;
        }
        s.sanitize();
        s
    }

    pub fn load(store: &Store) -> SqlResult<Settings> {
        Ok(Settings::from_pairs(&store.all_meta()?))
    }

    pub fn save(&self, store: &Store) -> SqlResult<()> {
        store.set_meta_pairs(&self.to_pairs())
    }

    /// 热键显示文本，如 `Alt+V`、`Ctrl+Shift+F2`。
    pub fn hotkey_text(&self) -> String {
        format_hotkey(self.hotkey_mods, self.hotkey_vk)
    }
}

fn bool_str(b: bool) -> String {
    if b { "1" } else { "0" }.to_string()
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// 把修饰符 + 虚拟键转成可读文本。
pub fn format_hotkey(mods: u32, vk: u32) -> String {
    let mut out = String::new();
    if mods & MOD_CONTROL != 0 {
        out.push_str("Ctrl+");
    }
    if mods & MOD_ALT != 0 {
        out.push_str("Alt+");
    }
    if mods & MOD_SHIFT != 0 {
        out.push_str("Shift+");
    }
    if mods & MOD_WIN != 0 {
        out.push_str("Win+");
    }
    out.push_str(&vk_text(vk));
    out
}

/// 虚拟键码 → 显示名。只覆盖能被 RegisterHotKey 接受且用户会选的键。
pub fn vk_text(vk: u32) -> String {
    match vk {
        0x30..=0x39 => ((vk as u8) as char).to_string(), // 0-9
        0x41..=0x5A => ((vk as u8) as char).to_string(), // A-Z
        0x70..=0x7B => format!("F{}", vk - 0x6F),        // F1-F12
        0x60..=0x69 => format!("小键盘{}", vk - 0x60),   // Num0-9
        0x20 => "Space".to_string(),
        0x0D => "Enter".to_string(),
        0x09 => "Tab".to_string(),
        0x1B => "Esc".to_string(),
        0x08 => "Backspace".to_string(),
        0x2C => "PrintScreen".to_string(),
        0xC0 => "`".to_string(),
        0xBD => "-".to_string(),
        0xBB => "=".to_string(),
        0xDB => "[".to_string(),
        0xDD => "]".to_string(),
        0xDC => "\\".to_string(),
        0xBA => ";".to_string(),
        0xDE => "'".to_string(),
        0xBC => ",".to_string(),
        0xBE => ".".to_string(),
        0xBF => "/".to_string(),
        0x25 => "Left".to_string(),
        0x26 => "Up".to_string(),
        0x27 => "Right".to_string(),
        0x28 => "Down".to_string(),
        0x24 => "Home".to_string(),
        0x23 => "End".to_string(),
        0x21 => "PageUp".to_string(),
        0x22 => "PageDown".to_string(),
        0x2D => "Insert".to_string(),
        0x2E => "Delete".to_string(),
        other => format!("VK(0x{other:02X})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_alt_v() {
        let s = Settings::default();
        assert_eq!(s.hotkey_text(), "Alt+V");
    }

    #[test]
    fn roundtrip_through_pairs() {
        let s = Settings {
            max_items: 42,
            keep_days: 0,
            paste_mode: PasteMode::ClipboardOnly,
            autostart: true,
            capture_images: false,
            hotkey_mods: MOD_CONTROL | MOD_SHIFT,
            hotkey_vk: 0x70,
            ..Settings::default()
        };
        let pairs: Vec<(String, String)> = s
            .to_pairs()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        assert_eq!(Settings::from_pairs(&pairs), s);
    }

    #[test]
    fn garbage_values_fall_back_to_default() {
        let pairs = vec![
            (
                "retention.max_items".to_string(),
                "not-a-number".to_string(),
            ),
            ("hotkey.vk".to_string(), "".to_string()),
            ("capture.paused".to_string(), "maybe".to_string()),
        ];
        let s = Settings::from_pairs(&pairs);
        assert_eq!(s.max_items, Settings::default().max_items);
        assert_eq!(s.hotkey_vk, DEFAULT_HOTKEY_VK);
        assert!(!s.pause_capture);
    }

    #[test]
    fn sanitize_clamps_absurd_values() {
        let pairs = vec![
            ("retention.max_items".to_string(), "-5".to_string()),
            ("retention.max_item_bytes".to_string(), "1".to_string()),
            ("image.max_edge".to_string(), "999999".to_string()),
        ];
        let s = Settings::from_pairs(&pairs);
        assert_eq!(s.max_items, 0);
        assert_eq!(s.max_item_bytes, 1024);
        assert_eq!(s.max_image_edge, 8192);
    }

    #[test]
    fn hotkey_formatting() {
        assert_eq!(format_hotkey(MOD_CONTROL | MOD_SHIFT, 0x41), "Ctrl+Shift+A");
        assert_eq!(format_hotkey(MOD_WIN, 0x31), "Win+1");
        assert_eq!(format_hotkey(MOD_ALT, 0x7B), "Alt+F12");
        assert_eq!(format_hotkey(MOD_CONTROL | MOD_ALT, 0xC0), "Ctrl+Alt+`");
    }
}
