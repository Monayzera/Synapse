use crate::error::{AppError, AppResult};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct HistoryEntry {
    pub id: i64,
    pub created_at: i64,
    pub duration_ms: i64,
    pub word_count: i64,
    pub raw_text: String,
    pub final_text: String,
    pub language: String,
    pub on_gpu: bool,
    pub llm_used: bool,
    pub cloud: bool,
}

#[derive(Debug, Clone)]
pub struct NewEntry {
    pub created_at: i64,
    pub duration_ms: i64,
    pub word_count: i64,
    pub raw_text: String,
    pub final_text: String,
    pub language: String,
    pub on_gpu: bool,
    pub llm_used: bool,
    pub cloud: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub total_entries: i64,
    pub total_words: i64,
    pub total_speaking_ms: i64,
    pub avg_wpm: f64,
}

pub fn open(path: &Path) -> AppResult<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path).map_err(|e| AppError::Db(e.to_string()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| AppError::Db(e.to_string()))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
        .map_err(|e| AppError::Db(e.to_string()))?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS recordings (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at  INTEGER NOT NULL,
            duration_ms INTEGER NOT NULL,
            word_count  INTEGER NOT NULL,
            raw_text    TEXT NOT NULL,
            final_text  TEXT NOT NULL,
            language    TEXT NOT NULL DEFAULT '',
            on_gpu      INTEGER NOT NULL DEFAULT 1,
            llm_used    INTEGER NOT NULL DEFAULT 0,
            cloud       INTEGER NOT NULL DEFAULT 0
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS recordings_fts USING fts5(
            raw_text, final_text,
            content='recordings', content_rowid='id'
        );
        CREATE TRIGGER IF NOT EXISTS recordings_ai AFTER INSERT ON recordings BEGIN
            INSERT INTO recordings_fts(rowid, raw_text, final_text)
            VALUES (new.id, new.raw_text, new.final_text);
        END;
        CREATE TRIGGER IF NOT EXISTS recordings_ad AFTER DELETE ON recordings BEGIN
            INSERT INTO recordings_fts(recordings_fts, rowid, raw_text, final_text)
            VALUES ('delete', old.id, old.raw_text, old.final_text);
        END;
        CREATE TRIGGER IF NOT EXISTS recordings_au AFTER UPDATE ON recordings BEGIN
            INSERT INTO recordings_fts(recordings_fts, rowid, raw_text, final_text)
            VALUES ('delete', old.id, old.raw_text, old.final_text);
            INSERT INTO recordings_fts(rowid, raw_text, final_text)
            VALUES (new.id, new.raw_text, new.final_text);
        END;
        "#,
    )
    .map_err(|e| AppError::Db(e.to_string()))?;
    let _ = conn.execute(
        "ALTER TABLE recordings ADD COLUMN cloud INTEGER NOT NULL DEFAULT 0",
        [],
    );
    Ok(())
}

pub fn insert(conn: &Connection, entry: &NewEntry) -> AppResult<i64> {
    conn.execute(
        "INSERT INTO recordings (created_at, duration_ms, word_count, raw_text, final_text, language, on_gpu, llm_used, cloud)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            entry.created_at,
            entry.duration_ms,
            entry.word_count,
            entry.raw_text,
            entry.final_text,
            entry.language,
            entry.on_gpu as i64,
            entry.llm_used as i64,
            entry.cloud as i64,
        ],
    )
    .map_err(|e| AppError::Db(e.to_string()))?;
    Ok(conn.last_insert_rowid())
}

fn row_to_entry(row: &rusqlite::Row) -> rusqlite::Result<HistoryEntry> {
    Ok(HistoryEntry {
        id: row.get(0)?,
        created_at: row.get(1)?,
        duration_ms: row.get(2)?,
        word_count: row.get(3)?,
        raw_text: row.get(4)?,
        final_text: row.get(5)?,
        language: row.get(6)?,
        on_gpu: row.get::<_, i64>(7)? != 0,
        llm_used: row.get::<_, i64>(8)? != 0,
        cloud: row.get::<_, i64>(9)? != 0,
    })
}

pub fn list(conn: &Connection, limit: i64, offset: i64) -> AppResult<Vec<HistoryEntry>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, created_at, duration_ms, word_count, raw_text, final_text, language, on_gpu, llm_used, cloud
             FROM recordings ORDER BY created_at DESC LIMIT ?1 OFFSET ?2",
        )
        .map_err(|e| AppError::Db(e.to_string()))?;
    let rows = stmt
        .query_map(params![limit, offset], row_to_entry)
        .map_err(|e| AppError::Db(e.to_string()))?;
    collect(rows)
}

pub fn search(conn: &Connection, query: &str, limit: i64) -> AppResult<Vec<HistoryEntry>> {
    let match_query = build_match_query(query);
    if match_query.is_empty() {
        return list(conn, limit, 0);
    }
    let mut stmt = conn
        .prepare(
            "SELECT r.id, r.created_at, r.duration_ms, r.word_count, r.raw_text, r.final_text, r.language, r.on_gpu, r.llm_used, r.cloud
             FROM recordings r
             JOIN recordings_fts f ON f.rowid = r.id
             WHERE recordings_fts MATCH ?1
             ORDER BY rank LIMIT ?2",
        )
        .map_err(|e| AppError::Db(e.to_string()))?;
    let rows = stmt
        .query_map(params![match_query, limit], row_to_entry)
        .map_err(|e| AppError::Db(e.to_string()))?;
    collect(rows)
}

fn collect<I>(rows: I) -> AppResult<Vec<HistoryEntry>>
where
    I: Iterator<Item = rusqlite::Result<HistoryEntry>>,
{
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| AppError::Db(e.to_string()))?);
    }
    Ok(out)
}

fn build_match_query(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn get(conn: &Connection, id: i64) -> AppResult<Option<HistoryEntry>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, created_at, duration_ms, word_count, raw_text, final_text, language, on_gpu, llm_used, cloud
             FROM recordings WHERE id = ?1",
        )
        .map_err(|e| AppError::Db(e.to_string()))?;
    let mut rows = stmt
        .query_map(params![id], row_to_entry)
        .map_err(|e| AppError::Db(e.to_string()))?;
    match rows.next() {
        Some(row) => Ok(Some(row.map_err(|e| AppError::Db(e.to_string()))?)),
        None => Ok(None),
    }
}

pub fn delete(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM recordings WHERE id = ?1", params![id])
        .map_err(|e| AppError::Db(e.to_string()))?;
    Ok(())
}

pub fn clear(conn: &Connection) -> AppResult<()> {
    conn.execute_batch("DELETE FROM recordings;")
        .map_err(|e| AppError::Db(e.to_string()))?;
    Ok(())
}

pub fn stats(conn: &Connection) -> AppResult<Stats> {
    conn.query_row(
        "SELECT
            COUNT(*),
            COALESCE(SUM(word_count), 0),
            COALESCE(SUM(duration_ms), 0)
         FROM recordings",
        [],
        |row| {
            let total_entries: i64 = row.get(0)?;
            let total_words: i64 = row.get(1)?;
            let total_speaking_ms: i64 = row.get(2)?;
            let avg_wpm = if total_speaking_ms > 0 {
                (total_words as f64) * 60000.0 / (total_speaking_ms as f64)
            } else {
                0.0
            };
            Ok(Stats {
                total_entries,
                total_words,
                total_speaking_ms,
                avg_wpm: (avg_wpm * 10.0).round() / 10.0,
            })
        },
    )
    .map_err(|e| AppError::Db(e.to_string()))
}
