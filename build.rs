//! 构建脚本：给 exe 嵌入 Windows VERSIONINFO 资源。
//!
//! 为什么需要：Rust 默认不写版本资源，右键 → 属性 → 详细信息里「文件版本」是空的。
//! 对用户来说，这意味着拿到一个 exe 后**没法一眼确认它是哪个版本**（今晚就因此被误判成
//! "没替换"）。写进去之后，资源管理器里能直接看到 0.2.1 / 产品名 / 版权。
//!
//! 资源编译失败不阻断构建：宁可少一点元数据，也不能让整个项目编不出来。

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // 只有 Windows 目标才需要（本项目只发布 Windows，但保持构建脚本本身可移植）。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set("ProductName", "ClipHist");
    res.set("FileDescription", "剪贴板历史 — 极简 Windows 剪贴板历史");
    res.set("CompanyName", "hunterlihao");
    res.set("LegalCopyright", "MIT License (c) 2026 hunterlihao");
    res.set("OriginalFilename", "ClipHist.exe");
    res.set("InternalName", "ClipHist");

    // 版本号由 winresource 从 CARGO_PKG_VERSION 自动填入 FileVersion / ProductVersion。
    if let Err(e) = res.compile() {
        println!("cargo:warning=VERSIONINFO 未能嵌入（不影响功能）：{e}");
    }
}
