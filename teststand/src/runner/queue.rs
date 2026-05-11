// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// Experiment run queue backed by a tokio broadcast channel.

use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::config::experiment::ExperimentSpec;

#[derive(Debug, Clone)]
pub struct QueueItem {
    pub experiment_id: String,
    pub spec: ExperimentSpec,
}

#[derive(Debug, Clone, Default)]
pub struct Queue {
    inner: Arc<Mutex<VecDeque<QueueItem>>>,
    notify: Arc<tokio::sync::Notify>,
}

impl Queue {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub async fn push(&self, item: QueueItem) {
        self.inner.lock().await.push_back(item);
        self.notify.notify_one();
    }

    pub async fn pop_wait(&self) -> QueueItem {
        loop {
            {
                let mut q = self.inner.lock().await;
                if let Some(item) = q.pop_front() {
                    return item;
                }
            }
            self.notify.notified().await;
        }
    }

    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    #[allow(dead_code)]
    pub async fn is_empty(&self) -> bool {
        self.inner.lock().await.is_empty()
    }
}
