// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// Axum REST API handlers for experiments, runs, logs, and SSE.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Sse},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

use crate::{
    config::{experiment::ExperimentSpec, families::FamiliesConfig},
    db::{self, Db},
    error::{Error, Result},
    image_manager::ImageManager,
    logging::LogBuffer,
    runner::{
        progress::{ProgressEvent, ProgressTx},
        queue::QueueItem,
        Runner,
    },
};

#[derive(Clone)]
pub struct ApiState {
    pub db: Db,
    pub runner: Arc<Runner>,
    pub progress_tx: ProgressTx,
    pub log_buffer: Arc<LogBuffer>,
    pub families: Arc<FamiliesConfig>,
    pub image_manager: Arc<ImageManager>,
}

// GET /api/families — list families + image IDs for the visual UI form
pub async fn list_families(State(s): State<ApiState>) -> impl IntoResponse {
    let data: Vec<_> = s
        .families
        .families
        .iter()
        .map(|f| {
            let format = f
                .images
                .first()
                .map(|i| i.format.as_str())
                .unwrap_or("qcow2");
            let images: Vec<_> = f
                .images
                .iter()
                .map(|i| {
                    json!({
                        "id":         i.id,
                        "size_bytes": i.size_bytes,
                    })
                })
                .collect();
            json!({
                "name":   f.name,
                "label":  f.label,
                "format": format,
                "images": images,
            })
        })
        .collect();
    Json(data)
}

// GET /api/status
pub async fn get_status(State(s): State<ApiState>) -> impl IntoResponse {
    let queue_len = s.runner.queue.len().await;
    Json(json!({ "queue_length": queue_len, "ok": true }))
}

// GET /api/experiments
pub async fn list_experiments(State(s): State<ApiState>) -> Result<impl IntoResponse> {
    let experiments = db::list_experiments(&s.db).await?;
    Ok(Json(experiments))
}

// GET /api/experiments/:id
pub async fn get_experiment(
    State(s): State<ApiState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    let exp = db::get_experiment(&s.db, &id)
        .await?
        .ok_or_else(|| Error::NotFound(id.clone()))?;
    let results = db::get_results_for_experiment(&s.db, &id).await?;
    Ok(Json(json!({ "experiment": exp, "results": results })))
}

// POST /api/experiments  (body: TOML text)
pub async fn create_experiment(
    State(s): State<ApiState>,
    body: String,
) -> Result<impl IntoResponse> {
    let spec: ExperimentSpec = crate::config::load_experiment(&body)?;
    let id = db::new_id();
    let spec_json = serde_json::to_string(&spec)?;
    db::insert_experiment(
        &s.db,
        &id,
        &spec.name,
        &spec.family,
        "BaseRelative",
        &spec_json,
    )
    .await?;
    s.runner
        .queue
        .push(QueueItem {
            experiment_id: id.clone(),
            spec,
        })
        .await;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

// POST /api/experiments/:id/abort — abort a running experiment
pub async fn abort_experiment(
    State(s): State<ApiState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    let exp = db::get_experiment(&s.db, &id)
        .await?
        .ok_or_else(|| Error::NotFound(id.clone()))?;
    if exp.status != "running" {
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({ "error": "experiment is not running" })),
        ));
    }
    db::update_experiment_status(&s.db, &id, "aborted").await?;
    s.runner.abort_running(&id).await;
    let _ = s.progress_tx.send(ProgressEvent::ExperimentFinished {
        experiment_id: id.clone(),
        status: "aborted".into(),
    });
    Ok((StatusCode::OK, Json(json!({ "ok": true }))))
}

// POST /api/experiments/:id/cancel — cancel a queued experiment
pub async fn cancel_experiment(
    State(s): State<ApiState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    let exp = db::get_experiment(&s.db, &id)
        .await?
        .ok_or_else(|| Error::NotFound(id.clone()))?;
    if exp.status != "queued" {
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({ "error": "experiment is not queued" })),
        ));
    }
    s.runner.queue.remove_by_id(&id).await;
    db::update_experiment_status(&s.db, &id, "cancelled").await?;
    let _ = s.progress_tx.send(ProgressEvent::ExperimentFinished {
        experiment_id: id.clone(),
        status: "cancelled".into(),
    });
    Ok((StatusCode::OK, Json(json!({ "ok": true }))))
}

// GET /api/results/:id  — download JSONL for an experiment
pub async fn download_results(
    State(s): State<ApiState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    let results = db::get_results_for_experiment(&s.db, &id).await?;
    let jsonl: String = results
        .iter()
        .filter_map(|r| serde_json::to_string(r).ok())
        .collect::<Vec<_>>()
        .join("\n");
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "application/x-ndjson"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"results.jsonl\"",
            ),
        ],
        jsonl,
    ))
}

