//! 数据文件定位：便携优先，不可写时回退到用户目录。
//!
//! 需求是"便携版数据都存 data.db 里"，所以默认路径就是 exe 同目录。
//! 但用户可能把 exe 放进 `Program Files`（普通权限下不可写）—— 那种情况下宁可
//! 回退到 `%LOCALAPPDATA%\ClipHist\data.db` 并**如实告诉用户实际路径**，
//! 也不要静默失败或者写不进历史。

use std::path::{Path, PathBuf};

/// 数据文件名（固定，不随版本变化）。
pub const DATA_FILE: &str = "data.db";

/// 当前进程的 exe 路径。
pub fn exe_path() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

/// exe 所在目录。
pub fn exe_dir() -> PathBuf {
    exe_path()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 目录是否可写（真的建一个探针文件，比检查属性可靠）。
pub fn dir_is_writable(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(".cliphist-write-probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn local_appdata_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|p| p.join(crate::APP_NAME))
}

/// 数据文件的实际位置与来源。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataLocation {
    pub path: PathBuf,
    /// true = 便携（exe 同目录）；false = 回退到用户目录。
    pub portable: bool,
    /// 非便携时给出人类可读的原因，设置窗口会显示。
    pub note: Option<String>,
}

impl DataLocation {
    pub fn dir(&self) -> PathBuf {
        self.path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

/// 解析 `data.db` 位置。测试会改环境变量，所以这里每次都重新求值，不做缓存。
pub fn resolve_data_location() -> DataLocation {
    let dir = exe_dir();
    if dir_is_writable(&dir) {
        return DataLocation {
            path: dir.join(DATA_FILE),
            portable: true,
            note: None,
        };
    }

    if let Some(fallback) = local_appdata_dir() {
        let note = format!(
            "程序目录不可写（{}），数据已改存到用户目录。如需真正的便携版，请把程序放到可写目录。",
            dir.display()
        );
        return DataLocation {
            path: fallback.join(DATA_FILE),
            portable: false,
            note: Some(note),
        };
    }

    DataLocation {
        path: dir.join(DATA_FILE),
        portable: true,
        note: Some("程序目录与用户目录都不可写，数据可能无法保存。".to_string()),
    }
}

/// 开机自启命令行：`"<exe>" --autostart`。
pub fn autostart_command() -> String {
    match exe_path() {
        Some(p) => format!("\"{}\" --autostart", p.display()),
        None => format!("\"{}\\{}.exe\" --autostart", exe_dir().display(), crate::APP_NAME),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writable_probe_accepts_temp_dir() {
        assert!(dir_is_writable(&std::env::temp_dir()));
    }

    #[test]
    fn writable_probe_rejects_missing_dir() {
        let mut p = std::env::temp_dir();
        p.push("cliphist-definitely-not-here-12345");
        assert!(!dir_is_writable(&p));
    }

    #[test]
    fn resolved_path_always_named_data_db() {
        let loc = resolve_data_location();
        assert_eq!(loc.path.file_name().unwrap(), DATA_FILE);
        // 便携与非便携两种情况都必须落在某个具体目录里
        assert!(loc.dir().is_absolute());
    }

    #[test]
    fn autostart_command_quotes_path_and_adds_flag() {
        let cmd = autostart_command();
        assert!(cmd.starts_with('"'), "路径必须带引号，否则含空格的目录会启动失败");
        assert!(cmd.ends_with("--autostart"));
        assert!(cmd.contains(".exe"));
    }
}
