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
    logging::LogBuffer,
    runner::{progress::ProgressTx, queue::QueueItem, Runner},
};

#[derive(Clone)]
pub struct ApiState {
    pub db: Db,
    pub runner: Arc<Runner>,
    pub progress_tx: ProgressTx,
    pub log_buffer: Arc<LogBuffer>,
    pub families: Arc<FamiliesConfig>,
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
        spec.kind.as_str(),
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
        [(axum::http::header::CONTENT_TYPE, "application/jsonl")],
        jsonl,
    ))
}

// GET /api/results/:id/csv  — download CSV for an experiment
pub async fn download_results_csv(
    State(s): State<ApiState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    let results = db::get_results_for_experiment(&s.db, &id).await?;
    let mut lines =
        vec!["image_id,base_image_id,archive_bytes,base_qcow2_bytes,cstar,compress_ms".to_owned()];
    for r in &results {
        let compress_ms: String = r
            .compress_stats_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| v["elapsed_secs"].as_f64())
            .map(|s| format!("{}", (s * 1000.0) as u64))
            .unwrap_or_default();
        lines.push(format!(
            "{},{},{},{},{},{}",
            r.image_id,
            r.base_image_id.as_deref().unwrap_or(""),
            r.archive_bytes.map(|v| v.to_string()).unwrap_or_default(),
            r.base_qcow2_bytes
                .map(|v| v.to_string())
                .unwrap_or_default(),
            r.cstar.map(|v| format!("{:.6}", v)).unwrap_or_default(),
            compress_ms,
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
