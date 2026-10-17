# 架构说明

## 1. 分层与依赖方向

```
        app.rs（控制器窗口过程 / 状态机）
        ↑            ↑              ↑
      ui/          win/           core/  ← 纯逻辑，零 Win32
   （自绘界面）  （Win32 直调）    image/ ← 纯编解码，零 Win32
```

依赖是单向的：`core` 与 `image` 不知道 Win32 的存在，`ui` 不知道数据库的存在，
`win` 不知道业务策略的存在。这条约束带来两个直接好处：

1. 最容易出错、最值得测的东西（去重、保留策略、DIB 字节布局、列表导航）全部可在
   `cargo test` 里覆盖，不需要桌面会话。
2. 界面与存储可以各自替换而互不影响。

唯一"知道所有事"的地方是 `app.rs`，它把三边接起来。

## 2. 线程模型

| 线程 | 职责 | 为什么这样分 |
|---|---|---|
| 主线程 | 所有窗口消息、剪贴板取数、绘制、写回剪贴板、模拟 Ctrl+V | 剪贴板与窗口 API 都要求在同一线程上按序调用 |
| 写线程（`cliphist-db`） | 独占一个 sqlite 连接：PNG 编码、缩略图生成、入库、保留策略修剪、`incremental_vacuum` | 一张 4K 截图的 PNG 编码要几十毫秒，放在主线程会让弹窗卡顿 |

两个连接同时打开同一个 `data.db`：主线程那个只读列表页，写线程那个只写。
SQLite 自己处理并发，`busy_timeout=2000` 兜底。

采集路径（`WM_CLIPBOARDUPDATE` → 入库）全程不阻塞界面：

```
WM_CLIPBOARDUPDATE
  → OpenClipboard（失败重试 5×20ms）        ← 微秒级
  → 读成 owned 的 Content                    ← 一次性拷贝
  → 过滤 / 生成预览 / 组装 CapturedItem      ← 微秒级
  → mpsc::Sender 投给写线程                  ← 立即返回
```

哈希刻意放在写线程算：文本 2 MB 以内还好，但一张 1200 万像素图的 RGBA 是 48 MB，
逐字节 FNV 要几十毫秒 —— 那是界面线程不能碰的开销。

## 3. 内存预算与手段

实测：空库 2.3 MB、500 条历史 2.7 MB（私有提交）。做到这一点靠的是四条：

1. **列表只加载预览**：`items.preview` 存前 200 字符，弹窗查询只取这个字段；
   正文 `text` 与图片 `blob` 只在真正要粘贴时读取，用完立刻释放。
   于是常驻内存与历史条数基本无关。
2. **缩略图 LRU 上限 24 张**：一屏最多十来行，缓存再多只是白占内存。
3. **SQLite 也省着用**：`cache_size=-512`（512KB，默认是 2MB/连接）、`mmap_size=0`
   （不建映射，私有内存可预测）、`temp_store=MEMORY`。
4. **图片入库即降采样**：长边 > 2560 先缩再存，回写时的 DIB 峰值也随之变小。

唯一可能到几十 MB 的瞬时分配是"读一张大截图的像素缓冲"，它用完立即释放，
并且用 `max_image_pixels`（默认 1200 万）在解码**之前**就卡住上限。

## 4. 数据库结构

单文件 `data.db`，刻意不用 WAL：WAL 会常驻 `-wal`/`-shm` 两个附属文件，
与"数据都在 data.db 里"的便携定位冲突。

```sql
meta(k, v)                       -- schema.version 与全部设置项（键值对，不产生独立配置文件）
items(
  id, kind,                      -- kind: 0=文本 1=文件 2=图片（数值不可改，改了破坏已有库）
  hash,                          -- FNV-1a 64，仅作去重索引
  preview,                       -- 前 200 字符，列表只读它
  text, byte_len,                -- 文本原文 / 文件路径换行拼接；byte_len 用于体积上限
  created_at, last_used_at, use_count,
  pinned,                        -- 置顶（"常用"），保留策略豁免
  thumb_state, thumb, blob,      -- 缩略图状态机 + PNG 本体
  source_exe,                    -- 来源进程名
  img_w, img_h, file_count
)
item_files(item_id, ordinal, path, is_dir)   -- 文件条目，外键 ON DELETE CASCADE
```

几个刻意的取舍：

- **哈希不是唯一索引**：FNV64 命中后一定逐字节比对内容，所以哈希碰撞只会多花一次查询，
  不会造成误去重。`hash` 用自实现而不是 `DefaultHasher`，是因为后者没有跨版本稳定性承诺，
  而 `data.db` 要跨程序升级存活 —— 算法一变，所有旧记录的去重索引就全失效。
