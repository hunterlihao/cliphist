//! 应用状态机：控制器窗口过程把「采集 → 落库 → 弹窗 → 回写」串起来。
//!
//! 线程模型：
//! - **主线程**：所有窗口消息、剪贴板读取、UI 绘制、写回与自动粘贴。
//! - **写线程**：独占一个 sqlite 连接，负责 PNG 编码、缩略图、入库、保留策略修剪。
//!   所有耗时操作（几十 ms 的图片编码）都在它上面，主线程只做微秒级的剪贴板取数。
//!
//! 控制器窗口过程里会调用 `TrackPopupMenu`（托盘菜单、条目右键菜单），那是会**泵消息**的
//! 调用。为此 App 存放在 `thread_local` 的 `RefCell` 里，重入时 `try_borrow_mut` 失败即
//! 退化为 `DefWindowProcW` —— 不会 panic，也不会产生别名 `&mut`。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, PostMessageW, PostQuitMessage,
    RegisterClassExW, RegisterWindowMessageW, SW_SHOWNORMAL, WM_APP, WM_CLIPBOARDUPDATE, WM_CREATE,
    WM_DESTROY, WM_HOTKEY, WM_NCCREATE, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::core::filter::{make_files_preview, make_image_preview, make_text_preview};
use crate::core::hash::hash_str;
use crate::core::model::{CapturedItem, ItemRow, Kind, PathEntry};
use crate::core::payload::{hash_image, hash_paths, join_paths_text};
use crate::core::settings::{PasteMode, Settings};
use crate::core::store::{now_ms, Store};
use crate::image::dib::rgba_to_dibv5;
use crate::image::thumb::{
    decode_png_rgba, downscale_rgba, encode_png_rgba, make_thumbnail_png, needs_downscale,
};
use crate::ui::{popup, settings_window};
use crate::win::clipboard::{self, Content, Limits, Payload};
use crate::win::hotkey;
use crate::win::paths::{self, DataLocation};
use crate::win::process;
use crate::win::registry;
use crate::win::tray::{self, MenuItem, Tray};

/// 控制器窗口类名（单实例唤醒时用它找窗口）。
pub const CLASS_NAME: &str = "ClipHist.Ctl";
/// 写线程 → 主线程：某条的缩略图已生成（`wParam` = item id）。
pub const WM_APP_ITEM_READY: u32 = WM_APP + 20;

const TRAY_ID: u32 = 1;
/// 控制器 → 自己：把主窗口显示出来（启动流程的第二步）。
const WM_SHOW_MAIN: u32 = WM_APP + 19;
/// 预览截断长度（与 `items.preview` 一致）。
const PREVIEW_CHARS: usize = 200;
/// 一次查询最多取多少条。
const PAGE_LIMIT: i64 = 300;
/// 回写剪贴板后、发送 Ctrl+V 前的等待：给目标窗口一点时间完成激活。
const PASTE_DELAY_MS: u64 = 40;
/// 每写 N 条跑一次保留策略。
const PRUNE_EVERY: usize = 8;
/// 统计"是不是目录"时最多查几个路径（网络路径的 metadata 可能很慢）。
const STAT_FILE_LIMIT: usize = 64;

// 托盘菜单命令
const CMD_SHOW: u32 = 100;
const CMD_PAUSE: u32 = 101;
const CMD_CLEAR: u32 = 102;
const CMD_SETTINGS: u32 = 103;
const CMD_AUTOSTART: u32 = 104;
const CMD_EXIT: u32 = 105;
const CMD_POPUP: u32 = 106;
// 条目右键菜单命令
const CMD_ITEM_PIN: u32 = 200;
const CMD_ITEM_DELETE: u32 = 201;
const CMD_ITEM_OPEN: u32 = 202;
const CMD_ITEM_REVEAL: u32 = 203;
const CMD_ITEM_COPY_PATH: u32 = 204;

/// 写线程消息。
enum DbMsg {
    Ingest(Box<CapturedItem>),
    Touch(i64),
    Pin(i64, bool),
    Delete(i64),
    Clear { keep_pinned: bool },
    Settings(Box<Settings>),
    Shutdown,
}

/// 写线程句柄。
struct Writer {
    tx: Sender<DbMsg>,
    join: Option<JoinHandle<()>>,
}

impl Writer {
    fn spawn(db_path: PathBuf, settings: Settings, notify: HWND) -> Writer {
        let (tx, rx) = channel::<DbMsg>();
        // HWND 是裸指针，不实现 Send；跨线程只能传地址（Send 的是 isize），
        // 线程内部再还原。窗口句柄在整个进程生命周期内有效。
        let notify_addr = notify as isize;
        let join = thread::Builder::new()
            .name("cliphist-db".to_string())
            .spawn(move || writer_loop(rx, db_path, settings, notify_addr as HWND))
            .ok();
        Writer { tx, join }
    }

    fn send(&self, msg: DbMsg) {
        // 写线程没了也不能让 UI 崩：忽略发送失败即可（历史丢一条，程序继续可用）。
        let _ = self.tx.send(msg);
    }

