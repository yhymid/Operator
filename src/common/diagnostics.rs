//! Always-on diagnostics recorder (`OPERATOR_DIAGNOSTICS=1`).
//!
//! Every log line and every structured event is appended, redacted, to
//! `<dir>/events-YYYY-MM-DD.txt` (one JSON object per line, UTC days). A background thread
//! keeps per-day aggregates and rewrites `<dir>/summary-YYYY-MM-DD.md` every
//! `OPERATOR_DIAGNOSTICS_SUMMARY_MINUTES` and at shutdown.
//!
//! Aggregation is generic: every numeric field whose name ends in `Ms` becomes a latency
//! sample, grouped by the event name and its `op` field (for log lines: the message). A new
//! measurement needs no code here, only a `record()` call.
//!
//! Recording never blocks the bot: events go through a bounded queue and are dropped (and
//! counted) when the writer falls behind. Writing, redaction and aggregation run on their
//! own thread.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

const QUEUE: usize = 50_000;
const MAX_SAMPLES: usize = 20_000;
/// `*Ms` values above this are timestamps, not durations (about 11.5 days).
const MAX_DURATION_MS: f64 = 1.0e9;
/// Discord epoch (2015-01-01) in Unix milliseconds.
const DISCORD_EPOCH_MS: u64 = 1_420_070_400_000;
/// Keys whose values are never written.
const SECRET_KEYS: [&str; 9] = [
    "token",
    "password",
    "authorization",
    "secret",
    "cookie",
    "cookies",
    "api_hash",
    "apihash",
    "arl",
];

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub dir: PathBuf,
    pub retention_days: u64,
    pub max_file_bytes: u64,
    pub summary_every: Duration,
}

impl Options {
    /// `OPERATOR_DIAGNOSTICS=1` enables recording; `OPERATOR_DIAGNOSTICS_DIR` (default
    /// `<base>/diagnostics`), `_RETENTION_DAYS` (14), `_MAX_FILE_MB` (256 per day) and
    /// `_SUMMARY_MINUTES` (15) tune it.
    pub fn from_env(base: &Path) -> Option<Self> {
        Self::from_lookup(base, crate::common::env::var)
    }

    fn from_lookup(base: &Path, get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let on = get("OPERATOR_DIAGNOSTICS")
            .is_some_and(|raw| crate::common::env::parse_bool(raw.trim()));
        if !on {
            return None;
        }
        let dir = match get("OPERATOR_DIAGNOSTICS_DIR").filter(|d| !d.trim().is_empty()) {
            Some(raw) if Path::new(raw.trim()).is_absolute() => PathBuf::from(raw.trim()),
            Some(raw) => base.join(raw.trim()),
            None => base.join("diagnostics"),
        };
        let num = |name: &str, default: u64, min: u64, max: u64| {
            get(name)
                .and_then(|raw| raw.trim().parse::<u64>().ok())
                .unwrap_or(default)
                .clamp(min, max)
        };
        Some(Self {
            dir,
            retention_days: num("OPERATOR_DIAGNOSTICS_RETENTION_DAYS", 14, 1, 365),
            max_file_bytes: num("OPERATOR_DIAGNOSTICS_MAX_FILE_MB", 256, 1, 10_240) * 1024 * 1024,
            summary_every: Duration::from_secs(
                num("OPERATOR_DIAGNOSTICS_SUMMARY_MINUTES", 15, 1, 1440) * 60,
            ),
        })
    }
}

struct Record {
    at: DateTime<Utc>,
    event: String,
    payload: Value,
    flush: Option<SyncSender<()>>,
}

struct Recorder {
    tx: SyncSender<Record>,
    dropped: AtomicU64,
}

static RECORDER: OnceLock<Recorder> = OnceLock::new();

pub fn enabled() -> bool {
    RECORDER.get().is_some()
}

/// Starts the writer thread (once per process). `secrets` are values (tokens, passwords)
/// that are replaced with `[redacted]` wherever they appear.
pub fn init(options: Options, app: &str, secrets: Vec<String>) -> Result<(), String> {
    fs::create_dir_all(&options.dir)
        .map_err(|e| format!("cannot create {}: {e}", options.dir.display()))?;
    let (tx, rx) = mpsc::sync_channel(QUEUE);
    if RECORDER
        .set(Recorder {
            tx,
            dropped: AtomicU64::new(0),
        })
        .is_err()
    {
        return Ok(());
    }
    let dir = options.dir.clone();
    let writer = Writer::new(options, app.to_owned(), Redactor::new(secrets));
    std::thread::Builder::new()
        .name("diagnostics".into())
        .spawn(move || writer.run(&rx))
        .map_err(|e| format!("cannot start the diagnostics thread: {e}"))?;
    install_panic_hook();
    record(
        "diagnostics.started",
        json!({ "pid": std::process::id(), "app": app, "version": env!("CARGO_PKG_VERSION"), "commit": super::GIT_COMMIT,
                "dir": dir.display().to_string() }),
    );
    Ok(())
}

/// Values of environment variables that look like credentials (`*TOKEN*`, `*PASSWORD*`,
/// `*SECRET*`, `*KEY*`, `*HASH*`, `*ARL*`, `*COOKIE*`, and URLs with a user:password part).
pub fn secrets_from_env() -> Vec<String> {
    const MARKERS: [&str; 7] = [
        "TOKEN", "PASSWORD", "SECRET", "KEY", "HASH", "ARL", "COOKIE",
    ];
    std::env::vars()
        .filter(|(name, value)| {
            let upper = name.to_ascii_uppercase();
            MARKERS.iter().any(|m| upper.contains(m))
                || (value.contains("://") && value.contains('@'))
        })
        .map(|(_, value)| value)
        .collect()
}

/// Records an event. Redaction happens on the writer thread.
pub fn record(event: &str, payload: Value) {
    push(event, payload, None);
}

/// Records one log line (`level`: debug/info/warn/error). Object `details` are flattened
/// into the event, anything else is kept under `details`.
pub fn log_line(level: &str, message: &str, details: Option<&Value>) {
    if !enabled() {
        return;
    }
    let mut payload = Map::new();
    payload.insert("level".into(), json!(level));
    payload.insert("message".into(), json!(message));
    match details {
        Some(Value::Object(map)) => {
            for (key, value) in map {
                if !matches!(key.as_str(), "level" | "message" | "t" | "e") {
                    payload.insert(key.clone(), value.clone());
                }
            }
        }
        Some(Value::Null) | None => {}
        Some(other) => {
            payload.insert("details".into(), other.clone());
        }
    }
    record("log", Value::Object(payload));
}

fn push(event: &str, payload: Value, flush: Option<SyncSender<()>>) -> bool {
    let Some(recorder) = RECORDER.get() else {
        return false;
    };
    let record = Record {
        at: Utc::now(),
        event: event.to_owned(),
        payload,
        flush,
    };
    match recorder.tx.try_send(record) {
        Ok(()) => true,
        Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
            recorder.dropped.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}

/// Writes everything queued and the summary; waits up to 2 s (shutdown).
pub fn flush() {
    let (tx, rx) = mpsc::sync_channel(1);
    if push("diagnostics.flush", json!({}), Some(tx)) {
        let _ = rx.recv_timeout(Duration::from_secs(2));
    }
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let thread = std::thread::current().name().unwrap_or("?").to_owned();
        let (tx, rx) = mpsc::sync_channel(1);
        if push(
            "panic",
            json!({ "message": message, "location": location, "thread": thread }),
            Some(tx),
        ) {
            let _ = rx.recv_timeout(Duration::from_millis(500));
        }
        previous(info);
    }));
}

// ----------------------------------------------------------------------------- helpers --

/// Milliseconds as `f64` with 0.1 ms precision.
pub fn ms(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 10_000.0).round() / 10.0
}

