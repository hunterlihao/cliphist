//! `ClipHist.exe` 入口。
//!
//! 参数：
//! - `--autostart`：开机自启时由注册表拉起，安静启动（不弹首启气泡）。
//! - `--selftest`：跑一遍存储/编解码自检后退出（给脚本与验收用，不开窗口）。
//! - `--version` / `-v`：打印版本。
//!
//! **GUI 子系统**：这个属性决定双击时不会分配控制台窗口 —— 否则每次启动都会先闪一个
//! 黑色终端窗口。代价是进程默认没有 stdout，所以 CLI 模式需要显式把标准输出接回
//! 调用者的终端，见 [`attach_console_for_cli`]。
#![windows_subsystem = "windows"]

use std::process::ExitCode;

use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MessageBoxW, TranslateMessage, MB_ICONERROR, MB_OK, MSG,
};

/// CLI 模式下把标准输出接回调用方终端。
///
/// 关键细节：**只在标准输出确实不可用**时才接管。用户若写了
/// `ClipHist.exe --selftest > out.txt`，那个重定向句柄本来有效，
/// 此时再去 AttachConsole 并 `SetStdHandle(CONOUT$)` 会把重定向吞掉
/// （输出跑到终端里，文件是空的）。
fn attach_console_for_cli() {
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE,
        STD_OUTPUT_HANDLE,
    };

    unsafe {
        let existing = GetStdHandle(STD_OUTPUT_HANDLE);
        if !existing.is_null() && existing != INVALID_HANDLE_VALUE {
            return; // 已被重定向，别动它
        }
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return; // 没有可挂的控制台（例如从资源管理器启动）
        }
        let name = cliphist::win::strings::to_wide("CONOUT$");
        let h = CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            return;
        }
        SetStdHandle(STD_OUTPUT_HANDLE, h);
        SetStdHandle(STD_ERROR_HANDLE, h);
    }
}

/// 启动期致命错误：GUI 模式下没有终端可打印，只能用消息框把原因说清楚。
fn fatal(message: &str, code: u8) -> ExitCode {
    let title = cliphist::win::strings::to_wide(cliphist::APP_TITLE);
    let body = cliphist::win::strings::to_wide(message);
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            body.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
    ExitCode::from(code)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let flag_value = |flag: &str| -> Option<String> {
        let i = args.iter().position(|a| a == flag)?;
        args.get(i + 1).cloned()
    };
    let arg_value = |args: &[String], flag: &str| -> Option<Option<usize>> {
        let i = args.iter().position(|a| a == flag)?;
        Some(args.get(i + 1).and_then(|v| v.parse().ok()))
    };

    // 有 CLI 参数才去接控制台；纯 GUI 启动时完全不碰控制台。
    let is_cli = has("--selftest")
        || has("--selftest-clipboard")
        || has("--bench-fill")
        || has("--render-preview")
        || has("--render-settings")
        || has("--version")
        || has("-v")
        || has("--help")
        || has("-h");
    if is_cli {
        attach_console_for_cli();
    }

    if has("--version") || has("-v") {
        println!("{} {} (portable)", cliphist::APP_TITLE, cliphist::VERSION);
        return ExitCode::SUCCESS;
    }
    if has("--help") || has("-h") {
        print_help();
        return ExitCode::SUCCESS;
    }
    if has("--selftest") {
        return selftest();
    }
    if has("--selftest-clipboard") {
        return selftest_clipboard();
    }
    if let Some(n) = arg_value(&args, "--bench-fill") {
        return bench_fill(n.unwrap_or(500), flag_value("--log"));
    }
    if let Some(out) = flag_value("--render-preview") {
        return render_preview_cmd(&out, has("--preview-empty"), flag_value("--preview-query"));
    }
    if let Some(out) = flag_value("--render-settings") {
        return render_settings_cmd(&out);
    }

    // 诊断日志：GUI 模式下把采集链路写进文件，用于现场排障（默认完全关闭）。
    if let Some(path) = flag_value("--log") {
        if let Err(e) = cliphist::app::set_diag_log(&path) {
            eprintln!("无法打开诊断日志 {path}：{e}");
        }
    }

    // ① DPI 感知必须在创建任何窗口之前设置：否则整个界面会被系统按位图拉伸，中文发虚。
    cliphist::win::dpi::set_process_dpi_awareness();

    // ② 单实例：已有实例就唤醒它然后退出。guard 必须活到 main 结束。
    let Some(_guard) = cliphist::win::single_instance::acquire() else {
        cliphist::win::single_instance::wake_existing(cliphist::app::CLASS_NAME);
        return ExitCode::SUCCESS;
    };

    let hinstance =
        unsafe { windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null()) };
    if hinstance.is_null() {
        return fatal("无法获取程序实例句柄，程序无法启动。", 2);
    }

    // ③ 注册三个窗口类：控制器（隐藏）、历史弹窗、设置窗口。
    if !cliphist::app::register_class(hinstance)
        || !cliphist::ui::popup::register_class(hinstance)
        || !cliphist::ui::settings_window::register_class(hinstance)
    {
        return fatal("窗口类注册失败，程序无法启动。", 3);
    }

    // ④ 控制器窗口始终隐藏，只负责收消息（托盘回调、热键、剪贴板变化）。
    let Some(hwnd) = cliphist::app::create(hinstance) else {
        return fatal("创建主窗口失败，程序无法启动。", 4);
    };

    cliphist::app::init_runtime(hwnd, has("--autostart"));

    // ⑤ 消息循环。
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let r = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if r <= 0 {
            break; // 0 = WM_QUIT，-1 = 出错
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    ExitCode::SUCCESS
}