    fn shutdown(&mut self) {
        let _ = self.tx.send(DbMsg::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// 写线程主循环。
fn writer_loop(rx: Receiver<DbMsg>, db_path: PathBuf, mut settings: Settings, notify: HWND) {
    let Ok((mut store, _recovered)) = Store::open_or_recover(&db_path) else {
        diag(format_args!("writer: 打开数据文件失败，写线程退出"));
        return;
    };
    diag(format_args!("writer: 已就绪 db={}", db_path.display()));

    // 程序上次被杀掉时可能留下"待生成缩略图"的图片，先补做。
    backfill_thumbs(&mut store, notify);

    let mut since_prune = 0usize;
    let mut quitting = false;
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        // 一次最多处理 32 条，避免长时间不响应设置变更/关机消息。
        while batch.len() < 32 {
            let Ok(m) = rx.try_recv() else { break };
            batch.push(m);
        }

        for msg in batch {
            match msg {
                DbMsg::Ingest(item) => {
                    handle_ingest(&mut store, *item, &settings, notify);
                    since_prune += 1;
                    if since_prune >= PRUNE_EVERY {
                        since_prune = 0;
                        let _ = store.prune(&settings, now_ms());
                    }
                }
                DbMsg::Touch(id) => {
                    let _ = store.touch_last_used(id, now_ms());
                }
                DbMsg::Pin(id, pinned) => {
                    let _ = store.set_pinned(id, pinned);
                }
                DbMsg::Delete(id) => {
                    let _ = store.delete(id);
                }
                DbMsg::Clear { keep_pinned } => {
                    let _ = store.clear(keep_pinned);
                }
                DbMsg::Settings(s) => {
                    settings = *s;
                    let _ = store.prune(&settings, now_ms());
                }
                DbMsg::Shutdown => {
                    // 退出前把空闲页还给系统，便携版（U 盘）上这一步尤其值。
                    let _ = store.incremental_vacuum();
                    quitting = true;
                    break;
                }
            }
        }
        if quitting {
            break;
        }
    }
}

/// 入库一条：图片先编码 PNG，再写行，最后补缩略图。
fn handle_ingest(store: &mut Store, mut item: CapturedItem, settings: &Settings, notify: HWND) {
    // 哈希在这里算：图片的 RGBA 可能有几十 MB，逐字节 FNV 要几十毫秒 ——
    // 这个开销必须留在写线程，不能落在 UI 线程上。
    finalize_hash(&mut item);

    let (blob, thumb) = if item.kind == Kind::Image {
        let blob = item
            .image
            .as_ref()
            .and_then(|img| encode_image_for_store(img, settings.max_image_edge));
        let thumb = item.image.as_ref().and_then(make_thumbnail_png);
        (blob, thumb)
    } else {
        (None, None)
    };

    let now = now_ms();
    match store.upsert(&item, blob, now) {
        Ok(outcome) => {
            diag(format_args!(
                "writer: upsert kind={:?} → {outcome:?}",
                item.kind
            ));
            let id = match outcome {
                crate::core::model::WriteOutcome::Inserted(id) => Some(id),
                crate::core::model::WriteOutcome::Touched(id) => Some(id),
                crate::core::model::WriteOutcome::Skipped => None,
            };
            if let Some(id) = id {
                if thumb.is_some() {
                    let _ = store.set_thumb(id, thumb);
                    if !notify.is_null() {
                        unsafe { PostMessageW(notify, WM_APP_ITEM_READY, id as usize, 0) };
                    }
                } else if item.kind == Kind::Image {
                    // 缩略图生成失败：标记成 2，避免每次启动都重试。
                    let _ = store.set_thumb(id, None);
                }
            }
        }
        Err(_) => {
            // 落库失败不致命，稍后还能再复制一次。
        }
    }
}

/// 图片入库前按设置决定是否降采样；返回编码后的 PNG。
fn encode_image_for_store(img: &crate::core::model::ImageData, max_edge: u32) -> Option<Vec<u8>> {
    if needs_downscale(img.width, img.height, max_edge) {
        let small = downscale_rgba(img, max_edge);
        encode_png_rgba(small.width, small.height, &small.rgba).ok()
    } else {
        encode_png_rgba(img.width, img.height, &img.rgba).ok()
    }
}

fn backfill_thumbs(store: &mut Store, notify: HWND) {
    let Ok(pending) = store.pending_thumbs(32) else {
        return;
    };
    if pending.is_empty() {
        return;
    }
    for (id, png) in pending {
        let thumb = decode_png_rgba(&png)
            .ok()
            .and_then(|img| make_thumbnail_png(&img));
        let ok = thumb.is_some();
        let _ = store.set_thumb(id, thumb);
        if ok && !notify.is_null() {
            unsafe { PostMessageW(notify, WM_APP_ITEM_READY, id as usize, 0) };
        }
    }
}

// ------------------------------------------------------- 采集决策（纯逻辑）

/// 采集决策。
#[derive(Debug)]
pub enum IngestDecision {
    Store(Box<CapturedItem>),
    /// 认识格式但按策略跳过，附原因（用于气泡提示）。
    Skip(&'static str),
    /// 不是我们要记的内容。
    Ignore,
}

/// 剪贴板内容 → 待入库条目。
///
/// `hash` 刻意留 0：哈希在写线程计算（见 [`finalize_hash`]），
/// 因为图片的逐字节哈希有几十毫秒开销。
pub fn to_captured(
    content: Content,
    source_exe: Option<String>,
    limits: &Limits,
) -> IngestDecision {
    match content {
        Content::Empty | Content::Ignored => IngestDecision::Ignore,
        Content::Skipped(msg) => IngestDecision::Skip(msg),
        Content::Text(text) => {
            if text.len() > limits.max_text_bytes {
                return IngestDecision::Skip("内容过大，未记录");
            }
            let preview = make_text_preview(&text, PREVIEW_CHARS);
            IngestDecision::Store(Box::new(CapturedItem::text(preview, text, 0, source_exe)))
        }
        Content::Files { paths, is_cut: _ } => {
            // 剪切/复制不影响去重：同一批文件就是同一条历史。
            // 但粘回去时一律用 DROPEFFECT_COPY，绝不替用户"移动"文件。
            let text = join_paths_text(&paths);
            if text.len() > limits.max_text_bytes {
                return IngestDecision::Skip("文件路径过多，未记录");
            }
            let entries: Vec<PathEntry> = paths
                .iter()
                .enumerate()
                .map(|(i, p)| PathEntry {
                    path: p.clone(),
                    is_dir: i < STAT_FILE_LIMIT && Path::new(p).is_dir(),
                })
                .collect();
            let preview = make_files_preview(&entries);
            IngestDecision::Store(Box::new(CapturedItem::files(
                preview, entries, text, 0, source_exe,
            )))
        }
        Content::Image(img) => {
            let w = img.width;
            let h = img.height;
            let preview = make_image_preview(w, h);
            let mut item = CapturedItem::image(preview, img, 0, source_exe);
            item.img_w = w as i32;
            item.img_h = h as i32;
            IngestDecision::Store(Box::new(item))
        }
    }
}

/// 计算内容哈希（在写线程调用）。
pub fn finalize_hash(item: &mut CapturedItem) {
    item.hash = match item.kind {
        Kind::Text => item.text.as_deref().map(hash_str).unwrap_or(0),
        Kind::Files => hash_paths(&item.files),
        Kind::Image => item
            .image
            .as_ref()
            .map(|img| hash_image(img.width, img.height, &img.rgba))
            .unwrap_or(0),
    };
}

// ------------------------------------------------------------------- App

struct App {
    hwnd: HWND,
    popup: HWND,
    /// 居中的主窗口（与浮动弹窗共用窗口类，只是模式不同）。
    main_hwnd: HWND,
    settings_hwnd: HWND,
    tray: Option<Tray>,
    settings: Settings,
    read: Store,
    data: DataLocation,
    writer: Writer,
    limits: Limits,
    /// 我们自己刚写进剪贴板的序号：下一次 `WM_CLIPBOARDUPDATE` 要忽略掉，
    /// 否则"粘贴"会被当成"复制"再记一条，形成自我循环。
    suppress_seq: Option<u32>,
    /// 弹窗打开前的前台窗口（粘贴目标）。
    target: HWND,
    /// 单实例唤醒用的注册消息。
    wake_msg: u32,
    hotkey_ok: bool,
    /// 托盘图标创建失败（受限权限/外壳不可用）。程序照常运行，只在页脚提示。
    tray_failed: bool,
    /// 上一次托盘单击的时间：用于去掉重复通知。
    last_tray_click: Option<std::time::Instant>,
    /// 粘贴队列（有序条目 id）。只存在内存里：它是一次性动作，不该进库。
    queue: Vec<i64>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// 诊断日志：`--log <路径>` 时把关键链路写到文件。
///
/// 为什么需要它：剪贴板链路是"系统通知 → 取数 → 过滤 → 写线程 → 落库"五段，
/// 任何一段静默失败，用户看到的都只是"怎么没记录"。现场排障必须能看出断在哪一段。
/// 默认完全关闭（连文件都不打开），不影响正常运行的任何开销。
static DIAG: std::sync::OnceLock<std::sync::Mutex<std::fs::File>> = std::sync::OnceLock::new();

/// 打开诊断日志。必须在创建窗口之前调用。
pub fn set_diag_log(path: &str) -> std::io::Result<()> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let _ = DIAG.set(std::sync::Mutex::new(f));
    diag(format_args!("=== ClipHist {} 启动 ===", crate::VERSION));
    Ok(())
}

fn diag(args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let Some(file) = DIAG.get() else { return };
    if let Ok(mut f) = file.lock() {
        let _ = writeln!(f, "{args}");
        let _ = f.flush();
    }
}

/// 供其它模块（如托盘）写入同一条诊断日志。
pub(crate) fn diag_msg(args: std::fmt::Arguments<'_>) {
    diag(args);
}

impl App {
    fn new(hwnd: HWND, popup: HWND, main_hwnd: HWND, settings_hwnd: HWND) -> Option<App> {
        let data = paths::resolve_data_location();
        let data_note = data.note.clone();
        let (read, recovered) = match Store::open_or_recover(&data.path) {
            Ok(v) => v,
            Err(e) => {
                // 注意：只有"真损坏"才会走到隔离重建；这里能走到说明是占用/权限问题，
                // 程序退出前绝不会改动用户的文件。
                show_error(&e.to_string());
                return None;
            }
        };
        let settings = Settings::load(&read).unwrap_or_default();
        let writer = Writer::spawn(data.path.clone(), settings.clone(), hwnd);
        let mut app = App {
            hwnd,
            popup,
            main_hwnd,
            settings_hwnd,
            tray: None,
            settings,
            read,
            data,
            writer,
            limits: Limits::default(),
            suppress_seq: None,
            target: std::ptr::null_mut(),
            wake_msg: 0,
            hotkey_ok: false,
            tray_failed: false,
            last_tray_click: None,
            queue: Vec::new(),
        };
        app.refresh_limits();

        // 上次是"损坏恢复"的话必须告诉用户，绝不能静默丢历史。
        if let Some(path) = recovered {
            app.balloon(
                "历史数据已重建",
                &format!(
                    "原数据文件无法读取，已隔离为 {}，程序以空库继续运行。",
                    path.file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ),
                true,
            );
        }
        if let Some(note) = data_note {
            app.balloon("数据位置已调整", &note, true);
        }
        // 自启项与实际路径对齐（程序被移动过时命令行需要更新）。
        if app.settings.autostart {
            let cmd = paths::autostart_command();
            if !registry::is_autostart_enabled(&cmd) {
                let _ = registry::set_autostart(true, &cmd);
            }
        }
        Some(app)
    }

    fn refresh_limits(&mut self) {
        self.limits = Limits {
            max_text_bytes: self.settings.max_item_bytes.max(1024) as usize,
            max_image_pixels: self.settings.max_image_pixels,
            capture_images: self.settings.capture_images,
            max_files: 1000,
        };
    }

    fn balloon(&self, title: &str, text: &str, warning: bool) {
        if let Some(tray) = &self.tray {
            tray.balloon(title, text, warning);
        }
    }

    fn tray_tooltip(&self) -> String {
        let hotkey = self.settings.hotkey_text();
        if self.settings.pause_capture {
            format!("{} · 已暂停记录（{hotkey} 打开）", crate::APP_TITLE)
        } else if self.hotkey_ok {
            format!("{} · {hotkey}", crate::APP_TITLE)
        } else {
            format!("{} · 热键未生效，请点托盘菜单", crate::APP_TITLE)
        }
    }

    /// 按指定列表窗口当前的搜索词查一页。
    fn query_rows_for(&self, hwnd: HWND) -> Vec<ItemRow> {
        let q = popup::current_query(hwnd);
        self.read.list_page(&q, PAGE_LIMIT).unwrap_or_default()
    }

    fn query_rows(&self) -> Vec<ItemRow> {
        self.query_rows_for(self.popup)
    }

    fn popup_status(&self) -> Option<String> {
        if !self.queue.is_empty() {
            return Some(format!("队列 {} 条", self.queue.len()));
        }
        if self.tray_failed {
            // 没有托盘就没有气泡提示，这行状态是唯一的告知途径。
            return Some("托盘不可用".to_string());
        }
        if self.settings.pause_capture {
            Some("已暂停".to_string())
        } else {
            None
        }
    }

    fn toggle_popup(&mut self) {
        if popup::is_visible(self.popup) {
            popup::hide(self.popup);
            return;
        }
        // 记录粘贴目标：必须是"别的窗口"，不能是我们自己。
        let fg = clipboard::foreground_window();
        if fg != self.popup
            && fg != self.main_hwnd
            && fg != self.hwnd
            && fg != self.settings_hwnd
            && !fg.is_null()
        {
            self.target = fg;
        }
        let rows = self.query_rows_for(self.popup);
        let status = self.popup_status();
        popup::show(self.popup, rows, status);
    }

    /// 打开/收起设置窗口。托盘菜单和主窗口标题栏的"设置"都走这里。
    fn open_settings(&mut self) {
        settings_window::toggle(
            self.settings_hwnd,
            self.settings.clone(),
            &self.data.path.display().to_string(),
        );
    }

    /// 显示/隐藏居中的主窗口。托盘单击走这里。
    /// 托盘单击。一次点击可能连发两条通知（`NIN_SELECT` + `WM_LBUTTONUP`），
    /// 直接各 toggle 一次就会"开了又关"，表现为点了没反应 —— 所以先做 350ms 去抖。
    fn tray_click(&mut self) {
        let now = std::time::Instant::now();
        if let Some(last) = self.last_tray_click {
            if now.duration_since(last) < std::time::Duration::from_millis(350) {
                diag(format_args!("tray: 去抖，忽略重复通知"));
                return;
            }
        }
        self.last_tray_click = Some(now);
        self.toggle_main();
    }

    /// 加入/移出粘贴队列。顺序就是加入的先后，界面上显示①②③。
    fn toggle_queue(&mut self, target: HWND, id: i64) {
        if let Some(pos) = self.queue.iter().position(|q| *q == id) {
            self.queue.remove(pos);
        } else {
            self.queue.push(id);
        }
        self.sync_queue(target);
    }

    /// 把队列推给列表窗口并刷新状态文字。
    fn sync_queue(&mut self, target: HWND) {
        for h in [self.popup, self.main_hwnd] {
            if !h.is_null() {
                popup::set_queue(h, self.queue.clone());
            }
        }
        if !target.is_null() {
            let status = self.popup_status();
            popup::set_status(target, status);
        }
        // 先算好 tooltip，再取 &mut tray（否则同时借 self）。
        let tip = self.tray_tooltip();
        if let Some(tray) = &mut self.tray {
            tray.set_tooltip(&tip);
        }
    }

    /// 粘贴队列里的下一条（Ctrl+Alt+V）。
    fn paste_next(&mut self) {
        if self.queue.is_empty() {
            self.balloon("队列是空的", "在列表里按空格把条目加入队列。", false);
            return;
        }
        let id = self.queue.remove(0);
        diag(format_args!(
            "queue: 粘贴下一条 id={id} 剩余={}",
            self.queue.len()
        ));
        self.choose(id, false);
        self.sync_queue(std::ptr::null_mut());
    }

    fn toggle_main(&mut self) {
        diag(format_args!(
            "toggle_main: hwnd={:?} visible={}",
            self.main_hwnd,
            popup::is_visible(self.main_hwnd)
        ));
        if popup::is_visible(self.main_hwnd) {
            // 可见但没在前台（比如被别的窗口盖住）→ 先把窗口提到前面，
            // 而不是直接关掉。否则用户的第一下点击看上去就是"什么都没发生"。
            if !popup::is_foreground(self.main_hwnd) {
                popup::bring_to_front(self.main_hwnd);
                return;
            }
            popup::hide(self.main_hwnd);
            return;
        }
        // 主窗口打开时也别忘了记录粘贴目标（与浮动列表同一套规则）。
        let fg = clipboard::foreground_window();
        if fg != self.popup
            && fg != self.main_hwnd
            && fg != self.hwnd
            && fg != self.settings_hwnd
            && !fg.is_null()
        {
            self.target = fg;
        }
        let rows = self.query_rows_for(self.main_hwnd);
        let status = self.popup_status();
        popup::show(self.main_hwnd, rows, status);
    }

    fn show_main(&mut self) {
        if !popup::is_visible(self.main_hwnd) {
            self.toggle_main();
        }
    }

    fn show_popup(&mut self) {
        if !popup::is_visible(self.popup) {
            self.toggle_popup();
        }
    }

    /// 采集一次剪贴板。
    fn ingest(&mut self) {
        if self.settings.pause_capture {
            diag(format_args!("ingest: 已暂停，跳过"));
            return;
        }
        let content = clipboard::read(self.hwnd, &self.limits);
        let source = process::foreground_process_name();
        diag(format_args!(
            "ingest: content={content:?} source={source:?}"
        ));
        match to_captured(content, source, &self.limits) {
            IngestDecision::Ignore => diag(format_args!("ingest: → ignore")),
            IngestDecision::Skip(msg) => {
                diag(format_args!("ingest: → skip（{msg}）"));
                self.balloon("未记录", msg, true)
            }
            IngestDecision::Store(item) => {
                diag(format_args!(
                    "ingest: → store kind={:?} preview={:?}",
                    item.kind, item.preview
                ));
                self.writer.send(DbMsg::Ingest(item))
            }
        }
    }

    /// 选中一条：写回剪贴板（+ 可选自动粘贴）。
    fn choose(&mut self, id: i64, clipboard_only: bool) {
        let kind = self.read.get_kind(id).ok().flatten();
        let result = match kind {
            Some(Kind::Text) => match self.read.get_text(id) {
                Ok(Some(text)) => clipboard::write(self.hwnd, &Payload::Text(&text)),
                _ => Err("读取文本失败".to_string()),
            },
            Some(Kind::Files) => {
                let files = self.read.get_files(id).unwrap_or_default();
                if files.is_empty() {
                    Err("文件记录已失效".to_string())
                } else {
                    let paths: Vec<String> = files.into_iter().map(|f| f.path).collect();
                    // 一律以"复制"写回：绝不因为来源是剪切就把用户的文件移走。
                    clipboard::write(
                        self.hwnd,
                        &Payload::Files {
                            paths: &paths,
                            is_cut: false,
                        },
                    )
                }
            }
            Some(Kind::Image) => match self.read.get_blob(id) {
                Ok(Some(png)) => match decode_png_rgba(&png) {
                    Ok(img) => match rgba_to_dibv5(img.width, img.height, &img.rgba) {
                        Some(dib) => clipboard::write(
                            self.hwnd,
                            &Payload::Image {
                                dibv5: &dib,
                                png: &png,
                            },
                        ),
                        None => Err("图片数据无效".to_string()),
                    },
                    Err(e) => Err(format!("图片解码失败：{e}")),
                },
                _ => Err("读取图片失败".to_string()),
            },
            None => Err("记录已不存在".to_string()),
        };

        if let Err(e) = result {
            self.balloon("粘贴失败", &e, true);
            return;
        }

        // 忽略由我们自己触发的那次剪贴板变化。
        self.suppress_seq = Some(clipboard::sequence_number());
        // 使用次数 +1（走写线程，UI 不等待）。
        self.writer.send(DbMsg::Touch(id));

        if clipboard_only || self.settings.paste_mode == PasteMode::ClipboardOnly {
            return;
        }

        let target = self.target;
        if !clipboard::activate_window(target) {
            self.balloon(
                "已复制到剪贴板",
                "目标窗口无法激活（可能以管理员权限运行），请手动按 Ctrl+V。",
                true,
            );
            return;
        }
        // 给目标窗口一点时间处理激活，再送 Ctrl+V；否则按键可能落空。
        thread::sleep(Duration::from_millis(PASTE_DELAY_MS));
        if !clipboard::send_ctrl_v() {
            self.balloon("已复制到剪贴板", "模拟粘贴失败，请手动按 Ctrl+V。", true);
        }
    }

    /// 条目右键菜单。必须在控制器窗口上弹：`TrackPopupMenu` 会泵消息，
    /// 交给弹窗去弹会破坏它"窗口过程不被重入"的前提。
    fn item_menu(&mut self, target: HWND, id: i64) {
        // 菜单是模态的：弹出期间列表窗口会失活。先挂上"菜单保护"，
        // 否则 WM_ACTIVATE 会把窗口当成"点了别处"直接关掉。
        popup::set_menu_open(target, true);
        let kind = self.read.get_kind(id).ok().flatten();
        let pinned = self.read.get_pinned(id).unwrap_or(false);
        let files = if kind == Some(Kind::Files) {
            self.read.get_files(id).unwrap_or_default()
        } else {
            Vec::new()
        };
        let first_path = files.first().map(|f| f.path.clone());

        let mut items = vec![
            MenuItem::new(
                CMD_ITEM_PIN,
                if pinned {
                    "取消置顶"
                } else {
                    "置顶（常用）"
                },
            ),
            MenuItem::new(CMD_ITEM_DELETE, "删除这条"),
        ];
        if kind == Some(Kind::Files) {
            items.push(MenuItem::separator());
            items.push(MenuItem::new(CMD_ITEM_OPEN, "打开").enabled(first_path.is_some()));
            items.push(
                MenuItem::new(CMD_ITEM_REVEAL, "打开所在文件夹").enabled(first_path.is_some()),
            );
            items.push(MenuItem::new(CMD_ITEM_COPY_PATH, "复制路径"));
        }

        let pos = tray::cursor_pos();
        let cmd = tray::popup_menu(self.hwnd, pos.x, pos.y, &items);
        // 菜单关闭后再解除保护：期间窗口失活不算"点别处"。
        popup::set_menu_open(target, false);
        if !target.is_null() && popup::is_visible(target) {
            popup::bring_to_front(target);
        }
        let Some(cmd) = cmd else {
            return;
        };
        match cmd {
            CMD_ITEM_PIN => {
                self.writer.send(DbMsg::Pin(id, !pinned));
            }
            CMD_ITEM_DELETE => {
                self.writer.send(DbMsg::Delete(id));
            }
            CMD_ITEM_OPEN => {
                if let Some(p) = first_path {
                    shell_open(&p, None);
                }
            }
            CMD_ITEM_REVEAL => {
                if let Some(p) = first_path {
                    reveal_in_explorer(&p);
                }
            }
            CMD_ITEM_COPY_PATH => {
                let paths: Vec<String> = files.into_iter().map(|f| f.path).collect();
                if !paths.is_empty() {
                    let text = join_paths_text(&paths);
                    let _ = clipboard::write(self.hwnd, &Payload::Text(&text));
                    self.suppress_seq = Some(clipboard::sequence_number());
                }
            }
            _ => {}
        }
        // 列表内容变了，刷新弹窗（保持选中项）。
        if popup::is_visible(self.popup) {
            let rows = self.query_rows();
            popup::set_rows(self.popup, rows);
        }
    }

    fn tray_menu(&mut self) {
        let items = vec![
            MenuItem::new(CMD_SHOW, "打开主窗口（居中）"),
            MenuItem::new(CMD_POPUP, "浮动列表（光标处）"),
            MenuItem::new(
                CMD_PAUSE,
                if self.settings.pause_capture {
                    "继续记录"
                } else {
                    "暂停记录"
                },
            )
            .checked(self.settings.pause_capture),
            MenuItem::separator(),
            MenuItem::new(CMD_SETTINGS, "设置…"),
            MenuItem::new(CMD_AUTOSTART, "开机自启").checked(self.settings.autostart),
            MenuItem::new(CMD_CLEAR, "清空历史（保留置顶）"),
            MenuItem::separator(),
            MenuItem::new(CMD_EXIT, "退出"),
        ];
        let pos = tray::cursor_pos();
        let Some(cmd) = tray::popup_menu(self.hwnd, pos.x, pos.y, &items) else {
            return;
        };
        self.handle_command(cmd);
    }

    fn handle_command(&mut self, cmd: u32) {
        match cmd {
            CMD_SHOW => self.show_main(),
            CMD_POPUP => self.show_popup(),
            CMD_PAUSE => {
                self.settings.pause_capture = !self.settings.pause_capture;
                self.apply_settings(self.settings.clone());
            }
            CMD_CLEAR => {
                self.writer.send(DbMsg::Clear { keep_pinned: true });
                self.balloon("已清空", "未置顶的历史记录已删除。", false);
                if popup::is_visible(self.popup) {
                    let rows = self.query_rows();
                    popup::set_rows(self.popup, rows);
                }
            }
            CMD_SETTINGS => self.open_settings(),
            CMD_AUTOSTART => {
                self.settings.autostart = !self.settings.autostart;
                self.apply_settings(self.settings.clone());
            }
            CMD_EXIT => {
                unsafe { DestroyWindow(self.hwnd) };
            }
            _ => {}
        }
    }

    /// 应用并持久化设置（热键、自启、托盘提示一起更新）。
    fn apply_settings(&mut self, new_settings: Settings) {
        // 先留住旧值：热键注册失败时要靠它回退，而 self.settings 马上就要被覆盖了。
        let prev = self.settings.clone();
        let hotkey_changed = new_settings.hotkey_mods != prev.hotkey_mods
            || new_settings.hotkey_vk != prev.hotkey_vk;
        let autostart_changed = new_settings.autostart != prev.autostart;

        self.settings = new_settings;
        self.settings.sanitize();
        self.refresh_limits();

        if hotkey_changed {
            let (mods, vk) = (self.settings.hotkey_mods, self.settings.hotkey_vk);
            hotkey::unregister(self.hwnd, hotkey::HOTKEY_SHOW);
            hotkey::unregister(self.hwnd, hotkey::HOTKEY_QUEUE);
            if hotkey::register(self.hwnd, hotkey::HOTKEY_SHOW, mods, vk) {
                self.hotkey_ok = true;
            } else {
                // 新组合键被占用：退回旧热键，并把设置改回去 ——
                // 否则用户会处于"既没有新热键、也没有旧热键"的状态。
                self.hotkey_ok = hotkey::register(
                    self.hwnd,
                    hotkey::HOTKEY_SHOW,
                    prev.hotkey_mods,
                    prev.hotkey_vk,
                );
                self.settings.hotkey_mods = prev.hotkey_mods;
                self.settings.hotkey_vk = prev.hotkey_vk;
                self.balloon(
                    "热键不可用",
                    &format!(
                        "{} 已被其他程序占用，已保留 {}。",
                        crate::core::settings::format_hotkey(mods, vk),
                        self.settings.hotkey_text()
                    ),
                    true,
                );
            }
        }

        if autostart_changed {
            let cmd = paths::autostart_command();
            if let Err(e) = registry::set_autostart(self.settings.autostart, &cmd) {
                self.balloon("开机自启设置失败", &e, true);
                self.settings.autostart = !self.settings.autostart;
            }
        }

        if let Err(e) = self.settings.save(&self.read) {
            self.balloon("设置保存失败", &e.to_string(), true);
        }
        self.writer
            .send(DbMsg::Settings(Box::new(self.settings.clone())));
        let tip = self.tray_tooltip();
        if let Some(tray) = &mut self.tray {
            tray.set_tooltip(&tip);
        }
        if popup::is_visible(self.popup) {
            let status = self.popup_status();
            popup::set_status(self.popup, status);
        }
    }

    fn on_clipboard_update(&mut self) {
        let seq = clipboard::sequence_number();
        diag(format_args!(
            "clipboard: WM_CLIPBOARDUPDATE seq={seq} suppress={:?}",
            self.suppress_seq
        ));
        if self.suppress_seq == Some(seq) {
            // 这次变化是我们自己写回去的：既不能记录，也不能弄丢标记。
            self.suppress_seq = None;
            return;
        }
        self.ingest();
    }

    fn shutdown(&mut self) {
        clipboard::remove_listener(self.hwnd);
        hotkey::unregister(self.hwnd, hotkey::HOTKEY_SHOW);
        self.writer.shutdown();
        if let Some(tray) = &mut self.tray {
            tray.remove();
        }
        self.tray = None;
    }
}

/// 注册控制器窗口类。
pub fn register_class(hinstance: HINSTANCE) -> bool {
    let cls = crate::win::strings::to_wide(CLASS_NAME);
    let mut wc: WNDCLASSEXW = unsafe { std::mem::zeroed() };
    wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
    wc.lpfnWndProc = Some(wnd_proc);
    wc.hInstance = hinstance;
    wc.lpszClassName = cls.as_ptr();
    unsafe { RegisterClassExW(&wc) != 0 }
}

/// 创建控制器窗口（始终隐藏，只用来收消息和挂托盘图标）。
pub fn create(hinstance: HINSTANCE) -> Option<HWND> {
    let cls = crate::win::strings::to_wide(CLASS_NAME);
    let title = crate::win::strings::to_wide(crate::APP_TITLE);
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            cls.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        None
    } else {
        Some(hwnd)
    }
}

/// 初始化运行时（托盘、热键、剪贴板监听）。`autostart` 时首启不弹气泡。
///
/// 调用前必须已经设置好进程 DPI 感知（在 `main` 里、创建窗口之前）。
pub fn init_runtime(hwnd: HWND, autostart: bool) {
    APP.with(|cell| {
        let mut guard = cell.borrow_mut();
        let Some(app) = guard.as_mut() else { return };

        // 开场动画：放在最前面，让它盖住后面这些初始化（托盘/监听/热键）。
        // 开机自启时一律不显示 —— 那时候用户没在等，弹一张卡片只会打扰。
        // 启动流程：开场动画 → 主界面。
        // 开机自启例外：那时候用户没在等，安静待在托盘里就好。
        let mut wait_splash = false;
        if !autostart && app.settings.show_splash {
            let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
            if !hinstance.is_null() {
                wait_splash = crate::ui::splash::show(hwnd, hinstance).is_some();
            }
        }
        if !autostart && !wait_splash {
            // 没有动画（或动画没建起来）也要进主界面，不能只在托盘里等人来点。
            unsafe { PostMessageW(hwnd, WM_SHOW_MAIN, 0, 0) };
        }

        // 托盘
        let tip = app.tray_tooltip();
        app.tray = Tray::add(hwnd, TRAY_ID, &tip);
        diag(format_args!(
            "init: tray = {}",
            if app.tray.is_some() { "ok" } else { "FAILED" }
        ));
        if app.tray.is_none() {
            // 托盘拿不到**绝不能**拖垮或阻塞整个程序：热键弹窗才是主入口。
            //
            // 早期版本这里弹了个模态错误框 —— 结果初始化卡在消息框上，消息循环根本没跑，
            // 剪贴板一条都收不到（受限令牌/无外壳的环境里 100% 复现）。
            // 现在只记下来，并在列表页脚用状态文字提示，不阻塞任何东西。
            app.tray_failed = true;
        }

        // 剪贴板监听（零轮询）
        let listener_ok = clipboard::add_listener(hwnd);
        diag(format_args!(
            "init: AddClipboardFormatListener = {listener_ok}"
        ));
        if !listener_ok {
            show_error("无法监听剪贴板变化，程序将退出。");
            unsafe { PostQuitMessage(1) };
            return;
        }

        // 全局热键
        let (mods, vk) = (app.settings.hotkey_mods, app.settings.hotkey_vk);
        app.hotkey_ok = hotkey::register(hwnd, hotkey::HOTKEY_SHOW, mods, vk);
        diag(format_args!(
            "init: RegisterHotKey({}) = {}",
            app.settings.hotkey_text(),
            app.hotkey_ok
        ));
        // 队列热键：Ctrl+Alt+V，固定绑定（不占设置里的那一行）。
        let queue_ok = hotkey::register(
            hwnd,
            hotkey::HOTKEY_QUEUE,
            windows_sys::Win32::UI::Input::KeyboardAndMouse::MOD_CONTROL
                | windows_sys::Win32::UI::Input::KeyboardAndMouse::MOD_ALT,
            0x56,
        );
        diag(format_args!(
            "init: RegisterHotKey(Ctrl+Alt+V) = {queue_ok}"
        ));
        if !app.hotkey_ok {
            let text = app.settings.hotkey_text();
            app.balloon(
                "热键被占用",
                &format!("{text} 无法注册（可能被其他程序占用），可在设置里换一个组合键。"),
                true,
            );
        }
        // 单实例唤醒消息
        app.wake_msg = unsafe {
            RegisterWindowMessageW(
                crate::win::strings::to_wide(crate::win::single_instance::WAKE_MESSAGE).as_ptr(),
            )
        };
        let tip = app.tray_tooltip();
        if let Some(tray) = &mut app.tray {
            tray.set_tooltip(&tip);
        }
        if !autostart {
            // 首次启动给一句提示，告诉用户怎么用；--autostart 时安静启动。
            app.balloon(
                crate::APP_TITLE,
                &format!(
                    "已开始记录剪贴板。按 {} 打开历史列表。",
                    app.settings.hotkey_text()
                ),
                false,
            );
        }
    });
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        return 1;
    }

    // 注册消息（单实例唤醒）必须用动态 id 比较。
    //
    // 注意用 try_borrow：控制器窗口过程里会调用 TrackPopupMenu（托盘/右键菜单），
    // 那会泵消息并重入到这里 —— 此时 APP 已被可变借用，`borrow()` 会直接 panic。
    let wake_msg = APP.with(|c| {
        c.try_borrow()
            .ok()
            .and_then(|g| g.as_ref().map(|a| a.wake_msg))
            .unwrap_or(0)
    });
    if msg == WM_SHOW_MAIN || msg == crate::ui::splash::WM_SPLASH_DONE {
        // 由消息驱动而不是在 init_runtime 里直接开窗：那样会在 App 借用的过程中
        // 再走一遍 SetWindowPos/SetFocus（同步派发消息），属于自己踩自己的重入坑。
        let busy = APP.with(|c| match c.try_borrow_mut() {
            Ok(mut g) => {
                if let Some(app) = g.as_mut() {
                    app.show_main();
                }
                false
            }
            Err(_) => true,
        });
        if busy {
            let again = if msg == WM_SHOW_MAIN {
                WM_SHOW_MAIN
            } else {
                crate::ui::splash::WM_SPLASH_DONE
            };
            unsafe { PostMessageW(hwnd, again, 0, 0) };
        }
        return 0;
    }
    if wake_msg != 0 && msg == wake_msg {
        // 第二个实例来敲门：把居中的主窗口叫出来（用户能立刻看到反馈），
        // 而不是什么都不发生 —— 否则"双击没反应"和"程序崩了"看起来一样。
        let busy = APP.with(|c| match c.try_borrow_mut() {
            Ok(mut g) => {
                if let Some(app) = g.as_mut() {
                    app.show_main();
                }
                false
            }
            Err(_) => true,
        });
        if busy {
            unsafe { PostMessageW(hwnd, msg, wp, lp) };
        }
        return 0;
    }

    match msg {
        WM_CREATE => {
            let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
            let popup = popup::create(hwnd, hinstance, popup::Mode::Popup);
            let main_hwnd = popup::create(hwnd, hinstance, popup::Mode::Main);
            let settings_hwnd = settings_window::create(hwnd, hinstance);
            let Some(popup) = popup else {
                show_error("创建界面窗口失败，程序将退出。");
                unsafe { PostQuitMessage(1) };
                return 0;
            };
            let settings_hwnd = settings_hwnd.unwrap_or(std::ptr::null_mut());
            let main_hwnd = main_hwnd.unwrap_or(std::ptr::null_mut());
            let app = App::new(hwnd, popup, main_hwnd, settings_hwnd);
            APP.with(|c| {
                *c.borrow_mut() = app;
            });
            return 0;
        }
        WM_CLIPBOARDUPDATE => {
            APP.with(|c| match c.try_borrow_mut() {
                Ok(mut g) => {
                    if let Some(app) = g.as_mut() {
                        app.on_clipboard_update();
                    } else {
                        diag(format_args!("clipboard: APP 尚未初始化，丢弃"));
                    }
                }
                Err(_) => diag(format_args!("clipboard: APP 被占用（重入），丢弃")),
            });
            return 0;
        }
        WM_HOTKEY => {
            if wp as i32 == hotkey::HOTKEY_QUEUE {
                APP.with(|c| {
                    if let Ok(mut g) = c.try_borrow_mut() {
                        if let Some(app) = g.as_mut() {
                            app.paste_next();
                        }
                    }
                });
                return 0;
            }
            if wp as i32 == hotkey::HOTKEY_SHOW {
                APP.with(|c| {
                    if let Ok(mut g) = c.try_borrow_mut() {
                        if let Some(app) = g.as_mut() {
                            app.toggle_popup();
                        }
                    }
                });
            }
            return 0;
        }
        tray::TRAY_CALLBACK_MSG => {
            let (event, _x, _y) = tray::decode_callback(wp, lp);
            const WM_LBUTTONUP: u32 = 0x0202;
            const WM_LBUTTONDBLCLK: u32 = 0x0203;
            const WM_RBUTTONUP: u32 = 0x0205;
            const WM_CONTEXTMENU: u32 = 0x007B;
            const NIN_SELECT: u32 = 0x0400;
            match event {
                // 托盘左键：打开**居中的主窗口**（要"待在屏幕上"的那种）。
                // 随手呼出的浮动列表留给全局热键 —— 那个要的是快、跟着光标。
                WM_LBUTTONUP | WM_LBUTTONDBLCLK | NIN_SELECT => {
                    let busy = APP.with(|c| match c.try_borrow_mut() {
                        Ok(mut g) => {
                            if let Some(app) = g.as_mut() {
                                app.tray_click();
                            }
                            false
                        }
                        Err(_) => true,
                    });
                    // 初始化（托盘/监听/热键/开场动画）期间 App 正被借用：这时托盘单击
                    // 不能丢，延后一拍重投给自己，等借用结束再处理。
                    if busy {
                        unsafe { PostMessageW(hwnd, msg, wp, lp) };
                    }
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => APP.with(|c| {
                    if let Ok(mut g) = c.try_borrow_mut() {
                        if let Some(app) = g.as_mut() {
                            app.tray_menu();
                        }
                    }
                }),
                _ => {}
            }
            return 0;
        }
        popup::WM_POPUP_QUEUE => {
            let id = lp as i64;
            let target = wp as HWND;
            APP.with(|c| {
                if let Ok(mut g) = c.try_borrow_mut() {
                    if let Some(app) = g.as_mut() {
                        app.toggle_queue(target, id);
                    }
                }
            });
            return 0;
        }
        popup::WM_LIST_OPEN_SETTINGS => {
            APP.with(|c| {
                if let Ok(mut g) = c.try_borrow_mut() {
                    if let Some(app) = g.as_mut() {
                        app.open_settings();
                    }
                }
            });
            return 0;
        }
        popup::WM_POPUP_CHOOSE => {
            let id = lp as i64;
            let clipboard_only = wp == 1;
            APP.with(|c| {
                if let Ok(mut g) = c.try_borrow_mut() {
                    if let Some(app) = g.as_mut() {
                        app.choose(id, clipboard_only);
                    }
                }
            });
            return 0;
        }
        popup::WM_POPUP_QUERY => {
            // wParam 是"谁在打字"（浮动列表 or 主窗口）。早先这里写死了 popup，
            // 于是在主窗口里搜关键字，结果全被塞去了另一个窗口 —— 看起来就是"搜索没用"。
            let target = wp as HWND;
            APP.with(|c| {
                if let Ok(mut g) = c.try_borrow_mut() {
                    if let Some(app) = g.as_mut() {
                        if !target.is_null() {
                            let q = popup::current_query(target);
                            let rows = app.query_rows_for(target);
                            diag(format_args!(
                                "query: target={:?} q={:?} → {} 条",
                                target,
                                q,
                                rows.len()
                            ));
                            popup::set_rows(target, rows);
                        }
                    }
                }
            });
            return 0;
        }
        popup::WM_POPUP_CONTEXT => {
            let id = lp as i64;
            let target = wp as HWND;
            APP.with(|c| {
                if let Ok(mut g) = c.try_borrow_mut() {
                    if let Some(app) = g.as_mut() {
                        app.item_menu(target, id);
                    }
                }
            });
            return 0;
        }
        popup::WM_POPUP_PIN => {
            let id = lp as i64;
            APP.with(|c| {
                if let Ok(mut g) = c.try_borrow_mut() {
                    if let Some(app) = g.as_mut() {
                        let pinned = app.read.get_pinned(id).unwrap_or(false);
                        app.writer.send(DbMsg::Pin(id, !pinned));
                        let rows = app.query_rows();
                        popup::set_rows(app.popup, rows);
                    }
                }
            });
            return 0;
        }
        popup::WM_POPUP_DELETE => {
            let id = lp as i64;
            APP.with(|c| {
                if let Ok(mut g) = c.try_borrow_mut() {
                    if let Some(app) = g.as_mut() {
                        app.writer.send(DbMsg::Delete(id));
                        let rows = app.query_rows();
                        popup::set_rows(app.popup, rows);
                    }
                }
            });
            return 0;
        }
        popup::WM_POPUP_NEED_THUMB => {
            let id = lp as i64;
            APP.with(|c| {
                if let Ok(mut g) = c.try_borrow_mut() {
                    if let Some(app) = g.as_mut() {
                        if let Ok(Some(png)) = app.read.get_thumb(id) {
                            popup::set_thumb(app.popup, id, &png);
                        }
                    }
                }
            });
            return 0;
        }
        WM_APP_ITEM_READY => {
            let id = wp as i64;
            APP.with(|c| {
                if let Ok(mut g) = c.try_borrow_mut() {
                    if let Some(app) = g.as_mut() {
                        if id != 0 {
                            if let Ok(Some(png)) = app.read.get_thumb(id) {
                                popup::set_thumb(app.popup, id, &png);
                            }
                        }
                    }
                }
            });
            return 0;
        }
        settings_window::WM_SETTINGS_CHANGED => {
            APP.with(|c| {
                if let Ok(mut g) = c.try_borrow_mut() {
                    if let Some(app) = g.as_mut() {
                        if let Some(new_settings) =
                            settings_window::take_settings(app.settings_hwnd)
                        {
                            app.apply_settings(new_settings);
                        }
                    }
                }
            });
            return 0;
        }
        settings_window::WM_SETTINGS_CLEAR => {
            APP.with(|c| {
                if let Ok(mut g) = c.try_borrow_mut() {
                    if let Some(app) = g.as_mut() {
                        app.writer.send(DbMsg::Clear { keep_pinned: true });
                        app.balloon("已清空", "未置顶的历史记录已删除。", false);
                    }
                }
            });
            return 0;
        }
        settings_window::WM_SETTINGS_OPEN_FOLDER => {
            APP.with(|c| {
                if let Ok(mut g) = c.try_borrow_mut() {
                    if let Some(app) = g.as_mut() {
                        let dir = app.data.dir().display().to_string();
                        shell_open(&dir, None);
                    }
                }
            });
            return 0;
        }
        WM_DESTROY => {
            APP.with(|c| {
                if let Ok(mut g) = c.try_borrow_mut() {
                    if let Some(app) = g.as_mut() {
                        app.shutdown();
                    }
                    *g = None;
                }
            });
            // 弹窗与设置窗口是控制器窗口的 owned window，会随它一起销毁。
            unsafe { PostQuitMessage(0) };
            return 0;
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn shell_open(target: &str, params: Option<&str>) {
    let op = crate::win::strings::to_wide("open");
    let file = crate::win::strings::to_wide(target);
    let params = params.map(crate::win::strings::to_wide);
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            params
                .as_ref()
                .map(|p| p.as_ptr())
                .unwrap_or(std::ptr::null()),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
}

fn reveal_in_explorer(path: &str) {
    let p = Path::new(path);
    if p.is_dir() {
        shell_open(path, None);
    } else {
        // explorer 的 /select, 参数要求路径不加引号，含逗号的路径会失效 —— 已知限制。
        shell_open("explorer.exe", Some(&format!("/select,{path}")));
    }
}

/// 无法继续时的最后手段：用系统消息框把原因说清楚。
fn show_error(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let title = crate::win::strings::to_wide(crate::APP_TITLE);
    let body = crate::win::strings::to_wide(text);
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            body.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::ImageData;

    fn limits() -> Limits {
        Limits::default()
    }

    #[test]
    fn text_content_becomes_item_with_preview() {
        let long = "行".repeat(500);
        let d = to_captured(
            Content::Text(long.clone()),
            Some("notepad.exe".into()),
            &limits(),
        );
        let IngestDecision::Store(item) = d else {
            panic!("应当入库");
        };
        assert_eq!(item.kind, Kind::Text);
        assert!(item.preview.chars().count() <= PREVIEW_CHARS + 1);
        assert_eq!(item.byte_len as usize, long.len());
        assert_eq!(item.source_exe.as_deref(), Some("notepad.exe"));
        // 哈希留给写线程
        assert_eq!(item.hash, 0);
        let mut item = *item;
        finalize_hash(&mut item);
        assert_eq!(item.hash, hash_str(&long), "哈希必须与纯函数一致");
    }

    #[test]
    fn oversized_text_is_skipped_not_truncated() {
        let mut l = limits();
        l.max_text_bytes = 10;
        let d = to_captured(Content::Text("0123456789ABC".into()), None, &l);
        assert!(
            matches!(d, IngestDecision::Skip(_)),
            "超限必须拒绝，不能悄悄截断"
        );
    }

    #[test]
    fn empty_and_ignored_are_dropped() {
        assert!(matches!(
            to_captured(Content::Empty, None, &limits()),
            IngestDecision::Ignore
        ));
        assert!(matches!(
            to_captured(Content::Ignored, None, &limits()),
            IngestDecision::Ignore
        ));
        assert!(matches!(
            to_captured(Content::Skipped("图片过大，未记录"), None, &limits()),
            IngestDecision::Skip(_)
        ));
    }

    #[test]
    fn files_content_builds_entries_and_preview() {
        let dir = env!("CARGO_MANIFEST_DIR");
        let file = format!("{dir}\\Cargo.toml");
        let d = to_captured(
            Content::Files {
                paths: vec![file.clone(), dir.to_string()],
                is_cut: true,
            },
            Some("explorer.exe".into()),
            &limits(),
        );
        let IngestDecision::Store(item) = d else {
            panic!("应当入库");
        };
        assert_eq!(item.kind, Kind::Files);
        assert_eq!(item.files.len(), 2);
        assert!(!item.files[0].is_dir, "Cargo.toml 是文件");
        assert!(item.files[1].is_dir, "工作区根目录应当是目录");
        assert!(item.preview.contains("Cargo.toml"));
        assert!(item.preview.contains('\\'), "目录名要带反斜杠以示区分");
        assert_eq!(
            item.text.as_deref(),
            Some(format!("{file}\r\n{dir}").as_str())
        );
        let mut item = *item;
        finalize_hash(&mut item);
        assert_ne!(item.hash, 0);
    }

    #[test]
    fn image_content_carries_dimensions_and_hash() {
        let img = ImageData {
            width: 4,
            height: 3,
            rgba: vec![200u8; 4 * 3 * 4],
        };
        let d = to_captured(Content::Image(img), None, &limits());
        let IngestDecision::Store(mut item) = d else {
            panic!("应当入库");
        };
        assert_eq!(item.kind, Kind::Image);
        assert_eq!(item.preview, "图片 4×3");
        assert_eq!(item.img_w, 4);
        assert_eq!(item.img_h, 3);
        assert_eq!(item.byte_len, 4 * 3 * 4);
        finalize_hash(&mut item);
        let expect = hash_image(4, 3, &[200u8; 48]);
        assert_eq!(item.hash, expect);
    }

    #[test]
    fn finalize_hash_is_stable_across_calls() {
        let mut a = CapturedItem::text("p".into(), "同样内容".into(), 0, None);
        let mut b = CapturedItem::text("p".into(), "同样内容".into(), 0, None);
        finalize_hash(&mut a);
        finalize_hash(&mut b);
        assert_eq!(a.hash, b.hash);
        assert_eq!(a.hash, hash_str("同样内容"));
    }

    #[test]
    fn image_downscale_only_when_needed() {
        let small = ImageData {
            width: 10,
            height: 10,
            rgba: vec![0u8; 400],
        };
        let png = encode_image_for_store(&small, 2560).expect("小图不需要缩放，必须能编码");
        assert!(!png.is_empty());

        let big = ImageData {
            width: 100,
            height: 10,
            rgba: vec![7u8; 100 * 10 * 4],
        };
        let png = encode_image_for_store(&big, 20).expect("大图先缩放再编码");
        let back = decode_png_rgba(&png).unwrap();
        assert_eq!(back.width, 20, "长边必须被限制到 max_edge");
        assert_eq!(back.height, 2);
    }
}
