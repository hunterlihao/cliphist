//! ClipHist —— 便携版 Windows 剪贴板历史（文本 / 文件 / 图片）。
//!
//! 分层约定：
//! - [`core`] / [`image`]：纯逻辑与纯编解码，不碰 Win32，可被 `cargo test` 完整覆盖。
//! - [`win`]：Win32 直调封装（剪贴板、热键、托盘、注册表、单实例、DPI、进程查询）。
//! - [`ui`]：自绘弹窗与设置窗口（仅依赖 gdi32/user32）。
//! - [`app`]：把上面三者接起来的状态机（控制器窗口过程）。

pub mod app;
pub mod core;
pub mod image;
pub mod ui;
pub mod win;

/// 程序版本号，取自 Cargo。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 单实例互斥体与窗口类名共用前缀（Local\ 作用域 = 每个会话一个）。
pub const APP_NAME: &str = "ClipHist";
pub const APP_TITLE: &str = "剪贴板历史";
