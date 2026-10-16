//! SQLite 存储层。整个程序的持久化只有这一个文件（`data.db`）。
//!
//! 低内存取向的 pragma 组合在 [`Store::open_or_recover`] 里集中设置：
//! - `cache_size = -512`：512KB 页缓存上限（默认是 2MB/连接，两个连接就 4MB）。
//! - `mmap_size = 0`：不建文件映射，私有内存可预测、不会被大库撑大工作集。
//! - `journal_mode = DELETE`：提交后不留 `-wal` / `-shm`，符合"数据只在一个文件里"。
//! - `auto_vacuum = INCREMENTAL` + 按需 `incremental_vacuum`：删完能把页还给系统。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

use crate::core::filter::{escape_like, SearchPlan};
use crate::core::model::{CapturedItem, ItemRow, Kind, PathEntry, WriteOutcome};
use crate::core::retention::{self, VACUUM_ROW_THRESHOLD};
use crate::core::settings::Settings;

/// 当前 schema 版本。改动表结构时必须 +1 并补迁移分支。
pub const SCHEMA_VERSION: i64 = 1;

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
  k TEXT PRIMARY KEY,
  v TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS items (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  kind         INTEGER NOT NULL,
  hash         INTEGER NOT NULL,
  preview      TEXT    NOT NULL,
  text         TEXT,
  byte_len     INTEGER NOT NULL,
  created_at   INTEGER NOT NULL,
  last_used_at INTEGER NOT NULL,
  use_count    INTEGER NOT NULL DEFAULT 0,
  pinned       INTEGER NOT NULL DEFAULT 0,
  thumb_state  INTEGER NOT NULL DEFAULT 0,
  thumb        BLOB,
  blob         BLOB,
  source_exe   TEXT,
  img_w        INTEGER NOT NULL DEFAULT 0,
  img_h        INTEGER NOT NULL DEFAULT 0,
  file_count   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_items_hash   ON items(hash);
CREATE INDEX IF NOT EXISTS idx_items_recent ON items(pinned DESC, last_used_at DESC);
CREATE INDEX IF NOT EXISTS idx_items_thumbs ON items(thumb_state) WHERE thumb_state = 0;
CREATE TABLE IF NOT EXISTS item_files (
  item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL,
  path    TEXT NOT NULL,
  is_dir  INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (item_id, ordinal)
);
"#;

/// 一次清理的结果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub by_count: usize,
    pub by_age: usize,
    pub by_image_bytes: usize,
    pub vacuumed: bool,
}

impl PruneReport {
    pub fn total(&self) -> usize {
        self.by_count + self.by_age + self.by_image_bytes
    }
}

/// 打开数据文件失败的原因。
///
/// **区分这两者是安全底线**：只有"真损坏"才允许隔离重建；
/// "被占用 / 无权限 / 路径不可达"绝不能改名或删除用户的库 ——
/// SQLite 在库被另一个连接独占时返回的是 BUSY，若把它当成损坏，
/// 一次瞬时锁就会让用户的历史"消失"（被改名成 corrupt 文件）。
#[derive(Debug)]
pub enum OpenFailure {
    Corrupt { path: PathBuf, detail: String },
    Unavailable { path: PathBuf, detail: String },
}

impl std::fmt::Display for OpenFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenFailure::Corrupt { path, detail } => {
                write!(f, "数据文件已损坏：{}\n原因：{detail}", path.display())
            }
            OpenFailure::Unavailable { path, detail } => write!(
                f,
                "无法打开数据文件：{}\n原因：{detail}\n\
                 （文件没有被改动。若程序已在运行，请先退出那个实例；\
                 也请检查文件是否被其他程序占用或目录是否可写。）",
                path.display()
            ),
        }
    }
}

impl std::error::Error for OpenFailure {}

/// 把一个 rusqlite 错误归类：只有 NOTADB / CORRUPT 才算"真损坏"。
fn classify(path: &Path, e: rusqlite::Error) -> OpenFailure {
    if let rusqlite::Error::SqliteFailure(err, ref msg) = e {
        use rusqlite::ErrorCode;
        if matches!(
            err.code,
            ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt
        ) {
            return OpenFailure::Corrupt {
                path: path.to_path_buf(),
                detail: msg.clone().unwrap_or_else(|| e.to_string()),
            };
        }
    }
    OpenFailure::Unavailable {
        path: path.to_path_buf(),
        detail: e.to_string(),
    }
}

