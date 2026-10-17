# 测试计划

原则：**能在没有桌面的环境里自动验证的，绝不留给手工；不能自动验证的，明确写清原因，
不装作测过。**

## 1. 四层验证

| 层 | 命令 | 覆盖 | 依赖 |
|---|---|---|---|
| 单元测试 | `cargo test` | 124 个用例：纯逻辑与纯编解码 | 无（只要文件系统） |
| 存储/编解码自检 | `ClipHist.exe --selftest` | 21 项：建库、去重、文件条目、图片、搜索、保留策略、设置持久化 | 无 |
| 剪贴板自检 | `ClipHist.exe --selftest-clipboard` | 11 项：真调 `OpenClipboard` / `SetClipboardData` 的文本、`CF_HDROP`、`CF_DIBV5`、注册 PNG、忽略标记 | 交互式桌面会话 |
| 界面渲染预览 | `ClipHist.exe --render-preview out.png` / `--render-settings out.png` | 用**正式绘制代码**把弹窗/设置窗口渲成 PNG，可加 `--preview-empty`、`--preview-query` | 无 |
| 采集链路诊断 | `ClipHist.exe --log clip.log` | 把"通知 → 取数 → 过滤 → 入库"五段全部写进日志，用于现场排障与端到端验证 | 无 |
| 内存实测 | `pwsh -File scripts\measure-memory.ps1` | 空库 / 预填 500 条 / 剪贴板 soak 三段采样与判定 | 第三部分需要剪贴板 |

界面预览这一层是本项目"看得见"的基础：原生 GUI 没法靠浏览器迭代，
而 `--render-preview` 渲的就是同一份 `render()`，所以截图即真实画面。
它已经抓到过一个真 bug：空态文字居中时用了"最大高度"而不是当前窗口高度，
结果 160px 高的空窗口里文字被推到窗口外（见 `ui/popup.rs` 的 `render_empty` 注释）。

## 2. 单元测试分布（124 个）