pub fn elapsed_ms(since: Instant) -> f64 {
    ms(since.elapsed())
}

/// Time from a Discord snowflake's creation to now (negative when our clock is behind).
pub fn snowflake_age_ms(id: u64) -> f64 {
    let created = (id >> 22) + DISCORD_EPOCH_MS;
    Utc::now().timestamp_millis() as f64 - created as f64
}

/// Server clock minus ours, from an HTTP `Date` header (1 s resolution).
pub fn date_skew_ms(date_header: &str) -> Option<f64> {
    let server = DateTime::parse_from_rfc2822(date_header.trim()).ok()?;
    Some((server.timestamp_millis() - Utc::now().timestamp_millis()) as f64)
}

/// Records an anomaly (unexpected data, clock skew, impossible state).
pub fn anomaly(kind: &str, mut details: Value) {
    if !enabled() {
        return;
    }
    if let Value::Object(map) = &mut details {
        map.insert("kind".into(), json!(kind));
    } else {
        details = json!({ "kind": kind, "details": details });
    }
    record("anomaly", details);
}

/// Path with ids and long numbers replaced by `{id}` and the query string removed, so
/// requests group by route.
pub fn route_template(path: &str) -> String {
    let path = path.split('?').next().unwrap_or_default();
    path.split('/')
        .map(|part| {
            if part.len() >= 5 && part.chars().all(|c| c.is_ascii_digit()) {
                "{id}"
            } else if part.len() >= 32 {
                "{token}"
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Records one HTTP exchange. `date` is the response `Date` header; a server clock that
/// differs from ours by more than 2 s is reported as an anomaly.
pub struct HttpTiming<'a> {
    pub client: &'a str,
    pub method: &'a str,
    pub host: &'a str,
    pub path: &'a str,
    pub status: Option<u16>,
    pub ttfb: Option<Duration>,
    pub body: Option<Duration>,
    pub total: Duration,
    pub bytes: Option<usize>,
    pub date: Option<&'a str>,
    pub error: Option<&'a str>,
}

impl HttpTiming<'_> {
    pub fn record(&self) {
        if !enabled() {
            return;
        }
        let route = route_template(self.path);
        let mut payload = json!({
            "op": format!("{} {}{}", self.method, self.host, route),
            "client": self.client,
            "status": self.status,
            "ok": self.status.is_some_and(|s| s < 400) && self.error.is_none(),
            "totalMs": ms(self.total),
        });
        if let Some(ttfb) = self.ttfb {
            payload["ttfbMs"] = json!(ms(ttfb));
        }
        if let Some(body) = self.body {
            payload["bodyMs"] = json!(ms(body));
        }
        if let Some(bytes) = self.bytes {
            payload["bytes"] = json!(bytes);
        }
        if let Some(error) = self.error {
            payload["error"] = json!(error.chars().take(300).collect::<String>());
        }
        if let Some(skew) = self.date.and_then(date_skew_ms) {
            payload["clockSkew"] = json!(skew);
            if skew.abs() > 2_000.0 {
                anomaly("clock_skew", json!({ "host": self.host, "skewMs": skew }));
            }
        }
        record("http", payload);
    }
}

// ---------------------------------------------------------------------- tracing layer --

/// `tracing` layer that copies every log line into the recorder.
pub struct TracingLayer;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for TracingLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if !enabled() {
            return;
        }
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        let level = match *event.metadata().level() {
            tracing::Level::ERROR => "error",
            tracing::Level::WARN => "warn",
            tracing::Level::INFO => "info",
            tracing::Level::DEBUG => "debug",
            tracing::Level::TRACE => "trace",
        };
        let mut details = visitor.fields;
        details.insert("target".into(), json!(event.metadata().target()));
        log_line(level, &visitor.message, Some(&Value::Object(details)));
    }
}

#[derive(Default)]
struct FieldVisitor {
    message: String,
    fields: Map<String, Value>,
}

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields
                .insert(field.name().to_owned(), json!(format!("{value:?}")));
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_owned();
        } else {
            self.fields.insert(field.name().to_owned(), json!(value));
        }
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields.insert(field.name().to_owned(), json!(value));
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields.insert(field.name().to_owned(), json!(value));
    }
    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.fields.insert(field.name().to_owned(), json!(value));
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields.insert(field.name().to_owned(), json!(value));
    }
}

// ------------------------------------------------------------------------- sampler --

/// Samples the process every minute and measures how late the tokio scheduler wakes a
/// timer (a blocked runtime delays every command). Returns at once when disabled.
pub fn spawn_sampler() {
    if !enabled() {
        return;
    }
    tokio::spawn(async {
        const TICK: Duration = Duration::from_millis(250);
        let mut lags: Vec<f64> = Vec::with_capacity(256);
        let mut last_sample = Instant::now();
        sample_process();
        loop {
            let before = Instant::now();
            tokio::time::sleep(TICK).await;
            lags.push(ms(before.elapsed().saturating_sub(TICK)));
            if last_sample.elapsed() >= Duration::from_secs(60) {
                last_sample = Instant::now();
                lags.sort_by(f64::total_cmp);
                let pick = |q: f64| lags[((lags.len() - 1) as f64 * q).round() as usize];
                record(
                    "runtime.lag",
                    json!({ "samples": lags.len(), "p50Ms": pick(0.5), "p99Ms": pick(0.99),
                            "maxMs": lags[lags.len() - 1] }),
                );
                lags.clear();
                sample_process();
            }
        }
    });
}

fn sample_process() {
    let mut payload = Map::new();
    let pid = std::process::id();
    if let Some((rss, threads)) = proc_status(pid) {
        payload.insert("rssKb".into(), json!(rss));
        payload.insert("threads".into(), json!(threads));
    }
    if let Some((user, system)) = proc_cpu_ms(pid) {
        payload.insert("cpuUserMsTotal".into(), json!(user));
        payload.insert("cpuSysMsTotal".into(), json!(system));
    }
    if let Ok(entries) = fs::read_dir("/proc/self/fd") {
        payload.insert("fds".into(), json!(entries.count()));
    }
    let children = descendants(pid);
    let child_rss: u64 = children
        .iter()
        .filter_map(|(p, _)| proc_status(*p).map(|(r, _)| r))
        .sum();
    let mut names: Vec<String> = children.into_iter().map(|(_, name)| name).collect();
    names.sort();
    payload.insert("childProcesses".into(), json!(names.len()));
    payload.insert("childNames".into(), json!(names));
    payload.insert("childRssKb".into(), json!(child_rss));
    if let Ok(load) = fs::read_to_string("/proc/loadavg")
        && let Some(first) = load.split_whitespace().next()
    {
        payload.insert("load1".into(), json!(first.parse::<f64>().unwrap_or(0.0)));
    }
    host_sample(&mut payload);
    if let Some(recorder) = RECORDER.get() {
        payload.insert(
            "droppedEvents".into(),
            json!(recorder.dropped.load(Ordering::Relaxed)),
        );
    }
    record("process.sample", Value::Object(payload));
}

fn proc_status(pid: u32) -> Option<(u64, u64)> {
    let text = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let field = |name: &str| {
        text.lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
    };
    Some((field("VmRSS:")?, field("Threads:").unwrap_or(0)))
}

/// `(comm, fields after comm)` from `/proc/<pid>/stat`.
fn proc_stat(pid: u32) -> Option<(String, Vec<String>)> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let name = text.get(open + 1..close)?.to_owned();
    let rest = text.get(close + 2..)?;
    Some((name, rest.split_whitespace().map(str::to_owned).collect()))
}

/// `/proc/stat` "cpu" counters of the previous sample.
static HOST_CPU: Mutex<Option<[u64; 8]>> = Mutex::new(None);