/// 数据库连接 + 表结构。读连接与写连接各持有一个实例（SQLite 自己处理并发）。
pub struct Store {
    conn: Connection,
    path: PathBuf,
    /// 自上次 `incremental_vacuum` 以来删除的行数，用于决定何时整理空闲页。
    deleted_since_vacuum: usize,
}

impl Store {
    /// 打开数据库；**仅在确认损坏时**隔离旧文件并重建。
    ///
    /// 返回 `(store, Some(隔离文件路径))` 表示发生过恢复 —— 调用方应当把这件事告诉用户，
    /// 而不是静默丢历史。
    pub fn open_or_recover(db_path: &Path) -> Result<(Store, Option<PathBuf>), OpenFailure> {
        if let Some(dir) = db_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }

        if db_path.exists() {
            match Store::open_inner(db_path) {
                Ok(store) => return Ok((store, None)),
                // 被占用/无权限：原样上抛，绝不碰用户的文件。
                Err(fail @ OpenFailure::Unavailable { .. }) => return Err(fail),
                Err(OpenFailure::Corrupt { .. }) => {
                    let stamp = now_ms();
                    let quarantine = db_path.with_extension(format!("db.corrupt-{stamp}"));
                    // 第一次改名失败就退化成删除，保证程序总能起来。
                    if std::fs::rename(db_path, &quarantine).is_err() {
                        let _ = std::fs::remove_file(db_path);
                    }
                    for side in ["-journal", "-wal", "-shm"] {
                        let p = PathBuf::from(format!("{}{side}", db_path.display()));
                        let _ = std::fs::remove_file(p);
                    }
                    let store = Store::open_inner(db_path)?;
                    return Ok((store, Some(quarantine)));
                }
            }
        }

