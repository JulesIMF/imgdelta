// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// In-memory log ring-buffer + tracing subscriber layers.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use chrono::Utc;
use serde::Serialize;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::{layer::Context, Layer};

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub ts_ms: i64,
    pub level: String,
    pub target: String,
    pub message: String,
}

// ── Ring-buffer ───────────────────────────────────────────────────────────────

pub struct LogBuffer {
    entries: Mutex<VecDeque<LogEntry>>,
    capacity: usize,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        })
    }

    pub fn push(&self, entry: LogEntry) {
        let mut q = self.entries.lock().unwrap();
        if q.len() >= self.capacity {
            q.pop_front();
        }
        q.push_back(entry);
    }

    /// Return the last `n` entries in chronological order.
    pub fn tail(&self, n: usize) -> Vec<LogEntry> {
        let q = self.entries.lock().unwrap();
        let skip = q.len().saturating_sub(n);
        q.iter().skip(skip).cloned().collect()
    }
}

// ── BufferLayer — writes to ring-buffer ──────────────────────────────────────

pub struct BufferLayer {
    pub buffer: Arc<LogBuffer>,
}

impl<S: Subscriber> Layer<S> for BufferLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        self.buffer.push(LogEntry {
            ts_ms: Utc::now().timestamp_millis(),
            level: level_str(event).to_owned(),
            target: event.metadata().target().to_owned(),
            message: extract_message(event),
        });
    }
}

// ── SseBroadcastLayer — forwards tracing events to SSE channel ───────────────

/// Sends every tracing event as a `ProgressEvent::Log` on the SSE broadcast
/// channel so the web UI live-log tab receives server logs in real time.
pub struct SseBroadcastLayer {
    pub tx: crate::runner::progress::ProgressTx,
}

impl<S: Subscriber> Layer<S> for SseBroadcastLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let message = extract_message(event);
        // Ignore empty messages (no-op events)
        if message.is_empty() {
            return;
        }
        let _ = self.tx.send(crate::runner::progress::ProgressEvent::Log {
            experiment_id: None,
            run_id: None,
            level: level_str(event).to_owned(),
            message,
        });
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn level_str(event: &Event<'_>) -> &'static str {
    match *event.metadata().level() {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

fn extract_message(event: &Event<'_>) -> String {
    use tracing_subscriber::field::Visit;

    struct V(String);
    impl Visit for V {
        fn record_debug(&mut self, field: &tracing::field::Field, val: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0 = format!("{val:?}").trim_matches('"').to_owned();
            }
        }
        fn record_str(&mut self, field: &tracing::field::Field, val: &str) {
            if field.name() == "message" {
                self.0 = val.to_owned();
            }
        }
    }

    let mut v = V(String::new());
    event.record(&mut v);
    v.0
}