/// Whole-server CPU (all services, not just this bot) since the previous sample, plus memory:
/// `hostCpuPct` (busy incl. steal), `hostIowaitPct`, `hostStealPct` (time the VPS host gave to
/// other guests), `memAvailableMb`, `swapUsedMb`.
fn host_sample(payload: &mut Map<String, Value>) {
    if let Some(now) = fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|t| host_cpu_counters(&t))
    {
        let mut prev = HOST_CPU.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((busy, iowait, steal)) = prev.and_then(|p| host_cpu_pct(&p, &now)) {
            let round = |v: f64| (v * 10.0).round() / 10.0;
            payload.insert("hostCpuPct".into(), json!(round(busy)));
            payload.insert("hostIowaitPct".into(), json!(round(iowait)));
            payload.insert("hostStealPct".into(), json!(round(steal)));
        }
        *prev = Some(now);
    }
    if let Ok(text) = fs::read_to_string("/proc/meminfo") {
        if let Some(avail) = meminfo_mb(&text, "MemAvailable") {
            payload.insert("memAvailableMb".into(), json!(avail.round()));
        }
        if let (Some(total), Some(free)) = (
            meminfo_mb(&text, "SwapTotal"),
            meminfo_mb(&text, "SwapFree"),
        ) {
            payload.insert("swapUsedMb".into(), json!((total - free).max(0.0).round()));
        }
    }
}

/// user nice system idle iowait irq softirq steal from the aggregate `cpu` line.
fn host_cpu_counters(stat: &str) -> Option<[u64; 8]> {
    let mut fields = stat.lines().next()?.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let mut out = [0u64; 8];
    for slot in &mut out {
        *slot = fields.next()?.parse().ok()?;
    }
    Some(out)
}

/// (busy, iowait, steal) as percent of all CPU time between two counter sets.
fn host_cpu_pct(prev: &[u64; 8], now: &[u64; 8]) -> Option<(f64, f64, f64)> {
    let d: Vec<f64> = now
        .iter()
        .zip(prev)
        .map(|(a, b)| a.saturating_sub(*b) as f64)
        .collect();
    let total: f64 = d.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let idle = d[3] + d[4];
    Some((
        (total - idle) / total * 100.0,
        d[4] / total * 100.0,
        d[7] / total * 100.0,
    ))
}

fn meminfo_mb(text: &str, key: &str) -> Option<f64> {
    let line = text.lines().find(|l| l.split(':').next() == Some(key))?;
    let kb = line.split_whitespace().nth(1)?.parse::<f64>().ok()?;
    Some(kb / 1024.0)
}

fn proc_cpu_ms(pid: u32) -> Option<(u64, u64)> {
    let (_, fields) = proc_stat(pid)?;
    // utime/stime are fields 14/15 of the stat line (index 11/12 after the comm field);
    // USER_HZ is 100 on Linux.
    let user = fields.get(11)?.parse::<u64>().ok()?;
    let system = fields.get(12)?.parse::<u64>().ok()?;
    Some((user * 10, system * 10))
}

/// All descendant processes (yt-dlp, ffmpeg, ffprobe) with their names.
fn descendants(root: u32) -> Vec<(u32, String)> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut parent: BTreeMap<u32, (u32, String)> = BTreeMap::new();
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if let Some((name, fields)) = proc_stat(pid)
            && let Some(ppid) = fields.first().and_then(|p| p.parse::<u32>().ok())
        {
            parent.insert(pid, (ppid, name));
        }
    }
    let mut found = Vec::new();
    for (&pid, (_, name)) in &parent {
        let mut current = pid;
        for _ in 0..64 {
            let Some((up, _)) = parent.get(&current) else {
                break;
            };
            if *up == root {
                found.push((pid, name.clone()));
                break;
            }
            if *up <= 1 {
                break;
            }
            current = *up;
        }
    }
    found
}

// ---------------------------------------------------------------------------- redaction --

struct Redactor {
    secrets: Vec<String>,
    patterns: Vec<regex::Regex>,
}

impl Redactor {
    fn new(secrets: Vec<String>) -> Self {
        let mut secrets: Vec<String> = secrets
            .into_iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| s.len() >= 6)
            .collect();
        // Longest first, so a secret containing another is replaced whole.
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        let patterns = [
            // Telegram bot token.
            r"\d{6,12}:[A-Za-z0-9_-]{30,}",
            // Discord bot token.
            r"\b[A-Za-z0-9_-]{23,28}\.[A-Za-z0-9_-]{6}\.[A-Za-z0-9_-]{27,40}",
            // Discord interaction token.
            r"aW50ZXJhY3Rpb246[A-Za-z0-9_-]+",
            // Webhook URLs carry their token in the path.
            r"(?i)(webhooks/\d+/)[A-Za-z0-9_-]+",
            // key=value secrets in URLs and messages.
            r"(?i)\b(token|password|passwd|secret|api_hash|apikey|api_key|authorization|arl)(=|:\s*)[^\s&,;]+",
        ]
        .into_iter()
        .filter_map(|p| regex::Regex::new(p).ok())
        .collect();
        Self { secrets, patterns }
    }

    fn text(&self, input: &str) -> String {
        let mut out = input.to_owned();
        for secret in &self.secrets {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), "[redacted]");
            }
        }
        for (index, pattern) in self.patterns.iter().enumerate() {
            if pattern.is_match(&out) {
                out = match index {
                    3 => pattern.replace_all(&out, "${1}[redacted]").into_owned(),
                    4 => pattern.replace_all(&out, "${1}${2}[redacted]").into_owned(),
                    _ => pattern.replace_all(&out, "[redacted]").into_owned(),
                };
            }
        }
        out
    }

    fn value(&self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.text(text),
            Value::Array(items) => items.iter_mut().for_each(|v| self.value(v)),
            Value::Object(map) => {
                for (key, item) in map.iter_mut() {
                    let lower = key.to_ascii_lowercase();
                    if SECRET_KEYS.contains(&lower.as_str()) && !item.is_null() {
                        *item = json!("[redacted]");
                    } else {
                        self.value(item);
                    }
                }
            }
            _ => {}
        }
    }
}

// ------------------------------------------------------------------------------ writer --

struct Writer {
    options: Options,
    app: String,
    redactor: Redactor,
    day: String,
    file: Option<BufWriter<File>>,
    written: u64,
    capped: bool,
    agg: Aggregates,
    last_summary: Instant,
    started: DateTime<Utc>,
}

impl Writer {
    fn new(options: Options, app: String, redactor: Redactor) -> Self {
        Self {
            options,
            app,
            redactor,
            day: String::new(),
            file: None,
            written: 0,
            capped: false,
            agg: Aggregates::default(),
            last_summary: Instant::now(),
            started: Utc::now(),
        }
    }

    fn run(mut self, rx: &Receiver<Record>) {
        loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(record) => {
                    let mut waiters = Vec::new();
                    self.handle(record, &mut waiters);
                    while let Ok(next) = rx.try_recv() {
                        self.handle(next, &mut waiters);
                    }
                    if !waiters.is_empty() {
                        self.flush_file();
                        self.write_summary();
                        for waiter in waiters {
                            let _ = waiter.try_send(());
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.flush_file();
                    self.write_summary();
                    return;
                }
            }
            self.flush_file();
            if self.last_summary.elapsed() >= self.options.summary_every {
                self.write_summary();
            }
        }
    }