        let store = Store::open_inner(db_path)?;
        Ok((store, None))
    }

    fn open_inner(db_path: &Path) -> Result<Store, OpenFailure> {
        let conn = Connection::open(db_path).map_err(|e| classify(db_path, e))?;
        // busy_timeout 必须**最先**设置：否则紧接着的 journal_mode 在库被其他连接
        // 占用时会立刻返回 BUSY，而不是等待。
        conn.execute_batch("PRAGMA busy_timeout=2000;")
            .map_err(|e| classify(db_path, e))?;
        // journal_mode 会返回一行结果，必须用 query_row 读掉。
        let _: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))
            .map_err(|e| classify(db_path, e))?;
        conn.execute_batch(
            "PRAGMA synchronous=NORMAL;
             PRAGMA cache_size=-512;
             PRAGMA mmap_size=0;
             PRAGMA foreign_keys=ON;
             PRAGMA temp_store=MEMORY;",
        )
        .map_err(|e| classify(db_path, e))?;

        let store = Store {
            conn,
            path: db_path.to_path_buf(),
            deleted_since_vacuum: 0,
        };
        store.migrate().map_err(|e| classify(db_path, e))?;

        // 完整性检查放在最后：只有这里失败才算"真损坏"。
        let ok: String = store
            .conn
            .query_row("PRAGMA quick_check", [], |r| r.get(0))
            .map_err(|e| classify(db_path, e))?;
        if ok != "ok" {
            return Err(OpenFailure::Corrupt {
                path: db_path.to_path_buf(),
                detail: format!("完整性检查未通过：{ok}"),
            });
        }
        Ok(store)
    }

    /// 建表 / 按版本迁移。版本比程序新时直接报错，绝不擅自降级写坏数据。
    fn migrate(&self) -> rusqlite::Result<()> {
        // 注意：sqlite_master 的列是 name/type/sql，这里只能用 name 判断表是否存在。
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='meta'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let version: i64 = if existing.is_some() {
            self.get_meta("schema.version")?
                .and_then(|v| v.parse().ok())
                .unwrap_or(0)
        } else {
            0
        };

        if version > SCHEMA_VERSION {
            return Err(rusqlite::Error::InvalidQuery);
        }

        self.conn.execute_batch(SCHEMA_SQL)?;
        if version < SCHEMA_VERSION {
            // v1 是初版，只需记录版本号；后续版本在此追加 ALTER TABLE 分支。
            self.set_meta("schema.version", &SCHEMA_VERSION.to_string())?;
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    // ---------------------------------------------------------------- meta

    pub fn get_meta(&self, key: &str) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row("SELECT v FROM meta WHERE k=?1", params![key], |r| r.get(0))
            .optional()
    }

    pub fn set_meta(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO meta(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            params![key, value],
        )?;
        tx.commit()
    }

    pub fn all_meta(&self) -> rusqlite::Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare("SELECT k,v FROM meta")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    pub fn set_meta_pairs(&self, pairs: &[(&str, String)]) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO meta(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            )?;
            for (k, v) in pairs {
                stmt.execute(params![k, v])?;
            }
        }
        tx.commit()
    }

    // -------------------------------------------------------------- 写入

    /// 插入或触摸（去重）。
    ///
    /// `blob` 只有图片条目会传（已编码好的 PNG）；文本/文件为 `None`。
    /// 返回 [`WriteOutcome`]，其中 `Touched` 表示内容已存在 —— 此时只更新
    /// `last_used_at` 与 `use_count`，**不会**产生第二条记录。
    pub fn upsert(
        &mut self,
        item: &CapturedItem,
        blob: Option<Vec<u8>>,
        now: i64,
    ) -> rusqlite::Result<WriteOutcome> {
        let tx = self.conn.transaction()?;
        let existing = find_duplicate(&tx, item)?;
        if let Some(id) = existing {
            tx.execute(
                "UPDATE items SET last_used_at=?2, use_count=use_count+1 WHERE id=?1",
                params![id, now],
            )?;
            tx.commit()?;
            return Ok(WriteOutcome::Touched(id));
        }

        let thumb_state = if item.kind == Kind::Image { 0 } else { 1 };
        tx.execute(
            "INSERT INTO items
               (kind, hash, preview, text, byte_len, created_at, last_used_at, use_count,
                pinned, thumb_state, thumb, blob, source_exe, img_w, img_h, file_count)
             VALUES (?1,?2,?3,?4,?5,?6,?6,0,0,?7,?8,?9,?10,?11,?12,?13)",
            params![
                item.kind.as_i64(),
                item.hash as i64,
                item.preview,
                item.text,
                item.byte_len,
                now,
                thumb_state,
                Option::<Vec<u8>>::None, // thumb 由后续 set_thumb 补写
                blob,
                item.source_exe,
                item.img_w,
                item.img_h,
                item.files.len() as i64,
            ],
        )?;
        let id = tx.last_insert_rowid();

        if !item.files.is_empty() {
            let mut stmt = tx.prepare(
                "INSERT INTO item_files(item_id, ordinal, path, is_dir) VALUES(?1,?2,?3,?4)",
            )?;
            for (i, f) in item.files.iter().enumerate() {
                stmt.execute(params![id, i as i64, f.path, f.is_dir as i64])?;
            }
        }

        tx.commit()?;
        Ok(WriteOutcome::Inserted(id))
    }

    /// 补写缩略图。`thumb == None` 表示生成失败，标记为 2 以免每次启动重试。
    pub fn set_thumb(&self, id: i64, thumb: Option<Vec<u8>>) -> rusqlite::Result<()> {
        let state = if thumb.is_some() { 1 } else { 2 };
        self.conn.execute(
            "UPDATE items SET thumb=?2, thumb_state=?3 WHERE id=?1",
            params![id, thumb, state],
        )?;
        Ok(())
    }

    /// 待生成缩略图的图片（程序上次被杀掉时会留在这里，启动后补做）。
    pub fn pending_thumbs(&self, limit: i64) -> rusqlite::Result<Vec<(i64, Vec<u8>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, blob FROM items
             WHERE kind=2 AND thumb_state=0 AND blob IS NOT NULL
             ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE items SET pinned=?2 WHERE id=?1",
            params![id, pinned as i64],
        )?;
        Ok(())
    }

    pub fn get_pinned(&self, id: i64) -> rusqlite::Result<bool> {
        let v: Option<i64> = self
            .conn
            .query_row("SELECT pinned FROM items WHERE id=?1", params![id], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(v.unwrap_or(0) != 0)
    }

    /// 被再次使用（粘贴/复制）时刷新：使用次数 +1、最近使用时间更新。
    ///
    /// 刻意**不动** `created_at`：那是最初复制的时间，"这条是什么时候进来的"不该被改写。
    pub fn touch_last_used(&self, id: i64, now: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE items SET last_used_at=?2, use_count=use_count+1 WHERE id=?1",
            params![id, now],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: i64) -> rusqlite::Result<usize> {
        let n = self
            .conn
            .execute("DELETE FROM items WHERE id=?1", params![id])?;
        // 外键级联在本连接上生效（PRAGMA foreign_keys=ON），无需手工删 item_files。
        Ok(n)
    }

    /// 清空历史。`keep_pinned=true` 时保留置顶（"常用文件"）条目。
    pub fn clear(&self, keep_pinned: bool) -> rusqlite::Result<usize> {
        let n = if keep_pinned {
            self.conn.execute("DELETE FROM items WHERE pinned=0", [])?
        } else {
            self.conn.execute("DELETE FROM items", [])?
        };
        Ok(n)
    }

    // -------------------------------------------------------------- 读取

    /// 列表页。`query` 为空时返回最近的条目（置顶优先）。
    pub fn list_page(&self, query: &str, limit: i64) -> rusqlite::Result<Vec<ItemRow>> {
        let q = query.trim();
        if q.is_empty() {
            self.query_rows(SELECT_ROWS, params![limit])
        } else {
            let pattern = format!("%{}%", escape_like(q));
            let rows = self.query_rows(
                "SELECT id, kind, preview, created_at, last_used_at, use_count, pinned,
                        source_exe, file_count, img_w, img_h, thumb_state
                 FROM items
                 WHERE preview LIKE ?1 ESCAPE '\\'
                 ORDER BY pinned DESC, last_used_at DESC
                 LIMIT ?2",
                params![pattern, limit],
            )?;
            if !rows.is_empty() {
                return Ok(rows);
            }
            // 预览没命中才回退全文搜索，且只在最近的、体积可控的条目里找。
            let plan = SearchPlan::default();
            if !plan.fallback_to_text {
                return Ok(rows);
            }
            let pattern = format!("%{}%", escape_like(q));
            self.query_rows(
                "SELECT id, kind, preview, created_at, last_used_at, use_count, pinned,
                        source_exe, file_count, img_w, img_h, thumb_state
                 FROM (SELECT * FROM items
                       WHERE byte_len <= ?2
                       ORDER BY last_used_at DESC
                       LIMIT ?3)
                 WHERE text LIKE ?1 ESCAPE '\\'
                 ORDER BY pinned DESC, last_used_at DESC
                 LIMIT ?4",
                params![
                    pattern,
                    plan.fallback_max_text_len,
                    plan.fallback_scan_limit,
                    limit
                ],
            )
        }
    }

    fn query_rows<P: rusqlite::Params>(&self, sql: &str, p: P) -> rusqlite::Result<Vec<ItemRow>> {
        let mut stmt = self.conn.prepare_cached(sql)?;
        let rows = stmt.query_map(p, |r| {
            Ok(ItemRow {
                id: r.get(0)?,
                kind: Kind::from_i64(r.get(1)?).unwrap_or(Kind::Text),
                preview: r.get(2)?,
                created_at: r.get(3)?,
                last_used_at: r.get(4)?,
                use_count: r.get(5)?,
                pinned: r.get::<_, i64>(6)? != 0,
                source_exe: r.get(7)?,
                file_count: r.get(8)?,
                img_w: r.get(9)?,
                img_h: r.get(10)?,
                thumb_state: r.get(11)?,
            })
        })?;
        rows.collect()
    }

    /// 取正文（只在真正要粘贴/展示时才读，读完即释放）。
    pub fn get_text(&self, id: i64) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row("SELECT text FROM items WHERE id=?1", params![id], |r| {
                r.get(0)
            })
            .optional()
            .map(|v| v.flatten())
    }

    pub fn get_blob(&self, id: i64) -> rusqlite::Result<Option<Vec<u8>>> {
        self.conn
            .query_row("SELECT blob FROM items WHERE id=?1", params![id], |r| {
                r.get(0)
            })
            .optional()
            .map(|v| v.flatten())
    }

    pub fn get_thumb(&self, id: i64) -> rusqlite::Result<Option<Vec<u8>>> {
        self.conn
            .query_row("SELECT thumb FROM items WHERE id=?1", params![id], |r| {
                r.get(0)
            })
            .optional()
            .map(|v| v.flatten())
    }

    pub fn get_files(&self, id: i64) -> rusqlite::Result<Vec<PathEntry>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, is_dir FROM item_files WHERE item_id=?1 ORDER BY ordinal")?;
        let rows = stmt.query_map(params![id], |r| {
            Ok(PathEntry {
                path: r.get(0)?,
                is_dir: r.get::<_, i64>(1)? != 0,
            })
        })?;
        rows.collect()
    }

    pub fn get_kind(&self, id: i64) -> rusqlite::Result<Option<Kind>> {
        let v: Option<i64> = self
            .conn
            .query_row("SELECT kind FROM items WHERE id=?1", params![id], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(v.and_then(Kind::from_i64))
    }

    /// 条目总数与数据库文件字节数（设置窗口显示用）。
    pub fn stats(&self) -> rusqlite::Result<(i64, i64)> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))?;
        let bytes = std::fs::metadata(&self.path)
            .map(|m| m.len() as i64)
            .unwrap_or(0);
        Ok((count, bytes))
    }

    // ------------------------------------------------------------ 保留策略

    /// 按设置清理。返回删除明细；`vacuumed` 表示本次顺带整理了空闲页。
    pub fn prune(&mut self, s: &Settings, now: i64) -> rusqlite::Result<PruneReport> {
        let mut report = PruneReport::default();
        let tx = self.conn.transaction()?;

        if retention::over_item_budget(unpinned_count(&tx)?, s.max_items) {
            report.by_count = tx.execute(
                "DELETE FROM items
                 WHERE pinned=0 AND id NOT IN (
                   SELECT id FROM items WHERE pinned=0
                   ORDER BY last_used_at DESC LIMIT ?1
                 )",
                params![s.max_items],
            )?;
        }

        if let Some(cutoff) = retention::cutoff_ms(s.keep_days, now) {
            report.by_age = tx.execute(
                "DELETE FROM items WHERE pinned=0 AND created_at < ?1",
                params![cutoff],
            )?;
        }

        let image_bytes: i64 = tx.query_row(
            "SELECT COALESCE(SUM(LENGTH(blob)),0) FROM items WHERE kind=2 AND pinned=0",
            [],
            |r| r.get(0),
        )?;
        if retention::over_image_budget(image_bytes, s.max_image_bytes) {
            // 窗口函数按 last_used_at 倒序累加，把超预算的最旧图片整批删掉。
            report.by_image_bytes = tx.execute(
                "DELETE FROM items WHERE pinned=0 AND kind=2 AND id IN (
                   SELECT id FROM (
                     SELECT id, SUM(LENGTH(blob)) OVER (ORDER BY last_used_at DESC) AS cum
                     FROM items WHERE pinned=0 AND kind=2
                   ) WHERE cum > ?1
                 )",
                params![s.max_image_bytes],
            )?;
        }

        tx.commit()?;

        self.deleted_since_vacuum += report.total();
        if retention::should_incremental_vacuum(self.deleted_since_vacuum, VACUUM_ROW_THRESHOLD) {
            self.incremental_vacuum()?;
            report.vacuumed = true;
            self.deleted_since_vacuum = 0;
        }
        Ok(report)
    }

    /// 把空闲页还给操作系统。只对 `auto_vacuum=INCREMENTAL` 的库有效。
    pub fn incremental_vacuum(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("PRAGMA incremental_vacuum;")?;
        Ok(())
    }
}

