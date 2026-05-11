// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// ProgressEvent broadcast channel for SSE streaming.

use serde::{Deserialize, Serialize};

/// Events broadcast over the SSE endpoint `/api/events`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProgressEvent {
    /// An experiment was enqueued.
    Enqueued { experiment_id: String, name: String },
    /// A specific run phase started.
    RunStarted {
        experiment_id: String,
        run_id: String,
        workers: usize,
        phase: String,
    },
    /// Per-image progress within a run.
    ImageDone {
        run_id: String,
        image_id: String,
        base_image_id: Option<String>,
        compress_ms: u64,
        archive_bytes: u64,
        cstar: f64,
    },
    /// A run finished (done or error).
    RunFinished {
        run_id: String,
        status: String,
        error: Option<String>,
    },
    /// An entire experiment finished.
    ExperimentFinished {
        experiment_id: String,
        status: String,
    },
    /// Download progress.
    DownloadProgress {
        image_id: String,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
    },
    /// Generic log message.
    Log {
        experiment_id: Option<String>,
        run_id: Option<String>,
        level: String,
        message: String,
    },
}

pub type ProgressTx = tokio::sync::broadcast::Sender<ProgressEvent>;
pub type ProgressRx = tokio::sync::broadcast::Receiver<ProgressEvent>;

pub fn channel() -> (ProgressTx, ProgressRx) {
    tokio::sync::broadcast::channel(256)
}