// GET /api/results/:id/csv  — download CSV for an experiment
pub async fn download_results_csv(
    State(s): State<ApiState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    let results = db::get_results_for_experiment(&s.db, &id).await?;
    let mut lines = vec![
        "image_id,base_image_id,workers,run_repetition,runs_total,archive_bytes,base_qcow2_bytes,target_qcow2_bytes,storage_bytes,c,cstar,compress_ms,decompress_ms".to_owned(),
    ];
    for r in &results {
        let compress_ms: String = r
            .compress_stats_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| v["elapsed_secs"].as_f64())
            .map(|s| format!("{}", (s * 1000.0) as u64))
            .unwrap_or_default();
        let decompress_ms: String = r
            .decompress_stats_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| v["elapsed_secs"].as_f64())
            .map(|s| format!("{}", (s * 1000.0) as u64))
            .unwrap_or_default();
        lines.push(format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{}",
            r.image_id,
            r.base_image_id.as_deref().unwrap_or(""),
            r.workers,
            r.run_repetition + 1,
            r.runs_total,
            r.archive_bytes.map(|v| v.to_string()).unwrap_or_default(),
            r.base_qcow2_bytes
                .map(|v| v.to_string())
                .unwrap_or_default(),
            r.target_qcow2_bytes
                .map(|v| v.to_string())
                .unwrap_or_default(),
            r.storage_bytes.map(|v| v.to_string()).unwrap_or_default(),
            r.c.map(|v| format!("{:.4}", v)).unwrap_or_default(),
            r.cstar.map(|v| format!("{:.4}", v)).unwrap_or_default(),
            compress_ms,
            decompress_ms,
        ));
    }
    let csv = lines.join("\n");
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "text/csv"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"results.csv\"",
            ),
        ],
        csv,
    ))
}

// GET /api/logs/:run_id
pub async fn get_run_logs(
    State(s): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<impl IntoResponse> {
    let logs = db::get_logs(&s.db, &run_id).await?;
    Ok(Json(logs))
}

// GET /api/experiments/:id/logs  — all log lines for an experiment (across all runs)
pub async fn get_experiment_logs(
    State(s): State<ApiState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    let logs = db::get_logs_for_experiment(&s.db, &id).await?;
    Ok(Json(logs))
}

#[derive(Deserialize)]
pub struct ServerLogsQuery {
    #[serde(default = "default_tail")]
    n: usize,
}
fn default_tail() -> usize {
    200
}

// GET /api/logs/server?n=200
pub async fn get_server_logs(
    State(s): State<ApiState>,
    Query(q): Query<ServerLogsQuery>,
) -> impl IntoResponse {
    let entries = s.log_buffer.tail(q.n);
    Json(entries)
}

// GET /api/events  (SSE)
pub async fn sse_events(
    State(s): State<ApiState>,
) -> Sse<
    impl futures_util::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
> {
    let rx = s.progress_tx.subscribe();
    let stream = async_stream::stream! {
        let mut rx = rx;
        loop {
            match rx.recv().await {
                Ok(event) => {
                    // Drop debug/trace log events — they flood the SSE buffer.
                    if let ProgressEvent::Log { ref level, .. } = event {
                        if level == "debug" || level == "trace" {
                            continue;
                        }
                    }
                    if let Ok(data) = serde_json::to_string(&event) {
                        yield Ok::<_, std::convert::Infallible>(
                            axum::response::sse::Event::default().data(data)
                        );
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

// GET /api/images — list all families + images with download state
pub async fn list_images(State(s): State<ApiState>) -> impl IntoResponse {
    let all = s.image_manager.list_all().await;
    let data: Vec<_> = s
        .families
        .families
        .iter()
        .map(|f| {
            let images: Vec<_> = f
                .images
                .iter()
                .map(|spec| {
                    let info = all.iter().find(|i| i.id == spec.id);
                    let state = info.map(|i| i.state.as_str()).unwrap_or("missing");
                    let progress = info.and_then(|i| i.progress_bytes).unwrap_or(0);
                    let total = info
                        .and_then(|i| i.total_bytes)
                        .or(spec.size_bytes)
                        .unwrap_or(0);
                    json!({
                        "id":             spec.id,
                        "size_bytes":     spec.size_bytes,
                        "state":          state,
                        "progress_bytes": progress,
                        "total_bytes":    total,
                    })
                })
                .collect();
            json!({
                "name":   f.name,
                "label":  f.label,
                "images": images,
            })
        })
        .collect();
    Json(data)
}

// POST /api/images/:id/download — start background download
pub async fn start_image_download(
    State(s): State<ApiState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    if s.image_manager.image_state(&id).await.is_none() {
        return Err(Error::NotFound(id));
    }
    let im = Arc::clone(&s.image_manager);
    let id2 = id.clone();
    tokio::spawn(async move {
        if let Err(e) = im.ensure(&id2).await {
            tracing::error!(id = %id2, err = %e, "background image download failed");
        }
    });
    Ok(Json(json!({ "ok": true })))
}

// DELETE /api/images/:id — evict (delete) image from disk
pub async fn evict_image(
    State(s): State<ApiState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    s.image_manager.evict(&id).await?;
    Ok(Json(json!({ "ok": true })))
}
