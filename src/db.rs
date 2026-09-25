//! mdrv-db engine handle: schema, per-file batch writes, queries.
//!
//! The daemon is the single writer (fjall file lock); all access happens on
//! the index worker thread. The DB is derived data — `ua reindex` rebuilds
//! everything from the notes dir. Never hand-edit `/x/db/upperadd/live/`.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use mdrv_db::engine::{Engine, EngineConfig, MutateRequest};
use mdrv_db::{Op, PortValue, SqlKind};

/// Fleet data dir for this app (registered in ~/.config/mdrv-db/config.toml,
/// which upperadd itself never reads).
pub const DATA_DIR: &str = "/x/db/upperadd";
pub const DB_NAME: &str = "upperadd";
const SECTIONS: &str = "sections";
const PINS: &str = "pins";
const ACTOR: &str = "upperadd-index";
/// v0.1 ships a single workspace; rows carry the name so more can exist.
pub const DEFAULT_WORKSPACE: &str = "default";

const COLUMNS: [&str; 7] = [
    "id",
    "path",
    "line",
    "title",
    "section_mtime",
    "hash",
    "content",
];

/// One indexed section: a row of the `sections` table.
#[derive(Clone, Debug)]
pub struct SectionRow {
    /// Notes-dir-relative, `/`-separated.
    pub path: String,
    /// 1-based line of the heading (1 for the pre-heading region).
    pub line: u32,
    pub title: String,
    /// Derived edit time (unix millis) — carried across re-indexes by hash.
    pub section_mtime: i64,
    /// blake3 hex of `content`; the change-detection key.
    pub hash: String,
    pub content: String,
}

/// Title/path metadata for in-memory fuzzy search.
#[derive(Clone, Debug)]
pub struct SectionMeta {
    pub path: String,
    pub line: u32,
    pub title: String,
    pub section_mtime: i64,
}

fn create_sections() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {SECTIONS} (
            id TEXT PRIMARY KEY,
            path TEXT NOT NULL,
            line INTEGER NOT NULL,
            title TEXT NOT NULL,
            section_mtime INTEGER NOT NULL,
            hash TEXT NOT NULL,
            content TEXT NOT NULL
        )"
    )
}

fn create_indexes() -> Vec<String> {
    vec![
        format!("CREATE INDEX IF NOT EXISTS idx_{SECTIONS}_path ON {SECTIONS}(path)"),
        format!("CREATE INDEX IF NOT EXISTS idx_{SECTIONS}_mtime ON {SECTIONS}(section_mtime)"),
    ]
}

fn create_pins() -> String {
    // Pin state is WORKSPACE state, not note state: it must survive
    // `ua reindex` (which resets only `sections`), so it gets its own table.
    // One row per pinned section per workspace; `rank` = user order.
    format!(
        "CREATE TABLE IF NOT EXISTS {PINS} (\
             id TEXT PRIMARY KEY, \
             workspace TEXT NOT NULL, \
             path TEXT NOT NULL, \
             line INTEGER NOT NULL, \
             rank INTEGER NOT NULL)"
    )
}

fn pin_id(workspace: &str, path: &str, line: u32) -> String {
    format!("{workspace}:{path}:{line}")
}

fn row_id(path: &str, line: u32) -> String {
    format!("{path}:{line}")
}

fn delete_path_op(path: &str) -> Op {
    Op::Sql {
        kind: SqlKind::Delete,
        table: SECTIONS.into(),
        pk_col: "path".into(),
        columns: vec!["path".into()],
        values: vec![PortValue::Text(path.into())],
        pk: PortValue::Text(path.into()),
    }
}