    fn handle(&mut self, mut record: Record, waiters: &mut Vec<SyncSender<()>>) {
        if let Some(waiter) = record.flush.take() {
            waiters.push(waiter);
            return;
        }
        let day = record.at.format("%Y-%m-%d").to_string();
        if day != self.day {
            if !self.day.is_empty() {
                self.write_summary();
            }
            self.rotate(&day);
        }
        self.redactor.value(&mut record.payload);
        self.agg.observe(&record);
        if self.capped {
            return;
        }
        let mut line = Map::new();
        line.insert(
            "t".into(),
            json!(
                record
                    .at
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            ),
        );
        line.insert("e".into(), json!(record.event));
        if let Value::Object(map) = record.payload {
            for (key, value) in map {
                if key != "t" && key != "e" {
                    line.insert(key, value);
                }
            }
        }
        // Second pass over the whole line catches secrets split across fields.
        let mut text = self.redactor.text(&Value::Object(line).to_string());
        text.push('\n');
        if self.written + text.len() as u64 > self.options.max_file_bytes {
            self.capped = true;
            text = format!(
                "{}\n",
                json!({ "t": record.at.to_rfc3339(), "e": "diagnostics.capped",
                        "message": "OPERATOR_DIAGNOSTICS_MAX_FILE_MB reached; further events today are only aggregated" })
            );
        }
        if let Some(file) = self.file.as_mut()
            && file.write_all(text.as_bytes()).is_ok()
        {
            self.written += text.len() as u64;
        }
    }

    fn flush_file(&mut self) {
        if let Some(file) = self.file.as_mut() {
            let _ = file.flush();
        }
    }

    fn rotate(&mut self, day: &str) {
        self.flush_file();
        self.day = day.to_owned();
        self.agg = Aggregates::default();
        self.capped = false;
        let path = self.options.dir.join(format!("events-{day}.txt"));
        self.written = fs::metadata(&path).map_or(0, |m| m.len());
        // A restart the same day continues that day's report instead of overwriting it.
        self.replay(&path);
        self.file = open_private(&path).map(BufWriter::new);
        self.cleanup();
    }

    /// Feeds an existing events file back into the aggregates (already redacted).
    fn replay(&mut self, path: &Path) {
        let Ok(file) = File::open(path) else {
            return;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(Value::Object(mut map)) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let at = map
                .remove("t")
                .and_then(|t| {
                    t.as_str()
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                })
                .map(|t| t.with_timezone(&Utc));
            let event = map.remove("e").and_then(|e| e.as_str().map(str::to_owned));
            if let (Some(at), Some(event)) = (at, event) {
                if event == "diagnostics.capped" {
                    self.capped = true;
                }
                self.agg.observe(&Record {
                    at,
                    event,
                    payload: Value::Object(map),
                    flush: None,
                });
            }
        }
    }

    fn cleanup(&self) {
        let cutoff = Utc::now() - chrono::Duration::days(self.options.retention_days as i64);
        let cutoff = cutoff.format("%Y-%m-%d").to_string();
        let Ok(entries) = fs::read_dir(&self.options.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let day = name
                .strip_prefix("events-")
                .or_else(|| name.strip_prefix("summary-"))
                .and_then(|rest| rest.get(..10));
            if let Some(day) = day
                && day < cutoff.as_str()
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    fn write_summary(&mut self) {
        self.last_summary = Instant::now();
        if self.day.is_empty() {
            return;
        }
        let dropped = RECORDER
            .get()
            .map_or(0, |r| r.dropped.load(Ordering::Relaxed));
        let text = self
            .agg
            .render(&self.app, &self.day, self.started, dropped, self.capped);
        let path = self.options.dir.join(format!("summary-{}.md", self.day));
        let tmp = path.with_extension("md.tmp");
        if let Some(mut file) = open_private_truncate(&tmp)
            && file.write_all(text.as_bytes()).is_ok()
        {
            drop(file);
            let _ = fs::rename(&tmp, &path);
        }
    }
}

/// Owner-only files: the events contain user and chat ids.
fn open_private(path: &Path) -> Option<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path).ok()
}

fn open_private_truncate(path: &Path) -> Option<File> {
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path).ok()
}

// -------------------------------------------------------------------------- aggregates --

#[derive(Default)]
struct Samples(Vec<f64>);

impl Samples {
    fn push(&mut self, value: f64) {
        if value.is_finite() {
            if self.0.len() >= MAX_SAMPLES {
                self.0.remove(0);
            }
            self.0.push(value);
        }
    }

    fn stats(&self) -> Option<Stats> {
        if self.0.is_empty() {
            return None;
        }
        let mut sorted = self.0.clone();
        sorted.sort_by(f64::total_cmp);
        let pick = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
        Some(Stats {
            n: sorted.len(),
            p50: pick(0.5),
            p90: pick(0.9),
            p99: pick(0.99),
            max: sorted[sorted.len() - 1],
            sum: sorted.iter().sum(),
        })
    }
}

#[derive(Clone, Copy)]
struct Stats {
    n: usize,
    p50: f64,
    p90: f64,
    p99: f64,
    max: f64,
    sum: f64,
}

#[derive(Default)]
struct Outcomes {
    total: u64,
    failed: u64,
    statuses: BTreeMap<String, u64>,
    last_error: Option<String>,
}

#[derive(Default)]
struct ProcessAgg {
    rss_first: Option<f64>,
    rss_last: Option<f64>,
    rss_max: f64,
    child_rss_max: f64,
    children_max: f64,
    fds_max: f64,
    threads_max: f64,
    load_max: f64,
    host_cpu_sum: f64,
    host_cpu_n: f64,
    host_cpu_max: f64,
    iowait_max: f64,
    steal_max: f64,
    mem_available_min: Option<f64>,
    swap_used_max: f64,
    /// CPU and wall time summed over consecutive samples of the same process.
    cpu_ms: f64,
    wall_ms: f64,
    last_cpu: Option<(DateTime<Utc>, f64)>,
    child_names: BTreeMap<String, u64>,
    restarts: u64,
}

#[derive(Default)]
struct Aggregates {
    events: BTreeMap<String, u64>,
    /// `(group, field)` → samples; group = event plus its `op` (or the log message).
    timings: BTreeMap<(String, String), Samples>,
    outcomes: BTreeMap<String, Outcomes>,
    process: ProcessAgg,
    anomalies: BTreeMap<String, (u64, Vec<String>)>,
    logs: BTreeMap<(String, String), (u64, String)>,
    panics: Vec<String>,
    first: Option<DateTime<Utc>>,
    last: Option<DateTime<Utc>>,
}

fn num(payload: &Value, key: &str) -> Option<f64> {
    payload.get(key).and_then(Value::as_f64)
}

