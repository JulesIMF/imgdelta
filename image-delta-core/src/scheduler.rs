// Scheduler is fully implemented in Phase 4. All items in this module
// are intentionally unused in the current phase.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// A work item in the processing queue.
pub enum WorkItem<T> {
    Task(T),
    /// Sentinel: signals a worker thread to shut down.
    Done,
}

/// Thread-safe FIFO queue shared between the dispatcher and worker threads.
///
/// Work items are added by the dispatcher and consumed by N worker threads.
/// Shutdown is signalled by pushing one [`WorkItem::Done`] sentinel per worker.
pub struct WorkQueue<T> {
    inner: Arc<Mutex<VecDeque<WorkItem<T>>>>,
}

impl<T: Send + 'static> WorkQueue<T> {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Enqueue a task.
    pub fn push(&self, item: T) {
        self.inner.lock().unwrap().push_back(WorkItem::Task(item));
    }

    /// Enqueue a `Done` sentinel (call once per worker thread to signal shutdown).
    pub fn push_done(&self) {
        self.inner.lock().unwrap().push_back(WorkItem::Done);
    }

    /// Dequeue the next item, or return `None` if the queue is empty.
    pub fn pop(&self) -> Option<WorkItem<T>> {
        self.inner.lock().unwrap().pop_front()
    }

    /// Clone the inner Arc so worker threads can share ownership.
    pub fn clone_arc(&self) -> Arc<Mutex<VecDeque<WorkItem<T>>>> {
        Arc::clone(&self.inner)
    }
}

impl<T: Send + 'static> Default for WorkQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}