/// Escape a LIKE needle: `%`, `_` and the escape char itself, with `ESCAPE '\'`.
fn escape_like(needle: &str) -> String {
    needle
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub struct Db {
    engine: Engine,
}

impl Db {
    pub fn open() -> Result<Self> {
        let root = PathBuf::from(DATA_DIR);
        // Fleet layout is <root>/live; ensure it exists BEFORE live_dir()
        // resolves, else it falls back to <root> itself.
        std::fs::create_dir_all(root.join("live"))
            .with_context(|| format!("creating {}", root.join("live").display()))?;
        let live = mdrv_db::live_dir(&root);
        // Turso needs the parent dir to exist; Engine::open would create it,
        // but only after we've already handed it the port.
        std::fs::create_dir_all(&live).with_context(|| format!("creating {}", live.display()))?;
        let port = mdrv_db::TursoPort::open(live.join("app.db"))
            .map_err(|e| anyhow!("opening turso port: {e}"))?;
        // The index is derived data — group-commit durability is plenty.
        let engine = Engine::open(
            &root,
            DB_NAME,
            Box::new(port),
            EngineConfig {
                fsync_each_write: false,
            },
        )
        .context("opening mdrv-db engine")?;
        let db = Self { engine };
        let mut ddl = vec![create_sections(), create_pins()];
        ddl.extend(create_indexes());
        db.engine.bootstrap(&ddl).context("bootstrapping schema")?;
        Ok(db)
    }

    fn select(&self, sql: String, params: Vec<PortValue>) -> Result<Vec<serde_json::Value>> {
        let out = self
            .engine
            .query(&sql, params)
            .map_err(|e| anyhow!("{e}"))?;
        Ok(out.as_array().cloned().unwrap_or_default())
    }

    pub fn section_count(&self) -> Result<u64> {
        let rows = self.select(format!("SELECT COUNT(*) AS n FROM {SECTIONS}"), vec![])?;
        Ok(rows.first().and_then(|r| r["n"].as_u64()).unwrap_or(0))
    }

    /// One section's content, for the preview pane.
    pub fn section_content(&self, path: &str, line: u32) -> Result<Option<String>> {
        let rows = self.select(
            format!("SELECT content FROM {SECTIONS} WHERE path = ? AND line = ?"),
            vec![
                PortValue::Text(path.into()),
                PortValue::Int(i64::from(line)),
            ],
        )?;
        Ok(rows
            .first()
            .and_then(|r| r["content"].as_str())
            .map(String::from))
    }

    /// Existing rows for one path as a hash → section_mtime carry map.
    pub fn mtime_carry(&self, path: &str) -> Result<std::collections::HashMap<String, i64>> {
        let rows = self.select(
            format!("SELECT hash, section_mtime FROM {SECTIONS} WHERE path = ?"),
            vec![PortValue::Text(path.into())],
        )?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                Some((
                    r["hash"].as_str()?.to_string(),
                    r["section_mtime"].as_i64()?,
                ))
            })
            .collect())
    }

    /// Replace all rows of one file in a single transaction: one delete by
    /// path plus one insert per section (the file is the logical entity).
    pub fn replace_file(&self, sections: &[SectionRow]) -> Result<()> {
        anyhow::ensure!(!sections.is_empty(), "replace_file: empty batch");
        let path = &sections[0].path;
        debug_assert!(sections.iter().all(|s| &s.path == path));
        let mut ops = vec![delete_path_op(path)];
        for s in sections {
            let id = row_id(&s.path, s.line);
            ops.push(Op::Sql {
                kind: SqlKind::Insert,
                table: SECTIONS.into(),
                pk_col: "id".into(),
                columns: COLUMNS.iter().map(|c| c.to_string()).collect(),
                values: vec![
                    PortValue::Text(id.clone()),
                    PortValue::Text(s.path.clone()),
                    PortValue::Int(i64::from(s.line)),
                    PortValue::Text(s.title.clone()),
                    PortValue::Int(s.section_mtime),
                    PortValue::Text(s.hash.clone()),
                    PortValue::Text(s.content.clone()),
                ],
                pk: PortValue::Text(id),
            });
        }
        self.engine
            .execute(MutateRequest {
                actor: ACTOR.into(),
                ops,
                idem_key: None,
                response: None,
            })
            .context("replacing file rows")?;
        Ok(())
    }

    /// Pinned section keys for `workspace`, in user-defined order.
    pub fn pins(&self, workspace: &str) -> Result<Vec<(String, u32)>> {
        let rows = self.select(
            format!("SELECT path, line FROM {PINS} WHERE workspace = ? ORDER BY rank"),
            vec![PortValue::Text(workspace.into())],
        )?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let path = row.get("path")?.as_str()?.to_string();
                let line = row.get("line")?.as_i64()? as u32;
                Some((path, line))
            })
            .collect())
    }

    /// Rewrite the whole pin list for `workspace` in one transaction
    /// (`rank` = list index). Full rewrite keeps reorder/toggle/prune on
    /// one code path — pin lists are tiny.
    pub fn write_pins(&self, workspace: &str, pins: &[(String, u32)]) -> Result<()> {
        let mut ops = vec![Op::Sql {
            kind: SqlKind::Delete,
            table: PINS.into(),
            pk_col: "workspace".into(),
            columns: vec![],
            values: vec![],
            pk: PortValue::Text(workspace.into()),
        }];
        for (rank, (path, line)) in pins.iter().enumerate() {
            ops.push(Op::Sql {
                kind: SqlKind::Insert,
                table: PINS.into(),
                pk_col: "id".into(),
                columns: vec![
                    "id".into(),
                    "workspace".into(),
                    "path".into(),
                    "line".into(),
                    "rank".into(),
                ],
                values: vec![
                    PortValue::Text(pin_id(workspace, path, *line)),
                    PortValue::Text(workspace.into()),
                    PortValue::Text(path.clone()),
                    PortValue::Int(*line as i64),
                    PortValue::Int(rank as i64),
                ],
                pk: PortValue::Text(pin_id(workspace, path, *line)),
            });
        }
        self.engine
            .execute(MutateRequest {
                actor: ACTOR.into(),
                ops,
                idem_key: None,
                response: None,
            })
            .map(|_| ())
            .map_err(|e| anyhow!("writing pins: {e}"))
    }

    pub fn delete_path(&self, path: &str) -> Result<()> {
        self.engine
            .execute(MutateRequest {
                actor: ACTOR.into(),
                ops: vec![delete_path_op(path)],
                idem_key: None,
                response: None,
            })
            .context("deleting file rows")?;
        Ok(())
    }

    /// Wipe + recreate the table; the caller re-walks the notes dir.
    pub fn reset(&self) -> Result<()> {
        let mut ddl = vec![
            format!("DROP TABLE IF EXISTS {SECTIONS}"),
            create_sections(),
        ];
        ddl.extend(create_indexes());
        self.engine.bootstrap(&ddl).context("resetting schema")?;
        Ok(())
    }

    pub fn all_meta(&self) -> Result<Vec<SectionMeta>> {
        let rows = self.select(
            format!("SELECT path, line, title, section_mtime FROM {SECTIONS}"),
            vec![],
        )?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                Some(SectionMeta {
                    path: r["path"].as_str()?.to_string(),
                    line: u32::try_from(r["line"].as_i64()?).ok()?,
                    title: r["title"].as_str()?.to_string(),
                    section_mtime: r["section_mtime"].as_i64()?,
                })
            })
            .collect())
    }

    /// Sections whose body contains `needle` (LIKE; ASCII case-insensitive).
    pub fn content_search(&self, needle: &str, limit: u32) -> Result<Vec<SectionMeta>> {
        let pattern = format!("%{}%", escape_like(needle));
        let rows = self.select(
            format!(
                "SELECT DISTINCT path, line, title, section_mtime FROM {SECTIONS} \
                 WHERE content LIKE ? ESCAPE '\\' \
                 ORDER BY section_mtime DESC LIMIT ?"
            ),
            vec![PortValue::Text(pattern), PortValue::Int(i64::from(limit))],
        )?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                Some(SectionMeta {
                    path: r["path"].as_str()?.to_string(),
                    line: u32::try_from(r["line"].as_i64()?).ok()?,
                    title: r["title"].as_str()?.to_string(),
                    section_mtime: r["section_mtime"].as_i64()?,
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_metacharacters_are_escaped() {
        assert_eq!(escape_like("50%_off\\x"), "50\\%\\_off\\\\x");
    }
}