fn print_help() {
    println!(
        "{} {} —— 便携版剪贴板历史",
        cliphist::APP_TITLE,
        cliphist::VERSION
    );
    println!();
    println!("用法：ClipHist.exe [选项]");
    println!("  --autostart   开机自启模式（安静启动，不弹提示）");
    println!("  --selftest    跑存储与编解码自检后退出");
    println!("  --selftest-clipboard  跑剪贴板读写自检后退出（会覆盖当前剪贴板内容）");
    println!("  --bench-fill [N]      往当前数据文件预填 N 条历史（默认 500），用于内存基准测试");
    println!("  --version     打印版本");
    println!();
    println!("数据文件：data.db（程序同目录；该目录不可写时回退到 %LOCALAPPDATA%\\ClipHist）");
}

/// 开发者预览：把设置窗口渲染成 PNG（不显示窗口、不写注册表、不落库）。
fn render_settings_cmd(out_path: &str) -> ExitCode {
    use cliphist::core::settings::Settings;
    use cliphist::image::thumb::encode_png_rgba;
    use cliphist::ui::settings_window::{self, Layout};

    let hinstance =
        unsafe { windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null()) };
    if hinstance.is_null() || !settings_window::register_class(hinstance) {
        eprintln!("窗口类注册失败");
        return ExitCode::from(3);
    }
    let Some(hwnd) = settings_window::create(std::ptr::null_mut(), hinstance) else {
        eprintln!("创建设置窗口失败");
        return ExitCode::from(4);
    };
    let dpi = unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForWindow(hwnd) };
    let dpi = if dpi == 0 { 96 } else { dpi };
    let l = Layout::new(dpi);
    let (w, h) = (l.width, l.height());
    let values = Settings::default();
    let sample_path = r"C:\Users\you\AppData\Local\ClipHist\data.db";
    let Some(rgba) = settings_window::render_preview(hwnd, values, sample_path, w, h) else {
        eprintln!("渲染失败");
        settings_window::destroy(hwnd);
        return ExitCode::from(5);
    };
    let png = encode_png_rgba(w as u32, h as u32, &rgba).unwrap_or_default();
    settings_window::destroy(hwnd);
    match std::fs::write(out_path, &png) {
        Ok(()) => {
            println!("已渲染设置窗口预览：{out_path}（{w}×{h} @ {dpi} dpi）");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("写入 {out_path} 失败：{e}");
            ExitCode::from(6)
        }
    }
}

