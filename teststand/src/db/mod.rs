// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// SQLite database access layer for experiments, runs, results, and logs.

use chrono::Utc;
use sqlx::{Pool, Sqlite};
use uuid::Uuid;

use crate::error::Result;

pub type Db = Pool<Sqlite>;

pub async fn open(db_path: &std::path::Path) -> Result<Db> {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;
    let url = format!("sqlite:{}", db_path.display());
    let opts = SqliteConnectOptions::from_str(&url)?.create_if_missing(true);
    let pool = SqlitePoolOptions::new().connect_with(opts).await?;
    sqlx::migrate!().run(&pool).await?;
    Ok(pool)
}

/// Open an in-memory SQLite database (useful in unit/integration tests).
#[allow(dead_code)]
pub async fn open_memory() -> Result<Db> {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;
    // max_connections(1) is required: every new connection gets a fresh
    // in-memory DB, so we must keep exactly one live connection.
    let opts = SqliteConnectOptions::from_str("sqlite::memory:")?;
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await?;
    sqlx::migrate!().run(&pool).await?;
    Ok(pool)
}

// ── Row types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct Experiment {
    pub id: String,
    pub name: String,
    pub family: String,
    pub kind: String,
    pub spec_json: String,
    pub status: String,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct Run {
    pub id: String,
    pub experiment_id: String,
    pub run_index: i64,
    pub workers: i64,
    pub phase: String,
    pub status: String,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct ExperimentResult {
    pub id: String,
    pub run_id: String,
    pub image_id: String,
    pub base_image_id: Option<String>,
    pub compress_stats_json: Option<String>,
    pub decompress_stats_json: Option<String>,
    pub timing_json: Option<String>,
    pub archive_bytes: Option<i64>,
    pub base_qcow2_bytes: Option<i64>,
    pub target_qcow2_bytes: Option<i64>,
    /// Total size of the experiment-local storage directory after compression.
    pub storage_bytes: Option<i64>,
    pub cstar: Option<f64>,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct LogLine {
    pub id: i64,
    pub run_id: String,
    pub level: String,
    pub ts: i64,
    pub message: String,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}
pub fn now_ts() -> i64 {
    Utc::now().timestamp()
}

pub async fn insert_experiment(
    db: &Db,
    id: &str,
    name: &str,
    family: &str,
    kind: &str,
    spec_json: &str,
) -> Result<()> {
    let ts = now_ts();
    sqlx::query(
        "INSERT INTO experiments (id, name, family, kind, spec_json, status, created_at) VALUES (?,?,?,?,?,'queued',?)"
    )
    .bind(id).bind(name).bind(family).bind(kind).bind(spec_json).bind(ts)
    .execute(db).await?;
    Ok(())
}

pub async fn list_experiments(db: &Db) -> Result<Vec<Experiment>> {
    Ok(
        sqlx::query_as::<_, Experiment>("SELECT * FROM experiments ORDER BY created_at DESC")
            .fetch_all(db)
            .await?,
    )
}

pub async fn get_experiment(db: &Db, id: &str) -> Result<Option<Experiment>> {
    Ok(
        sqlx::query_as::<_, Experiment>("SELECT * FROM experiments WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await?,
    )
}

pub async fn update_experiment_status(db: &Db, id: &str, status: &str) -> Result<()> {
    let finished: Option<i64> = if status == "done" || status == "error" {
        Some(now_ts())
    } else {
        None
    };
    sqlx::query("UPDATE experiments SET status = ?, finished_at = ? WHERE id = ?")
        .bind(status)
        .bind(finished)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn insert_run(db: &Db, r: &Run) -> Result<()> {
    sqlx::query(
        "INSERT INTO runs (id, experiment_id, run_index, workers, phase, status) VALUES (?,?,?,?,?,?)"
    )
    .bind(&r.id).bind(&r.experiment_id).bind(r.run_index).bind(r.workers)
    .bind(&r.phase).bind(&r.status)
    .execute(db).await?;
    Ok(())
}

pub async fn update_run_started(db: &Db, id: &str) -> Result<()> {
    let ts = now_ts();
    sqlx::query("UPDATE runs SET status = 'running', started_at = ? WHERE id = ?")
        .bind(ts)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn update_run_done(db: &Db, id: &str, error: Option<&str>) -> Result<()> {
    let ts = now_ts();
    let status = if error.is_some() { "error" } else { "done" };
    sqlx::query("UPDATE runs SET status = ?, finished_at = ?, error = ? WHERE id = ?")
        .bind(status)
        .bind(ts)
        .bind(error)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn insert_result(db: &Db, res: &ExperimentResult) -> Result<()> {
    sqlx::query(
        "INSERT INTO results (id, run_id, image_id, base_image_id, compress_stats_json, decompress_stats_json, timing_json, archive_bytes, base_qcow2_bytes, target_qcow2_bytes, storage_bytes, cstar) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)"
    )
    .bind(&res.id).bind(&res.run_id).bind(&res.image_id).bind(&res.base_image_id)
    .bind(&res.compress_stats_json).bind(&res.decompress_stats_json).bind(&res.timing_json)
    .bind(res.archive_bytes).bind(res.base_qcow2_bytes).bind(res.target_qcow2_bytes)
    .bind(res.storage_bytes).bind(res.cstar)
    .execute(db).await?;
    Ok(())
}

pub async fn get_results_for_experiment(
    db: &Db,
    experiment_id: &str,
) -> Result<Vec<ExperimentResult>> {
    Ok(sqlx::query_as::<_, ExperimentResult>(
        "SELECT r.* FROM results r JOIN runs ru ON r.run_id = ru.id WHERE ru.experiment_id = ?",
    )
    .bind(experiment_id)
    .fetch_all(db)
    .await?)
}

pub async fn append_log(db: &Db, run_id: &str, level: &str, message: &str) -> Result<()> {
    let ts = now_ts();
    sqlx::query("INSERT INTO log_lines (run_id, level, ts, message) VALUES (?,?,?,?)")
        .bind(run_id)
        .bind(level)
        .bind(ts)
        .bind(message)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn get_logs(db: &Db, run_id: &str) -> Result<Vec<LogLine>> {
    Ok(
        sqlx::query_as::<_, LogLine>("SELECT * FROM log_lines WHERE run_id = ? ORDER BY id")
            .bind(run_id)
            .fetch_all(db)
            .await?,
    )
}

/// Fetch all log lines for all runs belonging to an experiment, ordered by id.
pub async fn get_logs_for_experiment(db: &Db, experiment_id: &str) -> Result<Vec<LogLine>> {
    Ok(sqlx::query_as::<_, LogLine>(
        "SELECT l.* FROM log_lines l \
         JOIN runs r ON l.run_id = r.id \
         WHERE r.experiment_id = ? \
         ORDER BY l.id",
    )
    .bind(experiment_id)
    .fetch_all(db)
    .await?)
}

// ── Telegram subscribers ─────────────────────────────────────────────────

pub async fn add_telegram_subscriber(db: &Db, chat_id: i64) -> Result<()> {
    sqlx::query("INSERT OR IGNORE INTO telegram_subscribers (chat_id, added_at) VALUES (?, ?)")
        .bind(chat_id)
        .bind(now_ts())
        .execute(db)
        .await?;
    Ok(())
}

pub async fn list_telegram_subscribers(db: &Db) -> Result<Vec<i64>> {
    let rows: Vec<(i64,)> = sqlx::query_as("SELECT chat_id FROM telegram_subscribers")
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

pub async fn remove_telegram_subscriber(db: &Db, chat_id: i64) -> Result<()> {
    sqlx::query("DELETE FROM telegram_subscribers WHERE chat_id = ?")
        .bind(chat_id)
        .execute(db)
        .await?;
    Ok(())
}
