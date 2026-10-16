//! `ClipHist.exe` 入口。
//!
//! 参数：
//! - `--autostart`：开机自启时由注册表拉起，安静启动（不弹首启气泡）。
//! - `--selftest`：跑一遍存储/编解码自检后退出（给脚本与验收用，不开窗口）。
//! - `--version` / `-v`：打印版本。

use std::process::ExitCode;

use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, TranslateMessage,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);

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
        eprintln!("GetModuleHandleW 失败");
        return ExitCode::from(2);
    }

    // ③ 注册三个窗口类：控制器（隐藏）、历史弹窗、设置窗口。
    if !cliphist::app::register_class(hinstance)
        || !cliphist::ui::popup::register_class(hinstance)
        || !cliphist::ui::settings_window::register_class(hinstance)
    {
        eprintln!("窗口类注册失败");
        return ExitCode::from(3);
    }

    // ④ 控制器窗口始终隐藏，只负责收消息（托盘回调、热键、剪贴板变化）。
    let Some(hwnd) = cliphist::app::create(hinstance) else {
        eprintln!("创建主窗口失败");
        return ExitCode::from(4);
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
    println!("{} {} —— 便携版剪贴板历史", cliphist::APP_TITLE, cliphist::VERSION);
    println!();
    println!("用法：ClipHist.exe [选项]");
    println!("  --autostart   开机自启模式（安静启动，不弹提示）");
    println!("  --selftest    跑存储与编解码自检后退出");
    println!("  --version     打印版本");
    println!();
    println!("数据文件：data.db（程序同目录；该目录不可写时回退到 %LOCALAPPDATA%\\ClipHist）");
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
    check!("插入文本条目", matches!(first, Ok(WriteOutcome::Inserted(_))));
    let again = store.upsert(&item, None, 2_000);
    check!("重复内容被识别为同一条", matches!(again, Ok(WriteOutcome::Touched(_))));
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
        store.pending_thumbs(10).map(|p| p.len() == 1).unwrap_or(false)
    );

    // 列表与搜索
    check!(
        "列表返回 3 条",
        store.list_page("", 10).map(|r| r.len() == 3).unwrap_or(false)
    );
    check!(
        "搜索命中文本",
        store.list_page("hello", 10).map(|r| r.len() == 1).unwrap_or(false)
    );
    check!(
        "搜索命中文件名",
        store.list_page("a.txt", 10).map(|r| r.len() == 1).unwrap_or(false)
    );
    check!(
        "搜索结果类型正确",
        store
            .list_page("", 10)
            .map(|r| r.iter().any(|x| x.kind == Kind::Image))
            .unwrap_or(false)
    );

    // 保留策略
    let mut settings = Settings::default();
    settings.max_items = 2;
    settings.keep_days = 0;
    settings.max_image_bytes = 0;
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
        Settings::load(&store).map(|s| s.max_items == 77).unwrap_or(false)
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