/// 开发者预览：把历史弹窗渲染成 PNG（不显示窗口、不碰剪贴板、不碰数据库）。
///
/// 用法：`ClipHist.exe --render-preview out.png [--preview-empty] [--preview-query 关键字]`
fn render_preview_cmd(out_path: &str, empty: bool, query: Option<String>) -> ExitCode {
    use cliphist::core::model::{ItemRow, Kind};
    use cliphist::core::store::now_ms;
    use cliphist::image::thumb::encode_png_rgba;
    use cliphist::ui::popup;
    use cliphist::ui::theme::Metrics;

    let hinstance =
        unsafe { windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null()) };
    if hinstance.is_null() || !popup::register_class(hinstance) {
        eprintln!("窗口类注册失败");
        return ExitCode::from(3);
    }
    let Some(hwnd) = popup::create(std::ptr::null_mut(), hinstance) else {
        eprintln!("创建弹窗失败");
        return ExitCode::from(4);
    };

    // 一张示意用的缩略图：渐变 + 一块亮色，用来检查 AlphaBlend 的缩放与居中。
    let (tw, th) = (200u32, 120u32);
    let mut trgba = Vec::with_capacity((tw * th * 4) as usize);
    for y in 0..th {
        for x in 0..tw {
            let in_patch = x > 60 && x < 140 && y > 30 && y < 90;
            trgba.extend_from_slice(&[
                if in_patch { 232 } else { (30 + x / 2) as u8 },
                if in_patch { 226 } else { (60 + y / 2) as u8 },
                if in_patch {
                    210
                } else {
                    (110 + (x + y) / 4) as u8
                },
                255,
            ]);
        }
    }
    let thumb_png = encode_png_rgba(tw, th, &trgba).unwrap_or_default();

    let now = now_ms();
    let row = |id: i64,
               kind: Kind,
               preview: &str,
               src: &str,
               ago_secs: i64,
               use_count: i64,
               pinned: bool,
               extra: (i64, i32, i32)|
     -> ItemRow {
        ItemRow {
            id,
            kind,
            preview: preview.to_string(),
            created_at: now - ago_secs * 1000,
            last_used_at: now - ago_secs * 1000,
            use_count,
            pinned,
            source_exe: Some(src.to_string()),
            file_count: extra.0,
            img_w: extra.1,
            img_h: extra.2,
            thumb_state: 1,
        }
    };

    let rows = vec![
        row(
            1,
            Kind::Text,
            "「剪贴板历史」的设计稿已同步到 docs/design.md，注意第 3 节改成了索引页方向",
            "Code.exe",
            40,
            2,
            false,
            (0, 0, 0),
        ),
        row(
            2,
            Kind::Text,
            "git rebase -i HEAD~3 之后先把 fixup 合并，再 force-with-lease 推上去",
            "WindowsTerminal.exe",
            8 * 60,
            0,
            false,
            (0, 0, 0),
        ),
        row(
            3,
            Kind::Files,
            "报告 v2.docx、预算.xlsx 等 3 项",
            "explorer.exe",
            3 * 3600,
            0,
            true,
            (3, 0, 0),
        ),
        row(
            4,
            Kind::Image,
            "图片 1920×1080",
            "SnippingTool.exe",
            5 * 3600,
            1,
            false,
            (0, 1920, 1080),
        ),
        row(
            5,
            Kind::Text,
            "SELECT id, preview FROM items WHERE pinned = 1 ORDER BY last_used_at DESC;",
            "ssms.exe",
            26 * 3600,
            4,
            false,
            (0, 0, 0),
        ),
        row(
            6,
            Kind::Text,
            "https://example.com/docs/portable-clipboard?lang=zh-CN",
            "msedge.exe",
            3 * 86400,
            0,
            false,
            (0, 0, 0),
        ),
        row(
            7,
            Kind::Text,
            "中文与 English 混排，用来检查雅黑 15px 下的字重是否发虚",
            "WeChat.exe",
            12 * 86400,
            0,
            false,
            (0, 0, 0),
        ),
    ];

    let dpi = unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForWindow(hwnd) };
    let dpi = if dpi == 0 { 96 } else { dpi };
    let m = Metrics::new(dpi);
    let w = m.width;
    let rows = if empty { Vec::new() } else { rows };
    let h = m.height_for(rows.len());
    let query = query.unwrap_or_default();
    let Some(rgba) =
        popup::render_preview(hwnd, rows.clone(), None, &query, &[(4, thumb_png)], w, h)
    else {
        eprintln!("渲染失败");
        popup::destroy(hwnd);
        return ExitCode::from(5);
    };
    let png = encode_png_rgba(w as u32, h as u32, &rgba).unwrap_or_default();
    popup::destroy(hwnd);

    match std::fs::write(out_path, &png) {
        Ok(()) => {
            println!(
                "已渲染预览：{out_path}（{w}×{h} @ {dpi} dpi，{} 行）",
                rows.len()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("写入 {out_path} 失败：{e}");
            ExitCode::from(6)
        }
    }
}