- **图片比对用 `byte_len + 尺寸`**：逐字节比对一个几十 MB 的 BLOB 不值得，
  而 64 位哈希 + 长度 + 宽高三者同时相撞的概率可以忽略。
- **删除量累积到 200 行才 `incremental_vacuum`**：频繁整理会让便携版（U 盘）写入放大。

## 5. 剪贴板：格式与优先级

读的时候顺序是 **`CF_HDROP` → 图片(`PNG`/`CF_DIBV5`/`CF_DIB`) → `CF_UNICODETEXT`**，
这个顺序是必须的：资源管理器复制**图片文件**时可能同时提供 `CF_DIB` 缩略图，
若先取图片，用户"复制文件"的历史就会被记成一张图，粘出来也就不是文件了。

写回时一次 `EmptyClipboard` 会话里把多种格式都设上：

| 类型 | 写入的格式 | 理由 |
|---|---|---|
| 文本 | `CF_UNICODETEXT` | 通用 |
| 文件 | `CF_HDROP` + `Preferred DropEffect=COPY` + `CF_UNICODETEXT` | 资源管理器按 HDROP 粘贴出文件；`Preferred DropEffect` 保证是"复制"而不是"移动"；文本兜底让聊天软件也能粘贴出路径 |
| 图片 | `CF_DIBV5` + `CF_DIB` + 注册格式 `PNG` | DIBV5 兼容画图/Word/PS；PNG 让浏览器与现代应用拿到无损原图（且免一次解码） |

不记录的内容：对方设置了 `Clipboard Viewer Ignore` /
`ExcludeClipboardContentFromMonitorProcessing`（密码管理器的标准做法）时直接跳过。

自身写入抑制：写回剪贴板后记下 `GetClipboardSequenceNumber()`，
下一次 `WM_CLIPBOARDUPDATE` 若序号相同就整条忽略 —— 否则"粘贴"会被当成"复制"再记一遍，
形成自我循环。

## 6. 错误处理：一个必须区分的边界

`Store::open_or_recover` 把失败分成两类，这条边界是安全底线：

| 情况 | 判定 | 行为 |
|---|---|---|
| `SQLITE_NOTADB` / `SQLITE_CORRUPT` / `quick_check` 不通过 | `OpenFailure::Corrupt` | 把文件改名成 `data.db.corrupt-<时间戳>` 隔离保留，重建空库，并**用气泡如实告诉用户** |
| `SQLITE_BUSY`（另一个连接独占）/ 权限 / 路径不可达 | `OpenFailure::Unavailable` | 原样上抛，**绝不改动文件**；错误信息写明"文件没有被改动"以及该检查什么 |

早期版本把任何失败都当损坏，于是"另一个实例正在写"这种瞬时锁会把用户历史改名隔离。
现在有专门的回归测试（另一连接持有 `BEGIN EXCLUSIVE`）锁住这个行为。

`busy_timeout` 必须**最先**设置：否则紧接着的 `journal_mode` 在库被占用时会立刻返回 BUSY 而不是等待。

## 7. Win32 层的三个坑（都在代码里留了注释）
1. **弹窗的窗口过程不能被重入**。所以弹窗自己不弹菜单、不弹模态框 ——
   右键菜单交给控制器窗口去弹（`TrackPopupMenu` 会泵消息）。
   凡是会同步派发消息的动作（只有 `ShowWindow(SW_HIDE)`）都推迟到
   `&mut PopupState` 借用结束之后执行（见 `ui/popup/mod.rs` 的 `Outcome`/`dispatch`）。
   控制器那边则用 `thread_local + try_borrow_mut`：重入时退化为 `DefWindowProcW`，
   既不 panic 也不产生别名 `&mut`。
2. **`HWND` 是裸指针，不实现 `Send`**。写线程需要窗口句柄来投递"缩略图已生成"，
   所以跨线程只传 `isize` 地址，线程内再还原。
3. **GDI 画图不写 alpha 通道**。运行时绘制托盘图标时先在画布上填一个哨兵色，
   画完再按"颜色是否等于哨兵色"判定透明度 —— 直接用 GDI 输出的 alpha 会得到一张全透明图标。

另外两处与"正确性"而非"崩溃"有关的细节：

- 自动粘贴前必须**校验 `GetForegroundWindow()` 的实际结果**（`SetForegroundWindow`
  可能被系统的前台锁定规则静默拒绝），否则 Ctrl+V 会打进我们自己的弹窗。
  校验失败时用 `AttachThreadInput` 兜底；仍失败就降级为"内容留在剪贴板 + 气泡提示"。