| 模块 | 用例 | 关键点 |
|---|---|---|
| `core::store` | 16 | 去重与 `use_count`、**哈希碰撞不误合并**、文件条目级联删除、图片缩略图状态机、置顶排序、预览→正文回退搜索、LIKE 转义、三种修剪边界、0 = 不限、**损坏隔离重建**、**占用不隔离** |
| `core::filter` | 8 | 预览折叠空白、按**字符**截断（多字节安全）、控制字符剔除、文件预览形态、`\\?\` 长路径取文件名、LIKE 转义、大小写不敏感匹配 |
| `core::payload` | 7 | `DROPFILES` 往返（含中文路径）、空列表仍带终止符、越界拒绝、`Preferred DropEffect` 往返、路径文本往返、路径哈希对顺序与目录标记敏感 |
| `image::dib` | 7 | `DIBV5` 往返逐像素一致（含垂直翻转）、带 alpha 往返、自上而下 24bpp、全 0 alpha 视为不透明、40 字节头 + 位域掩码、拒绝 8 位与截断输入、参数校验 |
| `image::thumb` | 7 | PNG 往返无损、拒绝垃圾输入、降采样长边与等比、**不放大**、整数面积平均（边界格必须平均而非最近邻）、缩略图体积上限 |
| `ui::popup` | 8 | 列表导航钳制与可视区跟随、空列表、滚动保持选择可见、y→行号命中、行矩形无缝、键盘动作表、负坐标 `lParam` 解码 |
| `ui::settings_window` | 8 | 行布局不重叠、命中测试、数值两半分割、关闭按钮位置、数值上下限（含"到 0 后保持 0"）、开关切换、热键捕获（单独 Ctrl 不算、Esc 取消）、清空需两次点击 |
| 其它 | 63 | 哈希固定向量、模型、设置解析与容错、保留策略饱和运算、主题度量化与 DPI、时间分桶与本地日期、GDI 内存 DC 生命周期与 LRU 淘汰、宽字符串、单实例互斥、DPI 缩放往返、路径探测、进程名、托盘回调解码 |

## 3. 自检覆盖矩阵

`--selftest` 跑的是**正式代码**（`Store`/`dib`/`thumb`），不是另写一套"测试专用"逻辑，
因此不会出现"测试通过但产品路径不同"的假验证：

```
打开/新建 data.db · 全新库未触发恢复 · 插入文本 · 重复内容识别为同一条 · 去重后仅 1 条
插入文件条目 · 文件路径按序读回 · PNG 编码 · PNG 往返无损
构造 CF_DIBV5 · DIBV5 往返逐像素一致 · 插入图片 · 图片 blob 读回一致 · 缩略图状态为待生成
列表返回 3 条 · 搜索命中文本 · 搜索命中文件名 · 搜索结果类型正确
按条数修剪生效 · 设置写入 · 设置读回
```

`--selftest-clipboard` 覆盖的是**只有真调 Win32 才知道对不对**的那部分：

```
文本往返一致 · 写入文件到剪贴板 · 文件路径往返一致 · 复制（非剪切）语义正确
写入图片（仅 CF_DIBV5） · DIB 通道尺寸正确 · DIB 通道像素一致
写入图片（DIBV5 + PNG） · PNG 通道无损 · 忽略标记生效
```

## 4. 采集链路的实测证据（以及本环境真的测不了的部分）

### 4.1 已经实测通过的（本机、当前环境）

剪贴板链路是"系统通知 → 取数 → 过滤 → 写线程 → 落库"五段，用 `--log` 诊断日志
逐段验证过（日志原文见下）。三类内容都真的落进了 `data.db`：

```
clipboard: WM_CLIPBOARDUPDATE seq=511 suppress=None
ingest: content=Text("端到端文本 E2E-715156551 中文与 emoji 🎯") source=Some("DeepSeek Harness.exe")
ingest: → store kind=Text preview="端到端文本 E2E-715156551 中文与 emoji 🎯"
writer: upsert kind=Text → Inserted(1)
clipboard: WM_CLIPBOARDUPDATE seq=517
ingest: content=Files { paths: ["...\\drop-test.txt"], is_cut: false }
writer: upsert kind=Files → Inserted(2)
clipboard: WM_CLIPBOARDUPDATE seq=519
ingest: content=Files { paths: ["...\\drop-test.txt"], is_cut: false }
writer: upsert kind=Files → Touched(2)          ← 重复内容正确去重
```

图片（`Clipboard::SetImage` 放入 160×90 位图）：

```
items 里出现 kind=2, img_w=160, img_h=90, thumb_state=1, blob=543 B, thumb=557 B
```

回写链路（`--selftest-clipboard`，真调 `SetClipboardData` 再读回来）：**11 项全通过** ——
文本往返、`CF_HDROP` 往返、`DIBV5` 逐像素一致、注册 PNG 无损、"请勿记录"标记生效。

### 4.2 仍然测不了的：交互流与托盘

| 项 | 为什么测不了 | 怎么办 |
|---|---|---|
| 热键 → 弹窗 → 单击 → 自动粘贴到别的程序 | 需要驱动真实键盘/鼠标与前台窗口 | 手工清单 A/B/C/D/F 组 |
| 托盘图标与托盘菜单 | 本沙箱给子进程的是**受限令牌**，`Shell_NotifyIcon(NIM_ADD)` 返回 `ERROR_ACCESS_DENIED(5)`（UIPI 拦住了向 Explorer 托盘窗口发消息） | 手工清单 A4/A5/F 组；代码已降级为"无托盘仍可运行，页脚提示" |
| 多显示器 / DPI 切换 / 管理员窗口降级 | 同上 | 手工清单 K6/K7/F4 |

### 4.3 这轮验证抓到的一个真 bug

第一轮实测发现"应用在跑、剪贴板正常、却一条都没记下来"。诊断日志给出根因：

```
tray: NIM_ADD = 0 (lastError=5)     ← 托盘创建失败
init: tray = FAILED
（没有后续任何 clipboard 行）
```

旧代码在托盘失败时弹**模态错误框**并准备退出 —— 模态框把初始化卡死，
消息循环根本没开始跑，于是剪贴板通知一条也处理不了。
现在托盘失败只记录并继续运行（列表页脚显示"托盘不可用"），
`--log` 则把五段链路全部留痕，现场排障不用再猜。

这也说明：**"没有报错"不等于"链路通"** —— 只有端到端落库才算数。

## 5. 回归重点

改代码时最容易踩坏、且已有测试锁住的地方，按重要性排序：

1. `Store::open_or_recover` 的**损坏 / 占用判定**（错判会丢用户历史）。
2. `CF_HDROP` 优先于 `CF_DIB` 的读顺序（错了会把"复制文件"记成一张图片）。
3. 自身写入抑制（`GetClipboardSequenceNumber` 比对）——错了会形成"粘贴→又记一条"的循环。
4. DIB 的自下而上翻转与全 0 alpha 视为不透明。
5. `core::hash` 的 FNV-1a 固定向量 —— 变了意味着 `data.db` 的去重索引失效。
6. 弹窗 `Outcome`/`dispatch` 的"借用结束后才 hide"约定（破了就是 UB/崩溃）。

## 6. 验收阈值

| 指标 | 阈值 | 当前实测 |
|---|---|---|
| 空闲私有提交内存 | ≤ 8 MB | 2.3 MB |
| 500 条历史相对空库的增量 | ≤ 3 MB | +0.4 MB |
| 产物外部依赖 | 无 VC++ 运行库 | 仅系统 DLL |
| `cargo test` | 全绿 | 124/124 |
| 自检 | 全绿 | 21/21 |
