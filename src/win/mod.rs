//! Win32 直调封装层（剪贴板、热键、托盘、注册表、单实例、DPI、进程查询）。
//!
//! 该层只做"系统调用 + 错误码翻译"，不含业务策略 —— 策略在 [`crate::app`] 与 [`crate::core`]。

pub mod clipboard;
pub mod dpi;
pub mod hotkey;
pub mod paths;
pub mod process;
pub mod registry;
pub mod single_instance;
pub mod strings;
pub mod tray;