fn text(payload: &Value, key: &str) -> String {
    match payload.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Digits become `#` so log messages with ids group together.
fn normalize(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut last_hash = false;
    for c in message.chars().take(140) {
        if c.is_ascii_digit() {
            if !last_hash {
                out.push('#');
            }
            last_hash = true;
        } else {
            out.push(c);
            last_hash = false;
        }
    }
    out
}

fn group_of(event: &str, payload: &Value) -> String {
    if event == "log" {
        return format!("log: {}", normalize(&text(payload, "message")));
    }
    match payload.get("op").and_then(Value::as_str) {
        Some(op) if !op.is_empty() => format!("{event} {op}"),
        _ => event.to_owned(),
    }
}

impl Aggregates {
    fn observe(&mut self, record: &Record) {
        let p = &record.payload;
        *self.events.entry(record.event.clone()).or_default() += 1;
        self.first.get_or_insert(record.at);
        self.last = Some(record.at);
        let group = group_of(&record.event, p);

        if record.event != "process.sample"
            && let Value::Object(map) = p
        {
            for (key, value) in map {
                if key.len() > 2
                    && key.ends_with("Ms")
                    && let Some(v) = value.as_f64()
                    && v.abs() < MAX_DURATION_MS
                {
                    self.timings
                        .entry((group.clone(), key.clone()))
                        .or_default()
                        .push(v);
                }
            }
        }

        if let Some(ok) = p.get("ok").and_then(Value::as_bool) {
            let entry = self.outcomes.entry(group.clone()).or_default();
            entry.total += 1;
            if !ok {
                entry.failed += 1;
                let error = text(p, "error");
                if !error.is_empty() {
                    entry.last_error = Some(error.chars().take(200).collect());
                }
            }
            if let Some(status) = p.get("status").filter(|s| !s.is_null()) {
                *entry.statuses.entry(status.to_string()).or_default() += 1;
            }
        }

        match record.event.as_str() {
            "process.sample" => self.observe_process(record.at, p),
            "diagnostics.started" => {
                // Memory growth and CPU are tracked per process.
                self.process.restarts += 1;
                self.process.rss_first = None;
                self.process.last_cpu = None;
            }
            "anomaly" => {
                let entry = self.anomalies.entry(text(p, "kind")).or_default();
                entry.0 += 1;
                if entry.1.len() < 5 {
                    let mut example = p.clone();
                    if let Value::Object(map) = &mut example {
                        map.remove("kind");
                    }
                    entry
                        .1
                        .push(example.to_string().chars().take(300).collect());
                }
            }
            "log" => {
                let level = text(p, "level");
                if level == "warn" || level == "error" {
                    let message = text(p, "message");
                    let entry = self
                        .logs
                        .entry((level, normalize(&message)))
                        .or_insert_with(|| (0, String::new()));
                    entry.0 += 1;
                    entry.1 = message.chars().take(300).collect();
                }
            }
            "panic" => self.panics.push(format!(
                "{} at {} ({})",
                text(p, "message"),
                text(p, "location"),
                text(p, "thread")
            )),
            _ => {}
        }
    }

    fn observe_process(&mut self, at: DateTime<Utc>, p: &Value) {
        let proc = &mut self.process;
        if let Some(kb) = num(p, "rssKb") {
            proc.rss_first.get_or_insert(kb);
            proc.rss_last = Some(kb);
            proc.rss_max = proc.rss_max.max(kb);
        }
        proc.child_rss_max = proc.child_rss_max.max(num(p, "childRssKb").unwrap_or(0.0));
        proc.children_max = proc
            .children_max
            .max(num(p, "childProcesses").unwrap_or(0.0));
        proc.fds_max = proc.fds_max.max(num(p, "fds").unwrap_or(0.0));
        proc.threads_max = proc.threads_max.max(num(p, "threads").unwrap_or(0.0));
        proc.load_max = proc.load_max.max(num(p, "load1").unwrap_or(0.0));
        if let Some(cpu) = num(p, "hostCpuPct") {
            proc.host_cpu_sum += cpu;
            proc.host_cpu_n += 1.0;
            proc.host_cpu_max = proc.host_cpu_max.max(cpu);
        }
        proc.iowait_max = proc.iowait_max.max(num(p, "hostIowaitPct").unwrap_or(0.0));
        proc.steal_max = proc.steal_max.max(num(p, "hostStealPct").unwrap_or(0.0));
        if let Some(mb) = num(p, "memAvailableMb") {
            proc.mem_available_min = Some(proc.mem_available_min.map_or(mb, |m: f64| m.min(mb)));
        }
        proc.swap_used_max = proc.swap_used_max.max(num(p, "swapUsedMb").unwrap_or(0.0));
        if let Some(Value::Array(names)) = p.get("childNames") {
            for name in names.iter().filter_map(Value::as_str) {
                *proc.child_names.entry(name.to_owned()).or_default() += 1;
            }
        }
        if let (Some(u), Some(s)) = (num(p, "cpuUserMsTotal"), num(p, "cpuSysMsTotal")) {
            let cpu = u + s;
            // Counters restart with the process; only deltas within one process count.
            if let Some((t0, c0)) = proc.last_cpu
                && cpu >= c0
            {
                proc.cpu_ms += cpu - c0;
                proc.wall_ms += (at - t0).num_milliseconds().max(0) as f64;
            }
            proc.last_cpu = Some((at, cpu));
        }
    }

    fn stat(&self, group: &str, field: &str) -> Option<Stats> {
        self.timings
            .get(&(group.to_owned(), field.to_owned()))
            .and_then(Samples::stats)
    }

    /// Stats of `field` over every group whose name starts with `prefix`.
    fn stat_prefix(&self, prefix: &str, field: &str) -> Option<Stats> {
        let mut all = Samples::default();
        for ((group, f), samples) in &self.timings {
            if f == field && group.starts_with(prefix) {
                for v in &samples.0 {
                    all.push(*v);
                }
            }
        }
        all.stats()
    }

    fn render(
        &self,
        app: &str,
        day: &str,
        started: DateTime<Utc>,
        dropped: u64,
        capped: bool,
    ) -> String {
        let mut out = String::new();
        let now = Utc::now();
        let _ = writeln!(out, "# {app} diagnostics - {day} (UTC)\n");
        let _ = writeln!(
            out,
            "Updated {} · process started {} · events today {} · dropped {dropped}{}\n",
            now.format("%H:%M:%S"),
            started.format("%Y-%m-%d %H:%M:%S"),
            self.events.values().sum::<u64>(),
            if capped {
                " · **file size cap reached, events only aggregated**"
            } else {
                ""
            }
        );

        let _ = writeln!(out, "## Hints\n");
        let hints = self.hints(dropped);
        if hints.is_empty() {
            let _ = writeln!(out, "- nothing stands out yet\n");
        } else {
            for hint in hints {
                let _ = writeln!(out, "- {hint}");
            }
            out.push('\n');
        }

        let _ = writeln!(
            out,
            "## Where the time goes\n\nEvery `*Ms` field, slowest total first (n x mean).\n"
        );
        let _ = writeln!(
            out,
            "| group | field | n | p50 | p90 | p99 | max | total |\n| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |"
        );
        let mut rows: Vec<(&(String, String), Stats)> = self
            .timings
            .iter()
            .filter_map(|(k, s)| s.stats().map(|st| (k, st)))
            .collect();
        rows.sort_by(|a, b| b.1.sum.total_cmp(&a.1.sum));
        for ((group, field), st) in rows.iter().take(60) {
            let _ = writeln!(
                out,
                "| {} | {field} | {} | {} | {} | {} | {} | {} |",
                cell(group),
                st.n,
                fmt_ms(st.p50),
                fmt_ms(st.p90),
                fmt_ms(st.p99),
                fmt_ms(st.max),
                fmt_ms(st.sum)
            );
        }
        if rows.len() > 60 {
            let _ = writeln!(out, "\n{} more rows in the events file.", rows.len() - 60);
        }

        let failing: Vec<_> = self
            .outcomes
            .iter()
            .filter(|(_, o)| o.failed > 0 || o.statuses.keys().any(|s| !s.starts_with('2')))
            .collect();
        if !failing.is_empty() {
            let _ = writeln!(
                out,
                "\n## Failures and status codes\n\n| group | failed / total | statuses | last error |\n| --- | ---: | --- | --- |"
            );
            for (group, o) in failing {
                let statuses: Vec<String> =
                    o.statuses.iter().map(|(s, n)| format!("{s}x{n}")).collect();
                let _ = writeln!(
                    out,
                    "| {} | {} / {} | {} | {} |",
                    cell(group),
                    o.failed,
                    o.total,
                    statuses.join(" "),
                    cell(o.last_error.as_deref().unwrap_or(""))
                );
            }
        }

        let proc = &self.process;
        if let Some(rss) = proc.rss_last {
            let _ = writeln!(out, "\n## Process\n");
            let _ = writeln!(
                out,
                "- RSS: now {:.1} MB, first {:.1} MB, max {:.1} MB; threads max {}; fds max {}",
                rss / 1024.0,
                proc.rss_first.unwrap_or(rss) / 1024.0,
                proc.rss_max / 1024.0,
                proc.threads_max,
                proc.fds_max
            );
            if proc.wall_ms > 0.0 {
                let _ = writeln!(
                    out,
                    "- CPU: {:.2}% of one core on average ({:.1} s over {:.1} h)",
                    proc.cpu_ms / proc.wall_ms * 100.0,
                    proc.cpu_ms / 1000.0,
                    proc.wall_ms / 3_600_000.0
                );
            }
            if proc.restarts > 0 {
                let _ = writeln!(out, "- process starts today: {}", proc.restarts);
            }
            let _ = writeln!(
                out,
                "- child processes: max {} at once, max {:.1} MB RSS; seen: {}",
                proc.children_max,
                proc.child_rss_max / 1024.0,
                if proc.child_names.is_empty() {
                    "none".to_owned()
                } else {
                    proc.child_names
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            );
            let _ = writeln!(out, "- host load (1 min): max {:.2}", proc.load_max);
            if proc.host_cpu_n > 0.0 {
                let _ = writeln!(
                    out,
                    "- host CPU (all services): avg {:.0}%, max {:.0}%; iowait max {:.1}%; steal max {:.1}%",
                    proc.host_cpu_sum / proc.host_cpu_n,
                    proc.host_cpu_max,
                    proc.iowait_max,
                    proc.steal_max
                );
            }
            if let Some(mb) = proc.mem_available_min {
                let _ = writeln!(
                    out,
                    "- host memory: available min {mb:.0} MB; swap used max {:.0} MB",
                    proc.swap_used_max
                );
            }
        }

        if !self.anomalies.is_empty() {
            let _ = writeln!(out, "\n## Anomalies\n");
            for (kind, (count, examples)) in &self.anomalies {
                let _ = writeln!(out, "- **{kind}** x{count}");
                for example in examples {
                    let _ = writeln!(out, "  - `{}`", example.replace('`', "'"));
                }
            }
        }
        if !self.panics.is_empty() {
            let _ = writeln!(out, "\n## Panics\n");
            for panic in &self.panics {
                let _ = writeln!(out, "- {panic}");
            }
        }
        if !self.logs.is_empty() {
            let _ = writeln!(
                out,
                "\n## Warnings and errors\n\n| level | count | last message |\n| --- | ---: | --- |"
            );
            let mut logs: Vec<_> = self.logs.iter().collect();
            logs.sort_by_key(|entry| std::cmp::Reverse(entry.1.0));
            for ((level, _), (count, last)) in logs.into_iter().take(25) {
                let _ = writeln!(out, "| {level} | {count} | {} |", cell(last));
            }
        }

        let _ = writeln!(out, "\n## Event counts\n");
        for (event, count) in &self.events {
            let _ = writeln!(out, "- `{event}`: {count}");
        }
        out
    }

    fn hints(&self, dropped: u64) -> Vec<String> {
        let mut hints = Vec::new();
        if dropped > 0 {
            hints.push(format!(
                "{dropped} events were dropped: the writer fell behind (slow disk?)"
            ));
        }
        if let Some(st) = self.stat("runtime.lag", "maxMs")
            && st.p90 > 50.0
        {
            hints.push(format!(
                "the async runtime was blocked (timer late by up to {} in 90% of minutes): something runs blocking work on a runtime thread",
                fmt_ms(st.p90)
            ));
        }
        if let Some(st) = self.stat_prefix("interaction", "firstResponseMs")
            && st.p99 > 2_000.0
        {
            hints.push(format!(
                "the first answer to an interaction took {} at p99; Discord rejects answers after 3 s: defer earlier",
                fmt_ms(st.p99)
            ));
        }
        if let Some(st) = self.stat_prefix("interaction", "gatewayDelayMs")
            && st.p90 > 1_000.0
        {
            hints.push(format!(
                "interactions arrived {} after Discord created them (p90): gateway delay or a wrong server clock",
                fmt_ms(st.p90)
            ));
        }
        if let Some(st) = self.stat("discord.heartbeat", "rttMs")
            && st.p90 > 250.0
        {
            hints.push(format!(
                "gateway heartbeat round trip is {} at p90: network to Discord is slow",
                fmt_ms(st.p90)
            ));
        }
        if let Some(st) = self.stat_prefix("lavalink.track_start", "playToStartMs")
            && st.p90 > 3_000.0
        {
            hints.push(format!(
                "tracks start {} after the play request (p90): the source (YouTube) is slow to answer",
                fmt_ms(st.p90)
            ));
        }
        if let Some(st) = self.stat("lavalink.player", "pingMs")
            && st.p90 > 150.0
        {
            hints.push(format!(
                "Lavalink's voice ping to Discord is {} at p90: audio may lag",
                fmt_ms(st.p90)
            ));
        }
        // Frame loss is split by when it happened (lavalink.rs); older records have no `op`.
        let lost = |op: &str| self.stat(&format!("lavalink.frames_lost {op}"), "lostMs");
        let all_lost = self
            .events
            .get("lavalink.frames_lost")
            .copied()
            .unwrap_or(0);
        let mid = lost("mid_track");
        let change = lost("track_change");
        if let Some(st) = &mid {
            hints.push(format!(
                "audio stuttered in the middle of tracks in {} minute(s) ({} of audio missing, worst minute {}): CPU, network or the source on the Lavalink node",
                st.n,
                fmt_ms(st.sum),
                fmt_ms(st.max)
            ));
        }
        if let Some(st) = &change {
            hints.push(format!(
                "silence around track changes in {} minute(s) ({} in total, worst {}): time to load the next track",
                st.n,
                fmt_ms(st.sum),
                fmt_ms(st.max)
            ));
        }
        let classified = mid.as_ref().map_or(0, |s| s.n) + change.as_ref().map_or(0, |s| s.n);
        let unclassified = (all_lost as usize).saturating_sub(classified);
        if unclassified > 0 {
            hints.push(format!(
                "Lavalink reported missing audio frames in {unclassified} minute(s) recorded before frame loss was split into mid-track and track changes"
            ));
        }
        if let Some(st) = self.stat_prefix("lavalink.track_gap", "gapMs")
            && st.p90 > 2_000.0
        {
            hints.push(format!(
                "silence between tracks is {} at p90 (max {}): the next track is slow to load from the source",
                fmt_ms(st.p90),
                fmt_ms(st.max)
            ));
        }
        let proc = &self.process;
        if proc.steal_max >= 10.0 {
            hints.push(format!(
                "the VPS host took up to {:.0}% of CPU time (steal): the provider's machine is overloaded",
                proc.steal_max
            ));
        }
        if proc.host_cpu_n > 0.0 && proc.host_cpu_sum / proc.host_cpu_n >= 80.0 {
            hints.push(format!(
                "the server CPU was {:.0}% busy on average (all services): encodes and audio slow down",
                proc.host_cpu_sum / proc.host_cpu_n
            ));
        }
        if proc.iowait_max >= 20.0 {
            hints.push(format!(
                "the server waited on disk up to {:.0}% of CPU time (iowait)",
                proc.iowait_max
            ));
        }
        if proc.mem_available_min.is_some_and(|mb| mb < 300.0) {
            hints.push(format!(
                "server memory ran low: {:.0} MB available at the lowest",
                proc.mem_available_min.unwrap_or_default()
            ));
        }
        if let Some(st) = self.stat_prefix("telegram.update", "ageMs")
            && st.p90 > 3_000.0
        {
            hints.push(format!(
                "Telegram messages reach the bot {} after they were sent (p90): polling backlog or slow network",
                fmt_ms(st.p90)
            ));
        }
        if let Some(st) = self.stat_prefix("telegram.call", "queueWaitMs")
            && st.p90 > 2_000.0
        {
            hints.push(format!(
                "Telegram calls wait {} in the per-chat limiter (p90): pacing is the main delay",
                fmt_ms(st.p90)
            ));
        }
        let rate_limited: u64 = self
            .events
            .iter()
            .filter(|(e, _)| e.ends_with("rate_limited"))
            .map(|(_, n)| n)
            .sum();
        if rate_limited > 0 {
            hints.push(format!(
                "{rate_limited} rate limit hit(s): see the `*.rate_limited` rows"
            ));
        }
        let mut slow_hosts: Vec<(String, Stats)> = self
            .timings
            .iter()
            .filter(|((g, f), _)| g.starts_with("http ") && f == "ttfbMs")
            .filter_map(|((g, _), s)| s.stats().map(|st| (g.clone(), st)))
            .filter(|(_, st)| st.p90 > 1_000.0 && st.n >= 3)
            .collect();
        slow_hosts.sort_by(|a, b| b.1.p90.total_cmp(&a.1.p90));
        for (group, st) in slow_hosts.into_iter().take(3) {
            hints.push(format!(
                "`{}` answers slowly: first byte after {} at p90",
                group.trim_start_matches("http "),
                fmt_ms(st.p90)
            ));
        }
        if let (Some(first), Some(last)) = (self.process.rss_first, self.process.rss_last)
            && last > first * 1.5
            && last - first > 50.0 * 1024.0
        {
            hints.push(format!(
                "memory grew from {:.0} MB to {:.0} MB today: watch for a leak",
                first / 1024.0,
                last / 1024.0
            ));
        }
        if !self.anomalies.is_empty() {
            hints.push(format!(
                "{} anomaly kind(s) recorded: see Anomalies",
                self.anomalies.len()
            ));
        }
        if !self.panics.is_empty() {
            hints.push(format!("{} panic(s)", self.panics.len()));
        }
        // The three groups that cost the most time overall.
        let mut totals: BTreeMap<&str, f64> = BTreeMap::new();
        for ((group, field), samples) in &self.timings {
            if field == "totalMs" || field == "durationMs" {
                *totals.entry(group.as_str()).or_default() += samples.0.iter().sum::<f64>();
            }
        }
        let mut totals: Vec<_> = totals.into_iter().filter(|(_, t)| *t > 0.0).collect();
        totals.sort_by(|a, b| b.1.total_cmp(&a.1));
        if !totals.is_empty() {
            let top: Vec<String> = totals
                .iter()
                .take(3)
                .map(|(g, t)| format!("`{g}` {}", fmt_ms(*t)))
                .collect();
            hints.push(format!("most time overall: {}", top.join(", ")));
        }
        hints
    }
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

fn fmt_ms(value: f64) -> String {
    if value.abs() >= 60_000.0 {
        format!("{:.1} min", value / 60_000.0)
    } else if value.abs() >= 1_000.0 {
        format!("{:.2} s", value / 1_000.0)
    } else {
        format!("{value:.0} ms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(event: &str, payload: Value) -> Record {
        Record {
            at: Utc::now(),
            event: event.to_owned(),
            payload,
            flush: None,
        }
    }

    #[test]
    fn options_from_env() {
        let base = Path::new("/srv/bot");
        assert_eq!(Options::from_lookup(base, |_| None), None);
        let on = |name: &str| match name {
            "OPERATOR_DIAGNOSTICS" => Some("true".to_owned()),
            "OPERATOR_DIAGNOSTICS_DIR" => Some("diag".to_owned()),
            "OPERATOR_DIAGNOSTICS_RETENTION_DAYS" => Some("0".to_owned()),
            _ => None,
        };
        let options = Options::from_lookup(base, on).unwrap();
        assert_eq!(options.dir, PathBuf::from("/srv/bot/diag"));
        assert_eq!(options.retention_days, 1, "clamped to the minimum");
        assert_eq!(options.max_file_bytes, 256 * 1024 * 1024);
        assert_eq!(options.summary_every, Duration::from_secs(15 * 60));
    }

    #[test]
    fn redaction() {
        let r = Redactor::new(vec!["hunter2-lavalink".to_owned()]);
        let tg = format!("{}:{}", "123456789", "A".repeat(35));
        let input = format!(
            "GET /bot{tg}/getUpdates pw hunter2-lavalink https://discord.com/api/webhooks/123/abcDEF_-x?x=1 token=abc&b=2 Authorization: Bot xyz"
        );
        let out = r.text(&input);
        assert!(!out.contains(&tg));
        assert!(!out.contains("hunter2"));
        assert!(!out.contains("abcDEF"));
        assert!(!out.contains("token=abc"));
        assert!(!out.contains("Bot xyz"));
        assert!(out.contains("webhooks/123/[redacted]"));
        let discord = format!("{}.{}.{}", "M".repeat(24), "G".repeat(6), "x".repeat(38));
        assert_eq!(r.text(&discord), "[redacted]");

        let mut value = json!({ "token": "t", "nested": { "Password": "p", "fine": "ok" },
                                "list": ["hunter2-lavalink"] });
        r.value(&mut value);
        assert_eq!(value["token"], "[redacted]");
        assert_eq!(value["nested"]["Password"], "[redacted]");
        assert_eq!(value["nested"]["fine"], "ok");
        assert_eq!(value["list"][0], "[redacted]");
    }

    #[test]
    fn routes_and_snowflakes() {
        assert_eq!(
            route_template("/v4/sessions/abc/players/1008460463148900373?noReplace=true"),
            "/v4/sessions/abc/players/{id}"
        );
        assert_eq!(route_template("/loadtracks?identifier=x"), "/loadtracks");
        // Snowflake created "now" has an age close to zero.
        let now = Utc::now().timestamp_millis() as u64;
        let id = (now - DISCORD_EPOCH_MS) << 22;
        assert!(snowflake_age_ms(id).abs() < 1_000.0);
        assert!(date_skew_ms("Mon, 16 Nov 2026 03:19:22 GMT").is_some());
        assert!(date_skew_ms("garbage").is_none());
    }

    #[test]
    fn aggregates_any_ms_field_by_group() {
        let mut agg = Aggregates::default();
        for v in [10.0, 20.0, 30.0, 40.0, 1000.0] {
            agg.observe(&rec(
                "http",
                json!({ "op": "GET discord.com/x", "ttfbMs": v, "ok": v < 500.0, "status": 200 }),
            ));
        }
        agg.observe(&rec(
            "log",
            json!({ "level": "info", "message": "Upload 12 done", "durationMs": 5.0,
                    "createdAtMs": 1_763_000_000_000u64 }),
        ));
        agg.observe(&rec("log", json!({ "level": "warn", "message": "slow 1" })));
        agg.observe(&rec("log", json!({ "level": "warn", "message": "slow 2" })));
        let st = agg.stat("http GET discord.com/x", "ttfbMs").unwrap();
        assert_eq!((st.n, st.p50, st.max), (5, 30.0, 1000.0));
        assert!(agg.stat("log: Upload # done", "durationMs").is_some());
        assert!(
            agg.stat("log: Upload # done", "createdAtMs").is_none(),
            "timestamps are not durations"
        );
        assert_eq!(agg.outcomes["http GET discord.com/x"].failed, 1);
        assert_eq!(agg.logs[&("warn".to_owned(), "slow #".to_owned())].0, 2);
        let text = agg.render("test", "2026-11-17", Utc::now(), 0, false);
        assert!(text.contains("| http GET discord.com/x | ttfbMs | 5 |"));
        assert!(text.contains("## Warnings and errors"));
    }

    #[test]
    fn hints_name_the_slow_parts() {
        let mut agg = Aggregates::default();
        for _ in 0..5 {
            agg.observe(&rec(
                "interaction",
                json!({ "op": "/play", "firstResponseMs": 2600.0 }),
            ));
            agg.observe(&rec("runtime.lag", json!({ "maxMs": 120.0 })));
        }
        agg.observe(&rec("discord.rate_limited", json!({ "waitMs": 900.0 })));
        let hints = agg.hints(3).join("\n");
        assert!(hints.contains("3 events were dropped"));
        assert!(hints.contains("Discord rejects answers after 3 s"));
        assert!(hints.contains("runtime was blocked"));
        assert!(hints.contains("1 rate limit hit"));
    }

    #[test]
    fn frame_loss_hints_tell_stutter_from_track_changes() {
        let mut agg = Aggregates::default();
        agg.observe(&rec("lavalink.frames_lost", json!({ "nulled": 9 })));
        agg.observe(&rec(
            "lavalink.frames_lost",
            json!({ "op": "track_change", "nulled": 50, "lostMs": 1000 }),
        ));
        agg.observe(&rec(
            "lavalink.frames_lost",
            json!({ "op": "mid_track", "nulled": 5, "lostMs": 100 }),
        ));
        agg.observe(&rec(
            "lavalink.track_gap",
            json!({ "op": "finished", "gapMs": 2500.0 }),
        ));
        agg.observe(&rec(
            "process.sample",
            json!({ "hostCpuPct": 90.0, "hostStealPct": 12.0, "hostIowaitPct": 1.0,
                    "memAvailableMb": 250.0, "swapUsedMb": 10.0 }),
        ));
        let hints = agg.hints(0).join("\n");
        assert!(hints.contains("stuttered in the middle of tracks in 1 minute(s)"));
        assert!(hints.contains("silence around track changes in 1 minute(s)"));
        assert!(hints.contains("1 minute(s) recorded before frame loss was split"));
        assert!(hints.contains("silence between tracks is 2.50 s"));
        assert!(hints.contains("steal"));
        assert!(hints.contains("90% busy"));
        assert!(hints.contains("250 MB available"));
    }

    #[test]
    fn host_cpu_percentages() {
        let prev = host_cpu_counters("cpu  100 0 100 700 50 0 0 50\ncpu0 1 2 3").unwrap();
        let now = host_cpu_counters("cpu  200 0 200 1300 100 0 0 200\n").unwrap();
        let (busy, iowait, steal) = host_cpu_pct(&prev, &now).unwrap();
        // deltas: user 100, system 100, idle 600, iowait 50, steal 150 → total 1000
        assert!((busy - 35.0).abs() < 1e-9);
        assert!((iowait - 5.0).abs() < 1e-9);
        assert!((steal - 15.0).abs() < 1e-9);
        assert!(host_cpu_counters("intr 1 2").is_none());
        let mem = "MemTotal: 2048 kB\nMemAvailable:    1024 kB\nSwapTotal: 0 kB\n";
        assert_eq!(meminfo_mb(mem, "MemAvailable"), Some(1.0));
        assert_eq!(meminfo_mb(mem, "Mem"), None);
    }

    #[test]
    fn restart_continues_the_days_report() {
        let dir = std::env::temp_dir().join(format!(
            "operator-diag-replay-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        fs::create_dir_all(&dir).unwrap();
        let day = "2026-11-17";
        let lines = [
            r#"{"t":"2026-11-17T01:00:00.000Z","e":"diagnostics.started"}"#,
            r#"{"t":"2026-11-17T01:00:00.000Z","e":"process.sample","cpuUserMsTotal":0,"cpuSysMsTotal":0,"rssKb":1000}"#,
            r#"{"t":"2026-11-17T01:01:00.000Z","e":"process.sample","cpuUserMsTotal":600,"cpuSysMsTotal":0,"rssKb":1000}"#,
            r#"{"t":"2026-11-17T01:01:00.000Z","e":"interaction","op":"/play","totalMs":250}"#,
            "not json",
            r#"{"t":"2026-11-17T02:00:00.000Z","e":"diagnostics.started"}"#,
            r#"{"t":"2026-11-17T02:00:00.000Z","e":"process.sample","cpuUserMsTotal":10,"cpuSysMsTotal":0,"rssKb":900}"#,
        ];
        fs::write(dir.join(format!("events-{day}.txt")), lines.join("\n")).unwrap();
        let mut writer = Writer::new(
            Options {
                dir: dir.clone(),
                retention_days: 3650,
                max_file_bytes: 1024 * 1024,
                summary_every: Duration::from_secs(3600),
            },
            "test".to_owned(),
            Redactor::new(Vec::new()),
        );
        writer.rotate(day);
        let st = writer.agg.stat("interaction /play", "totalMs").unwrap();
        assert_eq!(st.n, 1);
        assert_eq!(writer.agg.process.restarts, 2);
        // 600 ms CPU over 60 s in the first process; the restart's lower counter is ignored.
        assert!((writer.agg.process.cpu_ms - 600.0).abs() < f64::EPSILON);
        assert!((writer.agg.process.wall_ms - 60_000.0).abs() < f64::EPSILON);
        assert_eq!(writer.agg.process.rss_first, Some(900.0));
        let _ = fs::remove_dir_all(&dir);
    }

    /// End to end: the real recorder writes redacted events and a summary to disk.
    #[test]
    fn recorder_writes_events_and_summary() {
        let dir = std::env::temp_dir().join(format!(
            "operator-diag-test-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        let secret = "s3cr3t-token-value-for-test".to_owned();
        init(
            Options {
                dir: dir.clone(),
                retention_days: 14,
                max_file_bytes: 10 * 1024 * 1024,
                summary_every: Duration::from_secs(3600),
            },
            "test",
            vec![secret.clone()],
        )
        .unwrap();
        // A second init is a no-op.
        init(
            Options::from_lookup(Path::new("/nonexistent"), |_| Some("1".to_owned())).unwrap(),
            "again",
            Vec::new(),
        )
        .ok();
        record(
            "http",
            json!({ "op": "GET example.org/a", "ttfbMs": 12.5, "ok": true,
                    "url": format!("https://x/{secret}"), "authorization": "Bot abc" }),
        );
        log_line(
            "warn",
            &format!("leak {secret}"),
            Some(&json!({ "waitMs": 3 })),
        );
        HttpTiming {
            client: "test",
            method: "GET",
            host: "example.org",
            path: "/b/123456789?q=1",
            status: Some(429),
            ttfb: Some(Duration::from_millis(5)),
            body: Some(Duration::from_millis(1)),
            total: Duration::from_millis(6),
            bytes: Some(10),
            date: Some("Mon, 01 Jan 2001 00:00:00 GMT"),
            error: None,
        }
        .record();
        flush();

        let day = Utc::now().format("%Y-%m-%d").to_string();
        let events = fs::read_to_string(dir.join(format!("events-{day}.txt"))).unwrap();
        let summary = fs::read_to_string(dir.join(format!("summary-{day}.md"))).unwrap();
        assert!(!events.contains(&secret), "secret leaked into events");
        assert!(!summary.contains(&secret), "secret leaked into the summary");
        assert!(!events.contains("Bot abc"));
        assert!(events.contains("\"e\":\"diagnostics.started\""));
        assert!(events.contains("GET example.org/b/{id}"));
        assert!(events.contains("\"kind\":\"clock_skew\""));
        for line in events.lines() {
            serde_json::from_str::<Value>(line).expect("every line is JSON");
        }
        assert!(summary.contains("| http GET example.org/a | ttfbMs | 1 |"));
        assert!(summary.contains("429x1"));
        assert!(summary.contains("clock_skew"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join(format!("events-{day}.txt")))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "events file must be private");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    fn rand_suffix() -> u64 {
        Utc::now().timestamp_nanos_opt().unwrap_or_default() as u64 % 1_000_000
    }
}