const SELECT_ROWS: &str = "SELECT id, kind, preview, created_at, last_used_at, use_count, pinned,
                                  source_exe, file_count, img_w, img_h, thumb_state
                           FROM items
                           ORDER BY pinned DESC, last_used_at DESC
                           LIMIT ?1";

fn unpinned_count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM items WHERE pinned=0", [], |r| {
        r.get(0)
    })
}

/// 在**同一个事务内**查找重复内容。
///
/// 哈希只当索引：命中候选后按类型逐项比对内容，因此 FNV64 碰撞不会造成误去重。
/// 图片比对 `byte_len + 尺寸`（逐字节比对一个可能几十 MB 的 BLOB 不值得，
/// 而 64 位哈希 + 长度 + 宽高三者同时相撞的概率可以忽略）。
fn find_duplicate(
    tx: &rusqlite::Transaction<'_>,
    item: &CapturedItem,
) -> rusqlite::Result<Option<i64>> {
    let mut stmt = tx.prepare_cached(
        "SELECT id, byte_len, img_w, img_h, text FROM items
         WHERE kind=?1 AND hash=?2
         ORDER BY last_used_at DESC LIMIT 8",
    )?;
    let mut rows = stmt.query(params![item.kind.as_i64(), item.hash as i64])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let byte_len: i64 = row.get(1)?;
        let img_w: i32 = row.get(2)?;
        let img_h: i32 = row.get(3)?;
        let text: Option<String> = row.get(4)?;
        let same = match item.kind {
            Kind::Text | Kind::Files => text.as_deref() == item.text.as_deref(),
            Kind::Image => byte_len == item.byte_len && img_w == item.img_w && img_h == item.img_h,
        };
        if same {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// 当前毫秒时间戳。
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::ImageData;

    /// 测试库统一放在工作区 `target/testdbs`：
    /// 比 `%TEMP%` 更可控（不依赖外部目录的可写性与 ACL），也不会把临时库散到用户机器上。
    fn test_dir() -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("testdbs");
        std::fs::create_dir_all(&dir).expect("创建测试数据目录");
        dir
    }

    fn tmp_db(tag: &str) -> PathBuf {
        let p = test_dir().join(format!("{tag}-{}.db", std::process::id()));
        for suffix in ["", "-journal"] {
            let _ = std::fs::remove_file(PathBuf::from(format!("{}{suffix}", p.display())));
        }
        p
    }

    /// 环境诊断：确认"测试进程能写哪里"。仅用于排查沙箱/ACL 问题。
    #[test]
    fn diagnostic_write_locations() {
        let t = std::env::temp_dir().join("cliphist-diag-probe.txt");
        println!("std write temp: {:?}", std::fs::write(&t, b"x"));
        println!("std remove temp: {:?}", std::fs::remove_file(&t));

        let db_tmp = std::env::temp_dir().join("cliphist-diag-temp.db");
        let _ = std::fs::remove_file(&db_tmp);
        println!(
            "sqlite temp: {:?}",
            rusqlite::Connection::open(&db_tmp).map(|_| "OK")
        );
        let _ = std::fs::remove_file(&db_tmp);

        let db_ws = test_dir().join("diag-workspace.db");
        let _ = std::fs::remove_file(&db_ws);
        println!(
            "sqlite workspace: {:?}",
            rusqlite::Connection::open(&db_ws).map(|_| "OK")
        );
    }

    fn open(tag: &str) -> (Store, PathBuf) {
        let p = tmp_db(tag);
        let (s, rec) = Store::open_or_recover(&p).unwrap();
        assert!(rec.is_none());
        (s, p)
    }

    fn text_item(pv: &str, body: &str) -> CapturedItem {
        CapturedItem::text(
            pv.to_string(),
            body.to_string(),
            crate::core::hash::hash_str(body),
            Some("notepad.exe".into()),
        )
    }

    #[test]
    fn schema_and_pragmas_are_applied() {
        let (s, p) = open("schema");
        let jm: String = s
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(jm.to_lowercase(), "delete");
        assert_eq!(s.get_meta("schema.version").unwrap().as_deref(), Some("1"));
        assert_eq!(s.stats().unwrap().0, 0);
        drop(s);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn duplicate_is_touched_not_inserted() {
        let (mut s, p) = open("dup");
        let item = text_item("hello", "hello");
        let a = s.upsert(&item, None, 1000).unwrap();
        let b = s.upsert(&item, None, 2000).unwrap();
        assert!(matches!(a, WriteOutcome::Inserted(_)));
        assert_eq!(a, WriteOutcome::Inserted(1));
        assert_eq!(b, WriteOutcome::Touched(1));
        assert_eq!(s.stats().unwrap().0, 1);

        let row = &s.list_page("", 10).unwrap()[0];
        assert_eq!(row.use_count, 1, "第二次出现累加使用次数");
        assert_eq!(row.last_used_at, 2000, "触摸会刷新最近使用时间");
        assert_eq!(row.created_at, 1000, "创建时间保持不变");
        drop(s);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn hash_collision_does_not_merge_different_content() {
        let (mut s, p) = open("collide");
        // 人为制造 hash 相同、内容不同的两条：必须各自入库。
        let mut a = text_item("aaa", "aaa");
        a.hash = 42;
        let mut b = text_item("bbb", "bbb");
        b.hash = 42;
        s.upsert(&a, None, 1).unwrap();
        s.upsert(&b, None, 2).unwrap();
        assert_eq!(s.stats().unwrap().0, 2);
        drop(s);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn files_are_stored_with_entries() {
        let (mut s, p) = open("files");
        let entries = vec![
            PathEntry {
                path: r"C:\a\one.txt".into(),
                is_dir: false,
            },
            PathEntry {
                path: r"C:\a\dir".into(),
                is_dir: true,
            },
        ];
        let text = crate::core::payload::join_paths_text(
            &entries.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
        );
        let item = CapturedItem::files(
            "one.txt、dir\\".into(),
            entries.clone(),
            text,
            crate::core::payload::hash_paths(&entries),
            None,
        );
        let id = match s.upsert(&item, None, 5).unwrap() {
            WriteOutcome::Inserted(id) => id,
            other => panic!("{other:?}"),
        };
        assert_eq!(s.get_files(id).unwrap(), entries);
        let row = &s.list_page("", 10).unwrap()[0];
        assert_eq!(row.kind, Kind::Files);
        assert_eq!(row.file_count, 2);
        // 删条目必须级联删掉 item_files（外键 + ON DELETE CASCADE）。
        s.delete(id).unwrap();
        assert!(s.get_files(id).unwrap().is_empty());
        drop(s);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn images_keep_blob_and_thumb_pending() {
        let (mut s, p) = open("img");
        let img = ImageData {
            width: 4,
            height: 3,
            rgba: vec![7u8; 4 * 3 * 4],
        };
        let mut item = CapturedItem::image("图片 4×3".into(), img, 999, None);
        item.img_w = 4;
        item.img_h = 3;
        let id = match s.upsert(&item, Some(vec![1, 2, 3]), 10).unwrap() {
            WriteOutcome::Inserted(id) => id,
            other => panic!("{other:?}"),
        };
        assert_eq!(s.pending_thumbs(10).unwrap().len(), 1);
        s.set_thumb(id, Some(vec![9, 9])).unwrap();
        assert!(s.pending_thumbs(10).unwrap().is_empty());
        assert_eq!(s.get_thumb(id).unwrap(), Some(vec![9, 9]));
        assert_eq!(s.get_blob(id).unwrap(), Some(vec![1, 2, 3]));
        drop(s);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn list_orders_pinned_first_then_recent() {
        let (mut s, p) = open("order");
        for (i, body) in ["a", "b", "c"].iter().enumerate() {
            s.upsert(&text_item(body, body), None, 100 + i as i64)
                .unwrap();
        }
        s.set_pinned(1, true).unwrap();
        let rows = s.list_page("", 10).unwrap();
        assert_eq!(rows[0].id, 1);
        assert!(rows[0].pinned);
        assert_eq!(rows[1].id, 3, "其余按最近使用倒序");
        drop(s);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn search_matches_preview_then_falls_back_to_text() {
        let (mut s, p) = open("search");
        s.upsert(&text_item("开头 HELLO 结尾", "开头 HELLO 结尾"), None, 1)
            .unwrap();
        // 预览会截断，正文里才有的关键字只能靠回退搜索找到。
        let long_body = format!("{}目标关键字{}", "前".repeat(400), "后".repeat(400));
        let preview = crate::core::filter::make_text_preview(&long_body, 200);
        s.upsert(&text_item(&preview, &long_body), None, 2).unwrap();

        let hits = s.list_page("hello", 10).unwrap();
        assert_eq!(hits.len(), 1, "预览命中（大小写不敏感）");
        let hits = s.list_page("目标关键字", 10).unwrap();
        assert_eq!(hits.len(), 1, "预览截断后走正文回退");
        // LIKE 通配符必须被转义，不能把 % 当通配符用。
        assert!(s.list_page("%", 10).unwrap().is_empty());
        drop(s);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn prune_by_count_keeps_newest_and_pinned() {
        let (mut s, p) = open("prune-count");
        for i in 0..10 {
            let body = format!("item-{i}");
            s.upsert(&text_item(&body, &body), None, 1000 + i).unwrap();
        }
        s.set_pinned(1, true).unwrap();
        let set = Settings {
            max_items: 3,
            keep_days: 0,
            max_image_bytes: 0,
            ..Settings::default()
        };
        let report = s.prune(&set, 10_000).unwrap();
        assert_eq!(report.by_count, 6); // 10 条里 1 条置顶 + 保留 3 条 → 删 6
        let rows = s.list_page("", 100).unwrap();
        assert_eq!(rows.len(), 4);
        assert!(rows.iter().any(|r| r.id == 1 && r.pinned));
        drop(s);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn prune_by_age_respects_zero_as_unlimited() {
        let (mut s, p) = open("prune-age");
        let day = 24 * 60 * 60 * 1000;
        for i in 0..5 {
            let body = format!("old-{i}");
            s.upsert(&text_item(&body, &body), None, i).unwrap();
        }
        let mut set = Settings {
            max_items: 0,
            keep_days: 0,
            max_image_bytes: 0,
            ..Settings::default()
        };
        assert_eq!(
            s.prune(&set, 100 * day).unwrap().by_age,
            0,
            "0 天 = 不按时间清理"
        );

        set.keep_days = 1;
        let report = s.prune(&set, 10 * day).unwrap();
        assert_eq!(report.by_age, 5);
        drop(s);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn prune_image_budget_deletes_oldest_images() {
        let (mut s, p) = open("prune-img");
        for i in 0..4 {
            let img = ImageData {
                width: 2,
                height: 2,
                rgba: vec![0; 16],
            };
            let mut item = CapturedItem::image(format!("图片 {i}"), img, i as u64 + 1, None);
            item.img_w = 2;
            item.img_h = 2;
            s.upsert(&item, Some(vec![0u8; 100]), 100 + i).unwrap();
        }
        let set = Settings {
            max_items: 0,
            keep_days: 0,
            // 只放得下 2 张（每张 100 字节）
            max_image_bytes: 250,
            ..Settings::default()
        };
        let report = s.prune(&set, 10_000).unwrap();
        assert_eq!(report.by_image_bytes, 2);
        assert_eq!(s.stats().unwrap().0, 2);
        drop(s);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn clear_can_keep_pinned() {
        let (mut s, p) = open("clear");
        for i in 0..3 {
            let body = format!("c{i}");
            s.upsert(&text_item(&body, &body), None, i).unwrap();
        }
        s.set_pinned(2, true).unwrap();
        assert_eq!(s.clear(true).unwrap(), 2);
        let rows = s.list_page("", 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, 2);
        assert_eq!(s.clear(false).unwrap(), 1);
        assert_eq!(s.stats().unwrap().0, 0);
        drop(s);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn settings_roundtrip_through_db() {
        let (s, p) = open("settings");
        let set = Settings {
            max_items: 77,
            pause_capture: true,
            ..Settings::default()
        };
        set.save(&s).unwrap();
        let back = Settings::load(&s).unwrap();
        assert_eq!(back.max_items, 77);
        assert!(back.pause_capture);
        assert_eq!(back.hotkey_text(), "Alt+V");
        drop(s);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn corrupted_db_is_quarantined_and_rebuilt() {
        let p = tmp_db("corrupt");
        std::fs::write(&p, b"this is definitely not a sqlite file").unwrap();
        let (s, recovered) = Store::open_or_recover(&p).unwrap();
        let quarantined = recovered.expect("损坏文件必须被隔离，而不是静默删除");
        assert!(
            quarantined
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("corrupt"),
            "隔离文件名要能一眼看出是损坏文件：{}",
            quarantined.display()
        );
        assert!(
            quarantined.exists(),
            "隔离文件必须留在磁盘上，用户还有机会自救"
        );
        assert_eq!(s.stats().unwrap().0, 0, "重建后应是空库");
        drop(s);
        let _ = std::fs::remove_file(&quarantined);
        let _ = std::fs::remove_file(&p);
    }

    /// 这是本模块最重要的一条回归测试。
    ///
    /// 曾经把"打开失败"一律当成损坏 → 只要库被另一个连接瞬时独占（另一个实例在写、
    /// 杀进程后句柄未释放、杀软正在扫描），就会把用户的历史改名成 corrupt 文件。
    /// 现在必须报 `Unavailable` 并且**原地保留文件**。
    #[test]
    fn busy_database_is_reported_unavailable_not_quarantined() {
        let p = tmp_db("busy");
        let (first, _) = Store::open_or_recover(&p).unwrap();
        assert_eq!(first.stats().unwrap().0, 0);

        // 另开一个连接并长期持有排它锁，模拟"另一个实例正在写"
        let blocker = Connection::open(&p).unwrap();
        blocker
            .execute_batch("PRAGMA busy_timeout=0; BEGIN EXCLUSIVE;")
            .unwrap();

        let result = Store::open_or_recover(&p);
        blocker.execute_batch("ROLLBACK;").unwrap();

        match result {
            Err(OpenFailure::Unavailable { .. }) => {}
            Err(OpenFailure::Corrupt { .. }) => panic!("占用被误判成损坏，用户历史会被隔离丢失"),
            Ok(_) => panic!("库被独占时不应该打开成功"),
        }
        assert!(p.exists(), "数据文件必须原地保留");
        // 目录里不能出现任何隔离文件
        let dir = p.parent().unwrap();
        let quarantined: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("busy") && n.contains("corrupt"))
            .collect();
        assert!(quarantined.is_empty(), "不该产生隔离文件：{quarantined:?}");

        drop(first);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn unavailable_error_message_mentions_no_modification() {
        // 面向用户的措辞必须让人放心：文件没被动过，以及该检查什么。
        let fail = OpenFailure::Unavailable {
            path: PathBuf::from(r"C:\x\data.db"),
            detail: "database is locked".into(),
        };
        let text = fail.to_string();
        assert!(text.contains("data.db"));
        assert!(text.contains("没有被改动"));
        assert!(text.contains("请先退出那个实例"));

        let corrupt = OpenFailure::Corrupt {
            path: PathBuf::from(r"C:\x\data.db"),
            detail: "file is not a database".into(),
        };
        assert!(corrupt.to_string().contains("已损坏"));
    }
}
