//! 领域模型：剪贴板条目、列表行、写入结果。

/// 条目类型。数值写入 `items.kind`，**不可改动**（改了就破坏已有 data.db）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Text = 0,
    Files = 1,
    Image = 2,
}

impl Kind {
    pub fn from_i64(v: i64) -> Option<Kind> {
        match v {
            0 => Some(Kind::Text),
            1 => Some(Kind::Files),
            2 => Some(Kind::Image),
            _ => None,
        }
    }

    pub fn as_i64(self) -> i64 {
        self as i64
    }

    /// 列表行上的类型徽标文字。
    pub fn badge(self) -> &'static str {
        match self {
            Kind::Text => "文本",
            Kind::Files => "文件",
            Kind::Image => "图片",
        }
    }
}

/// 文件条目里的单个路径。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathEntry {
    pub path: String,
    pub is_dir: bool,
}

/// 已解码的位图（RGBA8，行优先，无内边距）。
///
/// 只在采集到粘贴的**瞬时**存在：采集侧读完后交给写线程编码入库，随后释放。
/// 这是本程序唯一可能达到几十 MB 的临时分配（4K 截图 ≈ 33MB），用完即回收。
#[derive(Clone)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for ImageData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ImageData({}x{}, {} bytes)",
            self.width,
            self.height,
            self.rgba.len()
        )
    }
}

/// 一次采集的完整结果，主线程 → 写线程的搬运单元。
#[derive(Clone, Debug)]
pub struct CapturedItem {
    pub kind: Kind,
    /// 内容哈希（见 [`crate::core::hash`]），去重索引用。
    pub hash: u64,
    /// 单行预览（已折叠空白、已截断），列表只读这个字段。
    pub preview: String,
    /// 文本原文；文件条目为路径按行拼接；图片为 `None`。
    pub text: Option<String>,
    /// 原始内容字节数（文本为 UTF-8 字节数，图片为解码后像素字节数）。
    pub byte_len: i64,
    pub files: Vec<PathEntry>,
    pub source_exe: Option<String>,
    pub image: Option<ImageData>,
    pub img_w: i32,
    pub img_h: i32,
}

impl CapturedItem {
    pub fn text(preview: String, text: String, hash: u64, source_exe: Option<String>) -> Self {
        let byte_len = text.len() as i64;
        Self {
            kind: Kind::Text,
            hash,
            preview,
            text: Some(text),
            byte_len,
            files: Vec::new(),
            source_exe,
            image: None,
            img_w: 0,
            img_h: 0,
        }
    }

    pub fn files(
        preview: String,
        files: Vec<PathEntry>,
        text: String,
        hash: u64,
        source_exe: Option<String>,
    ) -> Self {
        let byte_len = text.len() as i64;
        Self {
            kind: Kind::Files,
            hash,
            preview,
            text: Some(text),
            byte_len,
            files,
            source_exe,
            image: None,
            img_w: 0,
            img_h: 0,
        }
    }

    pub fn image(preview: String, image: ImageData, hash: u64, source_exe: Option<String>) -> Self {
        let byte_len = image.rgba.len() as i64;
        Self {
            kind: Kind::Image,
            hash,
            preview,
            text: None,
            byte_len,
            files: Vec::new(),
            source_exe,
            image: Some(image),
            img_w: 0,
            img_h: 0,
        }
    }
}

/// 列表行：只包含渲染弹窗所需的小字段，**不含** text / blob / thumb 本体。
///
/// 这是"内存与历史条数解耦"的关键 —— 1000 条历史也只占几十 KB 常驻内存。
#[derive(Clone, Debug)]
pub struct ItemRow {
    pub id: i64,
    pub kind: Kind,
    pub preview: String,
    pub created_at: i64,
    pub last_used_at: i64,
    pub use_count: i64,
    pub pinned: bool,
    pub source_exe: Option<String>,
    pub file_count: i64,
    pub img_w: i32,
    pub img_h: i32,
    pub thumb_state: i64,
}

/// 落库结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    /// 新插入，返回行 id。
    Inserted(i64),
    /// 命中已有内容：只更新 `last_used_at` / `use_count`。
    Touched(i64),
    /// 被策略跳过（例如超出单条大小上限），不落库。
    Skipped,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_roundtrip() {
        for k in [Kind::Text, Kind::Files, Kind::Image] {
            assert_eq!(Kind::from_i64(k.as_i64()), Some(k));
        }
        assert_eq!(Kind::from_i64(9), None);
    }

    #[test]
    fn captured_item_sets_byte_len() {
        let t = CapturedItem::text("p".into(), "abcd".into(), 1, None);
        assert_eq!(t.byte_len, 4);
        assert_eq!(t.kind, Kind::Text);

        let img = ImageData {
            width: 2,
            height: 2,
            rgba: vec![0; 16],
        };
        let i = CapturedItem::image("图片 2×2".into(), img, 2, Some("mspaint.exe".into()));
        assert_eq!(i.byte_len, 16);
        assert_eq!(i.kind, Kind::Image);
    }
}