/// 基准填充：往**当前数据文件**里预填 N 条历史。
///
/// 存在的理由：本项目最核心的性能主张是"常驻内存与历史条数解耦"，
/// 而验证它必须有一个装了几百条历史的库。这个开关用**正式的 Store 代码**填充，
/// 因此不会出现"脚本自己造库、DDL 与代码漂移"的假验证。
///
/// `--log <路径>` 会把结果同时写一份到文件。脚本化调用请用它，**不要**让父进程去
/// 捕获本进程的标准输出：在受限沙箱/非交互式嵌套 shell 里，那类管道重定向实测会互相
/// 等待而卡死（这也是本参数存在的唯一原因）。
///
/// ⚠ 会写进真实数据文件（便携目录或 %LOCALAPPDATA%）。基准测试请先复制一份程序
///   到临时目录再运行（`scripts\measure-memory.ps1` 就是这么做的）。
fn bench_fill(count: usize, log_path: Option<String>) -> ExitCode {
    use cliphist::core::filter::{make_files_preview, make_text_preview};
    use cliphist::core::hash::hash_str;
    use cliphist::core::model::{CapturedItem, PathEntry};
    use cliphist::core::payload::hash_paths;
    use cliphist::core::store::{now_ms, Store};

    let mut report: Vec<String> = Vec::new();
    let data = cliphist::win::paths::resolve_data_location();
    println!("数据文件：{}", data.path.display());
    report.push(format!("db_path={}", data.path.display()));
    println!("预填 {count} 条…");

    let Ok((mut store, recovered)) = Store::open_or_recover(&data.path) else {
        eprintln!("无法打开数据文件");
        return ExitCode::from(6);
    };
    if let Some(p) = recovered {
        println!("（注意：原数据文件损坏，已隔离为 {}）", p.display());
        report.push(format!("recovered={}", p.display()));
    }

    let start = std::time::Instant::now();
    let base = now_ms() - (count as i64) * 60_000;
    let mut inserted = 0usize;
    for i in 0..count {
        // 混入不同长度与类型，避免用"等长纯英文"这种不真实的样本自我安慰
        let item = match i % 5 {
            0 => {
                let text = format!(
                    "第 {i} 条文本：中文与 English 混排，用于检验预览截断与等宽对齐。{}",
                    "补充内容 ".repeat(i % 40)
                );
                CapturedItem::text(
                    make_text_preview(&text, 200),
                    text.clone(),
                    hash_str(&text),
                    Some(
                        if i % 2 == 0 {
                            "chrome.exe"
                        } else {
                            "notepad.exe"
                        }
                        .into(),
                    ),
                )
            }
            1 => {
                let text = format!("https://example.com/page/{i}?q=剪贴板历史&lang=zh-CN");
                CapturedItem::text(
                    make_text_preview(&text, 200),
                    text.clone(),
                    hash_str(&text),
                    Some("msedge.exe".into()),
                )
            }
            2 => {
                let text = format!("SELECT id, preview FROM items WHERE id = {i};");
                CapturedItem::text(
                    text.clone(),
                    text.clone(),
                    hash_str(&text),
                    Some("ssms.exe".into()),
                )
            }
            3 => {
                let n = (i % 4) + 1;
                let entries: Vec<PathEntry> = (0..n)
                    .map(|k| PathEntry {
                        path: format!(r"C:\Users\demo\Documents\项目{i}\报告-{k}.docx"),
                        is_dir: false,
                    })
                    .collect();
                let text = cliphist::core::payload::join_paths_text(
                    &entries.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
                );
                CapturedItem::files(
                    make_files_preview(&entries),
                    entries.clone(),
                    text,
                    hash_paths(&entries),
                    Some("explorer.exe".into()),
                )
            }
            _ => {
                let text = format!("D:\\code\\demo\\src\\module_{i}.rs:{}: 这里是较长的代码片段，用来看正文回退搜索是否有效。", i * 7);
                CapturedItem::text(
                    make_text_preview(&text, 200),
                    text.clone(),
                    hash_str(&text),
                    Some("Code.exe".into()),
                )
            }
        };
        if store
            .upsert(&item, None, base + (i as i64) * 60_000)
            .is_ok()
        {
            inserted += 1;
        }
    }
    let elapsed = start.elapsed();

    // 顺手量一下列表查询：这是弹窗打开时唯一会碰库的操作
    let q_start = std::time::Instant::now();
    let rows = store.list_page("", 300).unwrap_or_default();
    let q_elapsed = q_start.elapsed();
    let s_start = std::time::Instant::now();
    let _ = store.list_page("报告", 300).unwrap_or_default();
    let s_elapsed = s_start.elapsed();

    let (total, bytes) = store.stats().unwrap_or((0, 0));
    println!("已写入 {inserted} 条（耗时 {elapsed:?}）");
    println!("库内共 {total} 条，data.db {} KB", bytes / 1024);
    println!(
        "查询最近 300 条：{:?}（返回 {} 条）；关键字搜索：{:?}",
        q_elapsed,
        rows.len(),
        s_elapsed
    );

    report.push(format!("inserted={inserted}"));
    report.push(format!("total={total}"));
    report.push(format!("db_kb={}", bytes / 1024));
    report.push(format!("fill_ms={}", elapsed.as_millis()));
    report.push(format!("query_us={}", q_elapsed.as_micros()));
    report.push(format!("query_rows={}", rows.len()));
    report.push(format!("search_us={}", s_elapsed.as_micros()));

    if let Some(path) = log_path {
        let text = report.join("\n") + "\n";
        match std::fs::write(&path, text) {
            Ok(()) => println!("结果已写入 {path}"),
            Err(e) => eprintln!("无法写入结果文件 {path}：{e}"),
        }
    }
    ExitCode::SUCCESS
}

