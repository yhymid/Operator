//! Port of `lib/telegram-rate-limiter.js`: per-chat paced queue with priorities,
//! `retry_after` handling and coalescing of replaceable status edits.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::time::Instant;

use crate::api::ApiError;
use crate::log;

pub type TaskFuture = Pin<Box<dyn Future<Output = Result<Value, ApiError>> + Send>>;
pub type Task = Box<dyn FnMut() -> TaskFuture + Send>;

struct Item {
    task: Task,
    respond: oneshot::Sender<Result<Value, ApiError>>,
    priority: i64,
    dedupe_key: Option<String>,
    label: String,
    sequence: u64,
}

#[derive(Default)]
struct ChatQueue {
    pending: Vec<Item>,
    running: bool,
    next_allowed_at: Option<Instant>,
}

pub struct Limiter {
    min_interval: Duration,
    max_retries: u64,
    retry_jitter_ms: u64,
    queues: Mutex<HashMap<String, ChatQueue>>,
    sequence: AtomicU64,
}

impl Limiter {
    pub fn new(min_interval_ms: u64, max_retries: u64, retry_jitter_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            min_interval: Duration::from_millis(min_interval_ms),
            max_retries,
            retry_jitter_ms,
            queues: Mutex::new(HashMap::new()),
            sequence: AtomicU64::new(1),
        })
    }

    /// `schedule(chatId, task, { priority, dedupeKey, label })`. Lower priorities run first,
    /// equal priorities keep FIFO order. A waiting item with the same `dedupe_key` is
    /// replaced and its caller resolves with `null`.
    pub async fn schedule(
        self: &Arc<Self>,
        key: String,
        task: Task,
        priority: i64,
        dedupe_key: Option<String>,
        label: &str,
    ) -> Result<Value, ApiError> {
        let (respond, receive) = oneshot::channel();
        let start_drain = {
            let mut queues = self.queues.lock().unwrap_or_else(|p| p.into_inner());
            let queue = queues.entry(key.clone()).or_default();
            if let Some(dedupe) = &dedupe_key
                && let Some(index) = queue
                    .pending
                    .iter()
                    .position(|i| i.dedupe_key.as_deref() == Some(dedupe))
            {
                let old = queue.pending.remove(index);
                let _ = old.respond.send(Ok(Value::Null));
            }
            queue.pending.push(Item {
                task,
                respond,
                priority,
                dedupe_key,
                label: label.to_owned(),
                sequence: self.sequence.fetch_add(1, Ordering::SeqCst),
            });
            queue.pending.sort_by(|a, b| {
                a.priority
                    .cmp(&b.priority)
                    .then(a.sequence.cmp(&b.sequence))
            });
            !std::mem::replace(&mut queue.running, true)
        };
        if start_drain {
            tokio::spawn(self.clone().drain(key));
        }
        receive.await.unwrap_or(Ok(Value::Null))
    }

    async fn drain(self: Arc<Self>, key: String) {
        loop {
            let (item, next_allowed_at) = {
                let mut queues = self.queues.lock().unwrap_or_else(|p| p.into_inner());
                let Some(queue) = queues.get_mut(&key) else {
                    return;
                };
                if queue.pending.is_empty() {
                    // The JS limiter forgets an idle chat (including its pacing timestamp).
                    queues.remove(&key);
                    return;
                }
                (queue.pending.remove(0), queue.next_allowed_at)
            };
            if let Some(at) = next_allowed_at {
                tokio::time::sleep_until(at).await;
            }
            let Item {
                mut task,
                respond,
                label,
                ..
            } = item;
            let result = self.execute_with_retry(&key, &label, &mut task).await;
            let _ = respond.send(result);
            let mut queues = self.queues.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(queue) = queues.get_mut(&key) {
                // Measured from the end of a request, as in JS.
                queue.next_allowed_at = Some(Instant::now() + self.min_interval);
            }
        }
    }

    async fn execute_with_retry(
        &self,
        key: &str,
        label: &str,
        task: &mut Task,
    ) -> Result<Value, ApiError> {
        let mut attempt = 0;
        loop {
            match task().await {
                Ok(value) => return Ok(value),
                Err(error) => {
                    let Some(retry_after) = error.retry_after_seconds() else {
                        return Err(error);
                    };
                    if attempt >= self.max_retries {
                        return Err(error);
                    }
                    attempt += 1;
                    let jitter = if self.retry_jitter_ms > 0 {
                        rand::random_range(0..self.retry_jitter_ms)
                    } else {
                        0
                    };
                    let wait_ms = (retry_after * 1000.0).ceil() as u64 + jitter;
                    log::warn(
                        "Telegram ograniczył tempo; ponawiam żądanie po opóźnieniu",
                        Some(json!({
                            "chatId": key,
                            "label": label,
                            "attempt": attempt,
                            "maxRetries": self.max_retries,
                            "retryAfterSeconds": retry_after,
                            "waitMs": wait_ms,
                        })),
                    );
                    tokio::time::sleep(Duration::from_millis(wait_ms)).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(value: i64, log: Arc<Mutex<Vec<i64>>>, busy_ms: u64) -> Task {
        Box::new(move || {
            let log = log.clone();
            Box::pin(async move {
                log.lock().unwrap().push(value);
                tokio::time::sleep(Duration::from_millis(busy_ms)).await;
                Ok(json!(value))
            })
        })
    }

    #[tokio::test(start_paused = true)]
    async fn priorities_dedupe_and_pacing() {
        let limiter = Limiter::new(3000, 5, 0);
        let order = Arc::new(Mutex::new(Vec::new()));
        let spawn = |value: i64, priority: i64, dedupe: Option<&str>| {
            let (limiter, order) = (limiter.clone(), order.clone());
            let dedupe = dedupe.map(str::to_owned);
            tokio::spawn(async move {
                limiter
                    .schedule("1".into(), task(value, order, 100), priority, dedupe, "t")
                    .await
            })
        };
        let started = Instant::now();
        let first = spawn(1, 10, None);
        tokio::time::sleep(Duration::from_millis(10)).await; // item 1 is running
        let low = spawn(2, 20, Some("edit"));
        tokio::time::sleep(Duration::from_millis(1)).await;
        let replaced = spawn(3, 20, Some("edit"));
        tokio::time::sleep(Duration::from_millis(1)).await;
        let high = spawn(4, 0, None);
        assert_eq!(first.await.unwrap().unwrap(), json!(1));
        assert_eq!(
            low.await.unwrap().unwrap(),
            Value::Null,
            "superseded edit resolves with null"
        );
        assert_eq!(high.await.unwrap().unwrap(), json!(4));
        assert_eq!(replaced.await.unwrap().unwrap(), json!(3));
        assert_eq!(*order.lock().unwrap(), vec![1, 4, 3]);
        // 3 requests of 100 ms separated by two 3 s gaps.
        assert!(started.elapsed() >= Duration::from_millis(6300));
    }
}
