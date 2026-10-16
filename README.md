# 剪贴板历史 · ClipHist

Windows 便携版剪贴板历史工具：记录**文本 / 文件 / 图片**，热键呼出，回车即粘回原窗口。
整个程序是一个 exe，所有数据只存在同目录的一个 `data.db` 里。

技术栈刻意选 **Rust + 原生 Win32**（`windows-sys` 直调 user32/gdi32/shell32），
不引入 WebView、不引入 GUI 框架、不引入 comctl32 —— 因为需求里写死了"运行内存占用越低越好"，
而这几样东西每一个都要几十 MB 的代价。

## 实测数据（本机 release 构建，`scripts\measure-memory.ps1`）

| 场景 | 私有提交内存 | 工作集 | 线程 | 句柄 |
|---|---|---|---|---|
| 空闲（空库） | **2.3 MB** | 14.6 MB | 5 | 168 |
| 空闲（500 条历史） | **2.7 MB** | 15.0 MB | 5 | 168 |

| 指标 | 实测 |
|---|---|
| 预填 500 条入库 | 742 ms（1.5 ms/条），`data.db` 224 KB |
| 查询最近 300 条 | 348 µs |
| 关键字搜索（含正文回退） | 302 µs |
| 产物大小 | 1686 KB |
| 外部依赖 | 仅系统 DLL：kernel32 / user32 / gdi32 / shell32 / msimg32 / advapi32（静态 CRT，**不依赖 vcruntime140.dll**） |

"500 条历史只多占 0.4 MB"不是巧合，而是设计出来的：列表只加载每条的前 200 字预览，
正文与图片按需读取、用完即释放；缩略图缓存上限 24 张；`data.db` 页缓存限制 512KB 并关闭内存映射。

## 快速开始

```powershell
# 构建便携版（跑测试 → release → 校验依赖 → 自检 → 打包到 dist-portable）
pwsh -File scripts\build.ps1

# 直接跑
.\dist-portable\ClipHist.exe
```

启动后常驻托盘，按 **Alt+V** 呼出历史列表。

| 按键 | 作用 |
|---|---|
| ↑ / ↓ | 选择 |
| Enter | 粘贴到原窗口 |
| Shift+Enter | 只放回剪贴板，不自动粘贴 |
| Ctrl+1 ~ 9 | 直接粘贴第 N 条 |
| Ctrl+P | 置顶 / 取消置顶（置顶＝常用，不会被自动清理） |
| Ctrl+D | 删除选中条目 |
| 右键 | 置顶 / 删除 / 打开 / 打开所在文件夹 / 复制路径 |
| Esc | 关闭 |

托盘菜单：显示历史 / 暂停记录 / 设置 / 开机自启 / 清空历史（保留置顶）/ 退出。

## 数据文件

`data.db` 就在 exe 同目录（单文件 SQLite，`journal_mode=DELETE`，空闲时不残留 `-wal`/`-journal`）。
整个目录拷到 U 盘或另一台电脑，历史与设置一起跟过去。

程序目录不可写时（例如放进 `C:\Program Files`）自动改存 `%LOCALAPPDATA%\ClipHist\data.db`，
并在设置窗口里显示**实际生效**的路径，同时用托盘气泡如实告知。

## 默认行为

- 去重：同一内容再次复制不会产生第二条，只累加使用次数并刷新时间。
- 保留策略：非置顶最多 500 条、30 天，图片总量 200 MB；置顶条目豁免。
- 大小上限：单条文本/路径 2 MB、图片 1200 万像素；超出**不记录并弹气泡**，绝不悄悄截断。
- 图片入库前长边超过 2560 会降采样，既是省库也是省回写时的瞬时内存。
- 不记录密码管理器标记了 `Clipboard Viewer Ignore` / `ExcludeClipboardContentFromMonitorProcessing` 的内容。
- 粘回文件时一律按"复制"写回（`DROPEFFECT_COPY`），绝不会把用户的文件移走。

## 目录结构

```
src/
  core/    纯逻辑：哈希、模型、SQLite 存储、保留策略、搜索、负载编解码   ← 无 Win32，可完整单测
  image/   纯编解码：DIB/DIBV5 解析与构造、缩放、PNG
  win/     Win32 直调：剪贴板、热键、托盘、注册表、单实例、DPI、进程查询
  ui/      自绘界面：GDI 原语、历史弹窗、设置窗口、主题、时间格式
  app.rs   状态机：控制器窗口过程，把采集/存储/UI/回写串起来
docs/      架构、界面设计、测试计划、手工验收清单
scripts/   build.ps1（打包）、measure-memory.ps1（内存实测）
packaging/ 便携版使用说明
```

## 文档

- [架构说明](docs/architecture.md) —— 分层、线程模型、数据流、数据库结构、内存预算与手段
- [界面设计](docs/design.md) —— 单色取向、反相选中、等宽数据字体、行布局与交互
- [测试计划](docs/test-plan.md) —— 测试分层、覆盖矩阵、以及**本环境无法验证的部分**
- [手工验收清单](docs/manual-checklist.md) —— 逐条步骤与预期结果

## 自己验证

```powershell
cargo test                                  # 124 个单元测试
.\dist-portable\ClipHist.exe --selftest     # 存储与图片编解码自检（21 项）
.\dist-portable\ClipHist.exe --selftest-clipboard   # 剪贴板读写自检（需桌面会话）
pwsh -File scripts\measure-memory.ps1       # 内存实测（三段采样 + 判定）
```

## 已知限制

- 仅支持 Windows 10 1809+ / Windows 11。
- 目标窗口以管理员权限运行时，Windows 的 UIPI 会丢弃模拟按键：此时内容仍会放进剪贴板，
  并弹气泡提示手动按 Ctrl+V（让本程序也以管理员身份运行可解决）。
- 默认热键 `Alt+V`（避开被系统占用的 `Win+V` 与编辑器里的 `Ctrl+Shift+V`），可在设置里改。
- `data.db` 是**明文**存储的，剪贴板里可能有密码、验证码，请勿分享该文件。

## 许可

MIT