/// 剪贴板自检：用**正式代码**把四种格式真写进系统剪贴板再读回来。
///
/// 这是唯一能覆盖 `OpenClipboard` / `SetClipboardData` / `CF_HDROP` / `CF_DIBV5`
/// 这些"只有真调 Win32 才知道对不对"的路径的手段。单元测试只能覆盖字节布局。
///
/// 注意：会覆盖当前剪贴板内容。在没有交互式桌面会话的环境（服务、受限沙箱）里
/// `OpenClipboard` 会失败，此时明确报告"剪贴板不可用"并以专用退出码结束 ——
/// 那是环境限制，不是程序缺陷。
fn selftest_clipboard() -> ExitCode {
    use cliphist::image::dib::rgba_to_dibv5;
    use cliphist::image::thumb::encode_png_rgba;
    use cliphist::win::clipboard::{self, Content, Limits, Payload};

    let hwnd: windows_sys::Win32::Foundation::HWND = std::ptr::null_mut();
    let limits = Limits::default();
    let mut fails: Vec<String> = Vec::new();
    macro_rules! check {
        ($name:expr, $cond:expr) => {
            if $cond {
                println!("  PASS  {}", $name);
            } else {
                println!("  FAIL  {}", $name);
                fails.push($name.to_string());
            }
        };
    }

    println!("ClipHist 剪贴板自检（{}）", cliphist::VERSION);

    // ① 探针：先确认剪贴板到底能不能用
    let probe = format!("ClipHist 剪贴板自检 {}", std::process::id());
    if let Err(e) = clipboard::write(hwnd, &Payload::Text(&probe)) {
        println!("  剪贴板不可用：{e}");
        println!();
        println!("当前会话没有可用的剪贴板（常见于服务/受限沙箱/无交互桌面）。");
        println!("这不代表程序有缺陷 —— 请在正常桌面会话里重新运行本命令。");
        return ExitCode::from(8);
    }

    // ② 文本往返
    match clipboard::read(hwnd, &limits) {
        Content::Text(t) => check!("文本往返一致", t == probe),
        other => {
            println!("      实际读到：{other:?}");
            check!("文本往返一致", false);
        }
    }

    // ③ 文件往返（CF_HDROP + Preferred DropEffect）
    let dir = cliphist::win::paths::exe_dir();
    let paths = vec![
        dir.join("ClipHist.exe").display().to_string(),
        dir.display().to_string(),
    ];
    check!(
        "写入文件到剪贴板",
        clipboard::write(
            hwnd,
            &Payload::Files {
                paths: &paths,
                is_cut: false
            }
        )
        .is_ok()
    );
    match clipboard::read(hwnd, &limits) {
        Content::Files {
            paths: back,
            is_cut,
        } => {
            check!("文件路径往返一致", back == paths);
            check!("复制（非剪切）语义正确", !is_cut);
        }
        other => {
            println!("      实际读到：{other:?}");
            check!("文件路径往返一致", false);
        }
    }

    // ④ 图片往返：先只给 DIBV5，验证 DIB 通道
    let (w, h) = (48u32, 32u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.push((x * 5 % 256) as u8);
            rgba.push((y * 7 % 256) as u8);
            rgba.push(((x + y) * 3 % 256) as u8);
            rgba.push(255);
        }
    }
    let dib = rgba_to_dibv5(w, h, &rgba).unwrap_or_default();
    let png = encode_png_rgba(w, h, &rgba).unwrap_or_default();
    check!(
        "写入图片（仅 CF_DIBV5）",
        clipboard::write(
            hwnd,
            &Payload::Image {
                dibv5: &dib,
                png: &[]
            }
        )
        .is_ok()
    );
    match clipboard::read(hwnd, &limits) {
        Content::Image(img) => {
            check!("DIB 通道尺寸正确", img.width == w && img.height == h);
            check!("DIB 通道像素一致", img.rgba == rgba);
        }
        other => {
            println!("      实际读到：{other:?}");
            check!("DIB 通道像素一致", false);
        }
    }

    // ⑤ 图片往返：同时给 PNG，验证优先走 PNG 通道且无损
    check!(
        "写入图片（DIBV5 + PNG）",
        clipboard::write(
            hwnd,
            &Payload::Image {
                dibv5: &dib,
                png: &png
            }
        )
        .is_ok()
    );
    match clipboard::read(hwnd, &limits) {
        Content::Image(img) => check!("PNG 通道无损", img.rgba == rgba),
        other => {
            println!("      实际读到：{other:?}");
            check!("PNG 通道无损", false);
        }
    }

    // ⑥ "请勿记录"标记必须被尊重（密码管理器的场景）
    check!(
        "写入带忽略标记的内容",
        clipboard::write_registered(
            "Clipboard Viewer Ignore",
            &[1u8, 0, 0, 0],
            Some("这段内容不应该被记录")
        )
        .is_ok()
    );
    match clipboard::read(hwnd, &limits) {
        Content::Ignored => check!("忽略标记生效（不会被记录）", true),
        other => {
            println!("      实际读到：{other:?}");
            check!("忽略标记生效（不会被记录）", false);
        }
    }

    // 收尾：把剪贴板恢复成一句提示，别留下自检垃圾
    let _ = clipboard::write(
        hwnd,
        &Payload::Text("ClipHist 剪贴板自检已完成，这里原本的内容已被覆盖。"),
    );

    println!();
    if fails.is_empty() {
        println!("剪贴板自检通过。");
        ExitCode::SUCCESS
    } else {
        println!("剪贴板自检失败 {} 项：{}", fails.len(), fails.join("；"));
        ExitCode::from(9)
    }
}

