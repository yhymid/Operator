//! Port of `lib/task-queue.js`, specialised for `/ytdlp` jobs: one shared priority queue
//! (lower estimated size first, FIFO on ties) with pause/resume/cancel/clear.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::json;

use crate::bot::Ctx;
use crate::log;
use crate::status::StatusUpdater;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Completed,
    Canceled,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Canceled => "canceled",
        }
    }
}

pub struct Job {
    pub id: String,
    sequence: u64,
    /// Estimated size in bytes; `f64::INFINITY` when unknown (`normalizePriority`).
    pub priority: f64,
    pub created_at_ms: u64,
    pub ctx: Ctx,
    pub url: String,
    pub estimated_size: Option<f64>,
    pub status: StatusUpdater,
    status_message_id: Option<i64>,
    state: Mutex<JobState>,
    started_at_ms: Mutex<Option<u64>>,
    last_position: Mutex<Option<usize>>,
    status_text: Mutex<Option<String>>,
    process: Mutex<Option<u32>>,
    cancel_requested: AtomicBool,
}

/// Sends `signal` to the process group led by `pid` (children are spawned with
/// `process_group(0)`), so helpers started by yt-dlp (ffmpeg merges) are reached too.
pub fn signal_group(pid: u32, signal: Signal) -> nix::Result<()> {
    let pid = i32::try_from(pid).map_err(|_| nix::errno::Errno::EINVAL)?;
    kill(Pid::from_raw(-pid), signal)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Job {
    pub fn status_message_id(&self) -> Option<i64> {
        self.status_message_id
    }

    pub fn state(&self) -> JobState {
        *lock(&self.state)
    }

    pub fn started_at_ms(&self) -> Option<u64> {
        *lock(&self.started_at_ms)
    }

    pub fn status_text(&self) -> Option<String> {
        lock(&self.status_text).clone()
    }

    pub fn set_status_text(&self, text: &str) {
        *lock(&self.status_text) = Some(text.to_owned());
    }

    /// `job.metadata.process = child`
    pub fn set_process(&self, pid: Option<u32>) {
        *lock(&self.process) = pid;
    }

    /// Set when a running job was cancelled; checked between processing steps.
    pub fn cancel_requested(&self) -> bool {
        self.cancel_requested.load(Ordering::SeqCst)
    }

    fn signal(&self, signal: Signal) {
        if let Some(pid) = *lock(&self.process)
            && let Err(error) = signal_group(pid, signal)
        {
            log::warn(
                &format!("Nie udało się wysłać {signal:?} do zadania {}", self.id),
                Some(json!(error.to_string())),
            );
        }
    }
}

/// `queueStatusText(position, queue, estimatedSizeBytes)`
pub fn queue_status_text(
    position: Option<usize>,
    pending: usize,
    estimated: Option<f64>,
) -> String {
    let suffix = if pending > 1 {
        format!("\n👥 Oczekujących: {pending}")
    } else {
        String::new()
    };
    let size_line = match estimated {
        Some(size) => format!(
            "\n📦 Szacowany rozmiar: {}",
            crate::ytdlp::format_bytes(size)
        ),
        None => "\n📦 Rozmiar: nieznany".to_owned(),
    };
    let position = position.map_or("?".to_owned(), |p| p.to_string());
    format!("⏳ Zadanie w kolejce…\n📍 Pozycja: {position}{size_line}{suffix}")
}

pub enum CancelOutcome {
    Canceled,
    CancelRequested,
    NotPending(JobState),
}

pub type Runner = Arc<dyn Fn(Arc<Job>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

#[derive(Default)]
struct Inner {
    pending: Vec<Arc<Job>>,
    running: Vec<Arc<Job>>,
    paused: bool,
    next_id: u64,
    next_sequence: u64,
}

pub struct Queue {
    pub name: String,
    pub concurrency: usize,
    pub max_pending: usize,
    inner: Mutex<Inner>,
    runner: OnceLock<Runner>,
    this: Weak<Queue>,
}

pub struct NewJob {
    pub ctx: Ctx,
    pub url: String,
    pub estimated_size: Option<f64>,
    pub status: StatusUpdater,
    pub status_message_id: Option<i64>,
}

impl Queue {
    pub fn new(name: &str, concurrency: usize, max_pending: usize) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            name: name.to_owned(),
            concurrency: concurrency.max(1),
            max_pending: max_pending.max(1),
            inner: Mutex::new(Inner {
                next_id: 1,
                next_sequence: 1,
                ..Inner::default()
            }),
            runner: OnceLock::new(),
            this: this.clone(),
        })
    }

    pub fn set_runner(&self, runner: Runner) {
        let _ = self.runner.set(runner);
    }

    pub fn paused(&self) -> bool {
        lock(&self.inner).paused
    }

    pub fn pending_count(&self) -> usize {
        lock(&self.inner).pending.len()
    }

    pub fn active_count(&self) -> usize {
        lock(&self.inner).running.len()
    }

    pub fn active_jobs(&self) -> Vec<Arc<Job>> {
        lock(&self.inner).running.clone()
    }

    /// Active and waiting jobs in display order.
    pub fn active_and_pending(&self) -> Vec<Arc<Job>> {
        let inner = lock(&self.inner);
        inner
            .running
            .iter()
            .chain(inner.pending.iter())
            .cloned()
            .collect()
    }

    fn position_in(inner: &Inner, job: &Arc<Job>) -> Option<usize> {
        match job.state() {
            JobState::Running => Some(1),
            JobState::Queued => inner
                .pending
                .iter()
                .position(|j| Arc::ptr_eq(j, job))
                .map(|index| inner.running.len() + index + 1),
            _ => None,
        }
    }

    /// One-based position across active and waiting jobs (a running job is 1).
    pub fn position(&self, job: &Arc<Job>) -> Option<usize> {
        Self::position_in(&lock(&self.inner), job)
    }

    /// `add(task, metadata)`; `Err(())` when the queue is full.
    pub fn add(&self, spec: NewJob) -> Result<Arc<Job>, ()> {
        let job = {
            let mut inner = lock(&self.inner);
            if inner.pending.len() >= self.max_pending {
                return Err(());
            }
            let id = format!("{}-{}", self.name, inner.next_id);
            inner.next_id += 1;
            let sequence = inner.next_sequence;
            inner.next_sequence += 1;
            let job = Arc::new(Job {
                id,
                sequence,
                priority: spec
                    .estimated_size
                    .filter(|s| s.is_finite() && *s >= 0.0)
                    .unwrap_or(f64::INFINITY),
                created_at_ms: now_ms(),
                ctx: spec.ctx,
                url: spec.url,
                estimated_size: spec.estimated_size,
                status: spec.status,
                status_message_id: spec.status_message_id,
                state: Mutex::new(JobState::Queued),
                started_at_ms: Mutex::new(None),
                last_position: Mutex::new(None),
                status_text: Mutex::new(None),
                process: Mutex::new(None),
                cancel_requested: AtomicBool::new(false),
            });
            let index = inner.pending.iter().position(|candidate| {
                job.priority < candidate.priority
                    || (job.priority == candidate.priority && job.sequence < candidate.sequence)
            });
            match index {
                Some(index) => inner.pending.insert(index, job.clone()),
                None => inner.pending.push(job.clone()),
            }
            job
        };
        self.refresh_positions();
        self.drain();
        Ok(job)
    }

    /// Notifies waiting jobs whose visible position changed (fire-and-forget status edits).
    fn refresh_positions(&self) {
        let updates: Vec<(Arc<Job>, String)> = {
            let inner = lock(&self.inner);
            let pending = inner.pending.len();
            inner
                .pending
                .iter()
                .filter_map(|job| {
                    let position = Self::position_in(&inner, job);
                    let mut last = lock(&job.last_position);
                    if *last == position {
                        return None;
                    }
                    *last = position;
                    (job.state() == JobState::Queued).then(|| {
                        (
                            job.clone(),
                            queue_status_text(position, pending, job.estimated_size),
                        )
                    })
                })
                .collect()
        };
        for (job, text) in updates {
            job.status.set_detached(text);
        }
    }

    fn drain(&self) {
        let started: Vec<Arc<Job>> = {
            let mut inner = lock(&self.inner);
            let mut started = Vec::new();
            while !inner.paused
                && inner.running.len() < self.concurrency
                && !inner.pending.is_empty()
            {
                let job = inner.pending.remove(0);
                *lock(&job.state) = JobState::Running;
                *lock(&job.started_at_ms) = Some(now_ms());
                inner.running.push(job.clone());
                started.push(job);
            }
            started
        };
        if let (Some(runner), Some(queue)) = (self.runner.get(), self.this.upgrade()) {
            for job in started {
                let runner = runner.clone();
                let queue = queue.clone();
                tokio::spawn(async move {
                    runner(job.clone()).await;
                    queue.finish(&job);
                });
            }
        }
        self.refresh_positions();
    }

    fn finish(&self, job: &Arc<Job>) {
        {
            let mut inner = lock(&self.inner);
            *lock(&job.state) = JobState::Completed;
            inner.running.retain(|j| !Arc::ptr_eq(j, job));
        }
        self.drain();
        self.refresh_positions();
    }

    /// Pause starting new jobs and stop (`SIGSTOP`) the running yt-dlp process.
    pub fn pause(&self) {
        let running = {
            let mut inner = lock(&self.inner);
            inner.paused = true;
            inner.running.clone()
        };
        for job in running {
            job.signal(Signal::SIGSTOP);
        }
    }

    /// Stop starting queued jobs (shutdown) without signalling the running process.
    pub fn stop_starting(&self) {
        lock(&self.inner).paused = true;
    }

    pub fn resume(&self) {
        let running = {
            let mut inner = lock(&self.inner);
            inner.paused = false;
            inner.running.clone()
        };
        for job in running {
            job.signal(Signal::SIGCONT);
        }
        self.drain();
    }

    fn on_cancel(job: &Arc<Job>) {
        job.cancel_requested.store(true, Ordering::SeqCst);
        job.signal(Signal::SIGTERM);
        // A job paused with SIGSTOP only acts on a handled SIGTERM (ffmpeg) once continued.
        job.signal(Signal::SIGCONT);
        job.status
            .set_detached(format!("❌ Zadanie {} zostało anulowane.", job.id));
    }

    pub fn cancel(&self, job: &Arc<Job>) -> CancelOutcome {
        let removed = {
            let mut inner = lock(&self.inner);
            match job.state() {
                JobState::Running => None,
                JobState::Queued => {
                    let Some(index) = inner.pending.iter().position(|j| Arc::ptr_eq(j, job)) else {
                        return CancelOutcome::NotPending(JobState::Queued);
                    };
                    inner.pending.remove(index);
                    *lock(&job.state) = JobState::Canceled;
                    Some(())
                }
                other => return CancelOutcome::NotPending(other),
            }
        };
        Self::on_cancel(job);
        if removed.is_none() {
            return CancelOutcome::CancelRequested;
        }
        self.refresh_positions();
        CancelOutcome::Canceled
    }

    /// Cancels running and waiting jobs; returns how many were affected.
    pub fn clear(&self) -> usize {
        let (running, pending) = {
            let inner = lock(&self.inner);
            (inner.running.clone(), inner.pending.clone())
        };
        running
            .iter()
            .chain(pending.iter())
            .filter(|job| !matches!(self.cancel(job), CancelOutcome::NotPending(_)))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn new_job(queue: &Queue, url: &str, size: Option<f64>) -> Arc<Job> {
        queue
            .add(NewJob {
                ctx: Ctx::new(&json!({ "update_id": 1 })),
                url: url.to_owned(),
                estimated_size: size,
                status: StatusUpdater::noop(),
                status_message_id: None,
            })
            .unwrap_or_else(|()| panic!("queue full"))
    }

    #[tokio::test]
    async fn smaller_jobs_first_fifo_on_ties_and_limits() {
        // No runner: the first job is moved to `running` and stays there.
        let queue = Queue::new("t", 1, 3);
        let running = new_job(&queue, "a", Some(10.0));
        let unknown = new_job(&queue, "b", None);
        let big = new_job(&queue, "c", Some(500.0));
        let small = new_job(&queue, "d", Some(5.0));
        assert_eq!(running.state(), JobState::Running);
        let order: Vec<String> = queue
            .active_and_pending()
            .iter()
            .map(|j| j.url.clone())
            .collect();
        assert_eq!(order, ["a", "d", "c", "b"]);
        assert_eq!(queue.position(&running), Some(1));
        assert_eq!(queue.position(&small), Some(2));
        assert_eq!(queue.position(&unknown), Some(4));
        assert!(
            queue
                .add(NewJob {
                    ctx: Ctx::new(&json!({})),
                    url: "e".into(),
                    estimated_size: None,
                    status: StatusUpdater::noop(),
                    status_message_id: None,
                })
                .is_err(),
            "max_pending counts waiting jobs only"
        );

        assert!(matches!(queue.cancel(&big), CancelOutcome::Canceled));
        assert_eq!(big.state(), JobState::Canceled);
        assert!(matches!(
            queue.cancel(&big),
            CancelOutcome::NotPending(JobState::Canceled)
        ));
        assert_eq!(queue.position(&unknown), Some(3));
        // clear(): one running (cancel requested) + two waiting.
        assert_eq!(queue.clear(), 3);
        assert_eq!(queue.pending_count(), 0);
    }

    /// `/ytdlp_admin pause` stops the process with SIGSTOP; a later cancel/clear must still
    /// terminate it. Linux kills a stopped process at once only when SIGTERM is fatal for it;
    /// programs with a SIGTERM handler (ffmpeg, a shell `trap`) stay stopped until SIGCONT.
    #[tokio::test]
    async fn cancel_terminates_a_paused_process() {
        let queue = Queue::new("t", 1, 5);
        let job = new_job(&queue, "a", None);
        let mut child = tokio::process::Command::new("sh")
            .args(["-c", "trap 'exit 0' TERM; while :; do sleep 0.05; done"])
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sleep");
        job.set_process(child.id());
        // Let the shell install its trap first.
        tokio::time::sleep(Duration::from_millis(300)).await;
        queue.pause();
        assert!(matches!(queue.cancel(&job), CancelOutcome::CancelRequested));
        let exited = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
        assert!(
            exited.is_ok(),
            "process paused with SIGSTOP ignored the cancel request"
        );
    }
}