- 前台进程名用 `PROCESS_QUERY_LIMITED_INFORMATION` 查询，对更高完整性级别的进程也能成功，
  拿不到就返回 `None`，绝不因此丢一条历史。

## 8. 启动流程与"绝不阻塞初始化"

顺序是有讲究的：

```
main
 ① CLI 模式先把 stdout 接回调用者终端（GUI 子系统进程默认没有 stdout）
 ② 设置进程 DPI 感知                  ← 必须在任何窗口之前
 ③ 单实例互斥体（已有实例就唤醒它并退出）
 ④ 注册三个窗口类 → 创建控制器窗口（隐藏）
      WM_CREATE：创建弹窗、设置窗口、App（开库、起写线程）
 ⑤ init_runtime：托盘 → 剪贴板监听 → 全局热键 → 注册唤醒消息
 ⑥ GetMessage 循环
```

一条硬规则：**第 ⑤ 步里任何一项失败都不许阻塞或退出**。

这条规则是踩出来的：早期版本在托盘创建失败时弹模态错误框，结果初始化卡在消息框上，
消息循环根本没开始跑，剪贴板通知一条也处理不了 —— 表现为"进程活着、内存正常、
但就是什么都不记录"，而且没有任何报错可看。现在的处理：

| 失败项 | 处理 |
|---|---|
| 托盘图标（受限令牌 / 外壳不可用时 `Shell_NotifyIcon` 会返回 `ERROR_ACCESS_DENIED`） | 记录 + 继续运行，列表页脚显示"托盘不可用"；热键弹窗照常可用 |
| 剪贴板监听 | 致命（没有它就完全没有功能）：弹消息框后退出 |
| 全局热键（组合键被占用） | 继续运行 + 托盘气泡提示，设置里可改键 |

## 9. 诊断日志（`--log`）

默认完全关闭（连文件都不打开，零开销）。加上 `--log clip.log` 后，采集链路的五段全部留痕：

```
tray: NIM_ADD = 0 (cbSize=976 lastError=5)
init: tray = FAILED
clipboard: WM_CLIPBOARDUPDATE seq=511 suppress=None
ingest: content=Text("…") source=Some("chrome.exe")
ingest: → store kind=Text preview="…"
writer: upsert kind=Text → Inserted(1)
```

设计上的一个细节：这个开关用**静态变量**保存文件句柄，而不是 App 的字段 ——
因为写日志会发生在 `&mut App` 被借用期间（例如 wndproc 内部），
若日志也要借 App 就会重入冲突。

它的价值在测试里已经兑现过一次：正是靠它才定位到"托盘失败 → 模态框 → 初始化卡死"
这条链路，否则现场看到的只是"程序在跑但不记录"。

## 10. 关键决策记录

| 决策 | 取舍 |
|---|---|
| 原生 Win32 而非 Tauri/WebView | 内存 2.3 MB vs 60–120 MB。代价是手写消息循环与自绘列表 |
| 不引入 comctl32 | 少一个 DLL、少一层控件对象，且不需要 v6 清单文件；代价是滚动条/列表全自己画 |
| 自绘设置面板而非按钮/下拉框 | 同上。整窗只有一个交互模型（点行、点值的左右半边），没有焦点管理与 Tab 顺序 |
| 搜索框用系统 EDIT 控件 | 中文输入法、光标、选区、右键菜单全部白送；自绘文本框要把这些重做一遍 |
| 静态 CRT | 去掉 `vcruntime140.dll` 依赖，真正做到"拷走就能跑"；体积多一点 |
| `journal_mode=DELETE` 而非 WAL | 保住"数据只在一个文件里"；牺牲一点写入吞吐（剪贴板场景完全够） |

## 附：`ui/popup` 的四文件结构（0.2.x 重构）

原先 `popup.rs` 一个文件 1825 行，命中测试、绘制、标题栏、消息过程混在一起，
"设置按钮点不到""右键窗口消失"这类 bug 都藏在这个夹缝里。现在：

| 文件 | 职责 |
|---|---|
| `popup/mod.rs` | 状态结构、定位计算、窗口类与消息过程、`Outcome`/`dispatch`、公开 API |
| `popup/nav.rs` | 列表导航与命中测试（**不碰 HWND**，纯函数，可单测） |
| `popup/view.rs` | 全部绘制：搜索行、列表行（含命中下划线、队列序号徽标）、空态、滚动条、页脚 |
| `popup/chrome.rs` | 标题栏按钮命中 `title_hit` + DWM 圆角 |

关键约束：**绘制与命中共用同一个 `title_hit`**；`nav` 层零 Win32 依赖。