/// 自检：不依赖桌面环境，验证存储层与图片编解码的关键路径。
///
/// 用的就是正式代码（`Store` / `dib` / `thumb`），因此能覆盖"数据库能不能开、
/// 去重对不对、DIB 与 PNG 往返是否无损"这些一旦坏掉就全盘失效的地方。
fn selftest() -> ExitCode {
    use cliphist::core::hash::hash_str;
    use cliphist::core::model::{CapturedItem, Kind, PathEntry, WriteOutcome};
    use cliphist::core::payload::hash_paths;
    use cliphist::core::settings::Settings;
    use cliphist::core::store::Store;
    use cliphist::image::dib::{dib_to_rgba, parse_dib, rgba_to_dibv5};
    use cliphist::image::thumb::{decode_png_rgba, encode_png_rgba};
    use std::fs;

    let root = cliphist::win::paths::exe_dir().join("selftest-tmp");
    if let Err(e) = fs::create_dir_all(&root) {
        eprintln!("无法创建自检目录 {}：{e}", root.display());
        return ExitCode::from(5);
    }
    let db_path = root.join("selftest.db");
    for suffix in ["", "-journal"] {
        let _ = fs::remove_file(format!("{}{suffix}", db_path.display()));
    }

    let mut fails: Vec<String> = Vec::new();
    macro_rules! check {
        ($name:expr, $cond:expr) => {
            if $cond {
                println!("  PASS  {}", $name);
            } else {
                println!("  FAIL  {}", $name);
                fails.push($name.to_string());
            }
        };
    }

    println!("ClipHist 自检（{}）", cliphist::VERSION);
    println!("数据目录：{}", root.display());

    let opened = Store::open_or_recover(&db_path);
    check!("打开/新建 data.db", opened.is_ok());
    let Ok((mut store, recovered)) = opened else {
        eprintln!("自检中止：数据库无法打开");
        return ExitCode::from(6);
    };
    check!("全新库没有触发恢复", recovered.is_none());

    // 文本：入库 + 去重
    let text = "自检文本 hello 世界".to_string();
    let item = CapturedItem::text(
        cliphist::core::filter::make_text_preview(&text, 200),
        text.clone(),
        hash_str(&text),
        Some("selftest.exe".into()),
    );
    let first = store.upsert(&item, None, 1_000);
    check!(
        "插入文本条目",
        matches!(first, Ok(WriteOutcome::Inserted(_)))
    );
    let again = store.upsert(&item, None, 2_000);
    check!(
        "重复内容被识别为同一条",
        matches!(again, Ok(WriteOutcome::Touched(_)))
    );
    check!(
        "去重后只有一条记录",
        store.stats().map(|(n, _)| n == 1).unwrap_or(false)
    );

    // 文件条目
    let entries = vec![
        PathEntry {
            path: r"C:\selftest\a.txt".into(),
            is_dir: false,
        },
        PathEntry {
            path: r"C:\selftest\子目录".into(),
            is_dir: true,
        },
    ];
    let files_item = CapturedItem::files(
        cliphist::core::filter::make_files_preview(&entries),
        entries.clone(),
        cliphist::core::payload::join_paths_text(
            &entries.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
        ),
        hash_paths(&entries),
        None,
    );
    let files_id = match store.upsert(&files_item, None, 3_000) {
        Ok(WriteOutcome::Inserted(id)) => Some(id),
        _ => None,
    };
    check!("插入文件条目", files_id.is_some());
    check!(
        "文件路径按序读回",
        files_id
            .map(|id| store.get_files(id).map(|f| f == entries).unwrap_or(false))
            .unwrap_or(false)
    );

    // 图片条目：PNG 与 DIB 往返
    let (w, h) = (64u32, 48u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.push((x % 256) as u8);
            rgba.push((y % 256) as u8);
            rgba.push(((x + y) % 256) as u8);
            rgba.push(255);
        }
    }
    let png = encode_png_rgba(w, h, &rgba);
    check!("PNG 编码", png.is_ok());
    let png = png.unwrap_or_default();
    check!(
        "PNG 往返无损",
        decode_png_rgba(&png)
            .map(|img| img.width == w && img.height == h && img.rgba == rgba)
            .unwrap_or(false)
    );

    let dib = rgba_to_dibv5(w, h, &rgba);
    check!("构造 CF_DIBV5", dib.is_some());
    let dib = dib.unwrap_or_default();
    let roundtrip_dib = parse_dib(&dib)
        .and_then(|info| dib_to_rgba(&dib, &info))
        .map(|(rw, rh, back)| rw == w && rh == h && back == rgba)
        .unwrap_or(false);
    check!("DIBV5 往返逐像素一致", roundtrip_dib);

    let mut image = CapturedItem::image(
        cliphist::core::filter::make_image_preview(w, h),
        cliphist::core::model::ImageData {
            width: w,
            height: h,
            rgba: rgba.clone(),
        },
        cliphist::core::payload::hash_image(w, h, &rgba),
        None,
    );
    image.img_w = w as i32;
    image.img_h = h as i32;
    let image_id = match store.upsert(&image, Some(png.clone()), 4_000) {
        Ok(WriteOutcome::Inserted(id)) => Some(id),
        _ => None,
    };
    check!("插入图片条目", image_id.is_some());
    check!(
        "图片 blob 读回一致",
        image_id
            .map(|id| store.get_blob(id).map(|b| b == Some(png)).unwrap_or(false))
            .unwrap_or(false)
    );
    check!(
        "图片缩略图状态为待生成",
        store
            .pending_thumbs(10)
            .map(|p| p.len() == 1)
            .unwrap_or(false)
    );

    // 列表与搜索
    check!(
        "列表返回 3 条",
        store
            .list_page("", 10)
            .map(|r| r.len() == 3)
            .unwrap_or(false)
    );
    check!(
        "搜索命中文本",
        store
            .list_page("hello", 10)
            .map(|r| r.len() == 1)
            .unwrap_or(false)
    );
    check!(
        "搜索命中文件名",
        store
            .list_page("a.txt", 10)
            .map(|r| r.len() == 1)
            .unwrap_or(false)
    );
    check!(
        "搜索结果类型正确",
        store
            .list_page("", 10)
            .map(|r| r.iter().any(|x| x.kind == Kind::Image))
            .unwrap_or(false)
    );

    // 保留策略
    let mut settings = Settings {
        max_items: 2,
        keep_days: 0,
        max_image_bytes: 0,
        ..Settings::default()
    };
    check!(
        "按条数修剪生效",
        store
            .prune(&settings, 10_000)
            .map(|rep| rep.by_count == 1 && store.stats().map(|(n, _)| n == 2).unwrap_or(false))
            .unwrap_or(false)
    );

    // 设置持久化
    settings.max_items = 77;
    check!("设置写入", settings.save(&store).is_ok());
    check!(
        "设置读回",
        Settings::load(&store)
            .map(|s| s.max_items == 77)
            .unwrap_or(false)
    );

    drop(store);
    let _ = fs::remove_file(&db_path);
    let _ = fs::remove_dir(&root);

    println!();
    if fails.is_empty() {
        println!("自检通过。");
        ExitCode::SUCCESS
    } else {
        println!("自检失败 {} 项：{}", fails.len(), fails.join("；"));
        ExitCode::from(7)
    }
}
