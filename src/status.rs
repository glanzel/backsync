use crate::config::{self, Config, ResticJob, SyncJob};
use crate::exec;
use crate::runner;
use serde::Serialize;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Serialize, Default, Clone)]
pub struct UnitInfo {
    pub state: String,
    pub result: String,
    pub last: String,
    pub next: String,
    pub tail: String,
    pub active: bool,
    pub failed: bool,
}

#[derive(Serialize, Default)]
pub struct SnapshotInfo {
    pub time: String,
    pub id: String,
    pub size: String,
    pub files: u64,
}

#[derive(Serialize, Default)]
pub struct ResticStatus {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub repository: String,
    pub paths: Vec<String>,
    pub snapshot: Option<SnapshotInfo>,
    pub snapshots: u64,
    pub history: Vec<HistoryEntry>,
    pub progress: String,
    pub error: String,
    #[serde(flatten)]
    pub unit: UnitInfo,
}

#[derive(Serialize, Default, Clone)]
pub struct HistoryEntry {
    pub kind: String,
    pub name: String,
    pub detail: String,
    pub when: String,
    #[serde(default)]
    pub ts: String,
    pub status: String,
}

#[derive(Serialize, Default)]
pub struct SyncStatus {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub source: String,
    pub target: String,
    pub mode: String,
    pub progress: String,
    pub error: String,
    #[serde(flatten)]
    pub unit: UnitInfo,
}

#[derive(Serialize, Default)]
pub struct CloudInfo {
    pub remote: String,
    pub used: String,
    pub free: String,
    pub total: String,
    pub percent: f64,
    pub ok: bool,
}

#[derive(Serialize, Default)]
pub struct Status {
    pub restic: Vec<ResticStatus>,
    pub sync: Vec<SyncStatus>,
    pub watchdog: UnitInfo,
    pub clouds: Vec<CloudInfo>,
    pub alarm: String,
    pub now: String,
    pub partial: bool,
    pub config_path: String,
    pub binary: String,
    pub restic_jobs: Vec<serde_json::Value>,
    pub sync_jobs: Vec<serde_json::Value>,
    pub jobs_total: u64,
    pub jobs_ok: u64,
    pub jobs_failed: u64,
    pub jobs_paused: u64,
    pub last_success: String,
    pub history: Vec<HistoryEntry>,
    pub checks: Vec<HistoryEntry>,
}

struct CachedJson {
    at: Instant,
    value: Option<serde_json::Value>,
}

static CACHE: OnceLock<Mutex<std::collections::HashMap<String, CachedJson>>> = OnceLock::new();

const TTL: Duration = Duration::from_secs(300);

fn cache() -> &'static Mutex<std::collections::HashMap<String, CachedJson>> {
    CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn cached(
    key: &str,
    force: bool,
    f: impl FnOnce() -> Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    let mut guard = cache().lock().unwrap_or_else(|e| e.into_inner());
    if !force {
        if let Some(c) = guard.get(key) {
            let effective = if c.value.is_none() {
                TTL.min(Duration::from_secs(20))
            } else {
                TTL
            };
            if c.at.elapsed() < effective {
                return c.value.clone();
            }
        }
    }
    let value = f();
    guard.insert(
        key.to_string(),
        CachedJson {
            at: Instant::now(),
            value: value.clone(),
        },
    );
    value
}

fn peek(key: &str) -> Option<serde_json::Value> {
    cache().lock().ok()?.get(key).and_then(|c| c.value.clone())
}

fn prop(unit: &str, name: &str) -> String {
    exec::run(
        "systemctl",
        &["--user", "show", unit, &format!("-p{}", name), "--value"],
        &[],
        Duration::from_secs(5),
    )
    .stdout
    .trim()
    .to_string()
}

fn next_run(timer: &str) -> String {
    let out = exec::run(
        "systemctl",
        &["--user", "list-timers", timer, "--no-pager"],
        &[],
        Duration::from_secs(5),
    );
    let line = out.stdout.lines().nth(1).unwrap_or("").trim();
    if line.is_empty() || line.starts_with('-') {
        return "–".into();
    }
    line.split_whitespace()
        .take(3)
        .collect::<Vec<_>>()
        .join(" ")
}

fn unit_info(unit: &str, timer: &str, log: &str, lines: usize) -> UnitInfo {
    let state = prop(unit, "ActiveState");
    let result = prop(unit, "Result");
    let tail = exec::tail(std::path::Path::new(log), lines);
    UnitInfo {
        active: state == "active" || state == "activating",
        failed: !result.is_empty()
            && result != "success"
            && !(state == "active" || state == "activating"),
        state,
        result,
        last: exec::human(&prop(unit, "ExecMainExitTimestamp")),
        next: next_run(timer),
        tail,
    }
}

/// Erwartet die nach Zeit aufsteigend sortierte Snapshot-Liste.
fn snapshot_of(v: serde_json::Value) -> Option<SnapshotInfo> {
    let first = v.as_array()?.last()?;
    let time = first["time"].as_str().unwrap_or("");
    let bytes = first["summary"]["total_bytes_processed"]
        .as_f64()
        .unwrap_or(0.0);
    let files = first["summary"]["total_files_processed"]
        .as_u64()
        .unwrap_or(0);
    Some(SnapshotInfo {
        time: exec::human(time.replace('T', " ").trim_end_matches('Z')),
        id: first["short_id"].as_str().unwrap_or("").to_string(),
        size: exec::fmt_bytes(bytes),
        files,
    })
}

fn remote_of(target: &str) -> Option<String> {
    let t = target.trim();
    if t.starts_with("rclone:") {
        return Some(t.to_string());
    }
    let remote = t.split(':').next().unwrap_or("");
    if !remote.is_empty() && !t.starts_with('/') && !remote.contains('/') && t.contains(':') {
        return Some(format!("{}:", remote));
    }
    None
}

pub fn collect(force: bool, network: bool) -> Status {
    let cfg = config::load();
    let mut s = Status {
        partial: !network,
        now: exec::run("date", &["+%d.%m.%Y %H:%M:%S"], &[], Duration::from_secs(5))
            .stdout
            .trim()
            .to_string(),
        config_path: config::config_path().to_string_lossy().into_owned(),
        binary: runner::restic_bin(&cfg),
        ..Default::default()
    };

    for job in &cfg.restic_jobs {
        s.restic.push(restic_status(&cfg, job, force, network));
    }
    for job in &cfg.sync_jobs {
        s.sync.push(sync_status(&cfg, job));
    }
    s.watchdog = unit_info(
        &config::watchdog_unit(&cfg),
        &config::watchdog_timer(&cfg),
        &config::log_path(&cfg, "watchdog", "main").to_string_lossy(),
        3,
    );

    let mut remotes: Vec<String> = Vec::new();
    for job in &cfg.sync_jobs {
        if let Some(r) = remote_of(&job.target) {
            if !remotes.contains(&r) {
                remotes.push(r);
            }
        }
    }
    for job in &cfg.restic_jobs {
        if let Some(r) = remote_of(&job.repository) {
            let r = r.replace("rclone:", "");
            let r = r.split('/').next().unwrap_or("").to_string();
            let r = if r.contains(':') {
                format!("{}:", r.split(':').next().unwrap_or(""))
            } else {
                r
            };
            if !r.is_empty() && !remotes.contains(&r) {
                remotes.push(r);
            }
        }
    }
    for remote in remotes {
        s.clouds.push(cloud_info(&remote, force, network));
    }

    s.alarm = std::fs::read_to_string(config::alarm_path(&cfg))
        .unwrap_or_default()
        .trim()
        .to_string();

    for r in &s.restic {
        s.jobs_total += 1;
        if !r.enabled {
            s.jobs_paused += 1;
        } else if r.unit.failed {
            s.jobs_failed += 1;
        } else if r.unit.result == "success" {
            s.jobs_ok += 1;
        }
        s.history.extend(r.history.clone());
    }
    for y in &s.sync {
        s.jobs_total += 1;
        if !y.enabled {
            s.jobs_paused += 1;
        } else if y.unit.failed {
            s.jobs_failed += 1;
        } else if y.unit.result == "success" {
            s.jobs_ok += 1;
        }
    }
    s.history.sort_by(|a, b| b.ts.cmp(&a.ts));
    s.history.truncate(14);

    s.checks = s
        .history
        .iter()
        .filter(|h| h.kind != "Backup")
        .cloned()
        .collect();

    s.last_success = s
        .restic
        .iter()
        .filter_map(|r| r.snapshot.as_ref().map(|x| x.time.clone()))
        .max()
        .or_else(|| {
            s.history
                .iter()
                .find(|h| h.kind == "Backup" && h.status == "success")
                .map(|h| h.when.clone())
        })
        .unwrap_or_default();

    s.restic_jobs = serde_json::to_value(&cfg.restic_jobs)
        .unwrap_or_default()
        .as_array()
        .cloned()
        .unwrap_or_default();
    s.sync_jobs = serde_json::to_value(&cfg.sync_jobs)
        .unwrap_or_default()
        .as_array()
        .cloned()
        .unwrap_or_default();
    s
}

fn restic_status(cfg: &Config, job: &ResticJob, force: bool, network: bool) -> ResticStatus {
    let key = format!("restic:{}", job.id);
    let data = if network {
        cached(&key, force, || {
            // Ein Abruf liefert Anzahl und neuesten Snapshot zugleich.
            runner::restic_snapshots(cfg, job, None).ok().map(|v| {
                let mut arr: Vec<serde_json::Value> = v.as_array().cloned().unwrap_or_default();
                arr.sort_by_key(|x| x["time"].as_str().unwrap_or("").to_string());
                serde_json::Value::Array(arr)
            })
        })
    } else {
        peek(&key)
    };

    let mut error = String::new();
    let snapshot = match &data {
        Some(v) => snapshot_of(v.clone()),
        None => None,
    };
    if network && data.is_none() {
        error = "Repository nicht erreichbar oder Passwort falsch".into();
    }

    let log_path = config::log_path(cfg, "restic", &job.id);
    let unit = unit_info(
        &config::restic_unit(job),
        &config::restic_timer(job),
        &log_path.to_string_lossy(),
        5,
    );
    let log = merged_log(&log_path, config::legacy_log("restic"));
    let progress = if unit.active {
        live_progress(&log)
    } else {
        String::new()
    };
    let mut history = history_from_log(&log, &job.name, "Backup");
    for (kind, label) in [("check", "Prüfung"), ("prune", "Aufräumen")] {
        let p = config::log_path(cfg, kind, &job.id);
        if let Ok(text) = std::fs::read_to_string(&p) {
            let mut extra = history_from_log(&text, &job.name, label);
            extra.truncate(2);
            history.extend(extra);
        }
    }
    history.sort_by(|a, b| b.ts.cmp(&a.ts));
    history.truncate(10);

    let snapshots = data
        .as_ref()
        .and_then(|v| v.as_array().map(|a| a.len() as u64))
        .unwrap_or(0);

    ResticStatus {
        id: job.id.clone(),
        name: job.name.clone(),
        enabled: job.enabled,
        repository: job.repository.clone(),
        paths: job.paths.clone(),
        snapshot,
        snapshots,
        history,
        progress,
        error,
        unit,
    }
}

/// Letzte Fortschrittszeile eines laufenden restic-Laufs.
/// Liest die eigene Logdatei und, falls vorhanden, die der Altinstallation.
fn merged_log(own: &std::path::Path, legacy: Option<std::path::PathBuf>) -> String {
    let mut text = String::new();
    if let Some(l) = legacy {
        text.push_str(&std::fs::read_to_string(l).unwrap_or_default());
        if !text.ends_with('\n') {
            text.push('\n');
        }
    }
    text.push_str(&std::fs::read_to_string(own).unwrap_or_default());
    text
}

fn live_progress(log: &str) -> String {
    log.lines()
        .rev()
        .take(200)
        .find(|l| {
            let l = l.trim();
            l.contains("Files:")
                || l.contains("processed")
                || (l.contains('%') && (l.contains("GiB") || l.contains("MiB")))
        })
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Baut aus Logzeilen eine kurze Ereignisliste (Backups, Restores, Prüfungen).
fn history_from_log(log: &str, name: &str, kind: &str) -> Vec<HistoryEntry> {
    let mut out = Vec::new();
    for line in log.lines().rev() {
        let t = line.trim();
        if out.len() >= 8 {
            break;
        }
        if t.contains("snapshot ") && t.contains(" saved") {
            let id = t
                .split("snapshot ")
                .nth(1)
                .and_then(|x| x.split_whitespace().next())
                .unwrap_or("");
            out.push(HistoryEntry {
                kind: kind.into(),
                name: name.into(),
                detail: if id.is_empty() {
                    String::new()
                } else {
                    id.to_string()
                },
                when: String::new(),
                ts: String::new(),
                status: "success".into(),
            });
        } else if t.starts_with("=== ")
            && (t.contains("start:") || t.contains("Start:") || t.contains(" Lauf"))
        {
            // Zeitstempel für den zuletzt gefundenen Eintrag nachreichen
            if let Some(last) = out.last_mut() {
                if last.when.is_empty() {
                    let ts = t
                        .trim_start_matches("=== ")
                        .split_whitespace()
                        .next()
                        .unwrap_or("");
                    last.ts = ts.to_string();
                    last.when = exec::human(ts);
                }
            }
        } else if kind != "Backup" && t.starts_with("=== ") && t.contains("ende: Code 0") {
            let ts = t
                .trim_start_matches("=== ")
                .split_whitespace()
                .next()
                .unwrap_or("");
            out.push(HistoryEntry {
                kind: kind.into(),
                name: name.into(),
                detail: String::new(),
                when: exec::human(ts),
                ts: ts.to_string(),
                status: "success".into(),
            });
        } else if t.contains("Fatal:") || t.contains("FEHLER") {
            out.push(HistoryEntry {
                kind: kind.into(),
                name: name.into(),
                detail: t.chars().take(120).collect(),
                when: String::new(),
                ts: String::new(),
                status: "failed".into(),
            });
        }
    }
    out
}

fn sync_status(cfg: &Config, job: &SyncJob) -> SyncStatus {
    let log_path = config::log_path(cfg, "sync", &job.id);
    let mut unit = unit_info(
        &config::sync_unit(job),
        &config::sync_timer(job),
        &log_path.to_string_lossy(),
        5,
    );
    let log = merged_log(&log_path, config::legacy_log("sync"));
    let progress = if unit.active {
        live_progress(&log)
    } else {
        String::new()
    };
    if let Some(line) = log
        .lines()
        .rev()
        .find(|l| l.contains("Sync fertig") || l.contains("Transferred:"))
    {
        unit.tail = line.trim().to_string();
    }

    SyncStatus {
        id: job.id.clone(),
        name: job.name.clone(),
        enabled: job.enabled,
        source: job.source.clone(),
        target: job.target.clone(),
        mode: job.mode.clone(),
        progress,
        error: String::new(),
        unit,
    }
}

fn cloud_info(remote: &str, force: bool, network: bool) -> CloudInfo {
    let key = format!("cloud:{}", remote);
    let data = if network {
        cached(&key, force, || {
            let out = exec::run(
                "rclone",
                &["about", remote, "--json"],
                &[],
                Duration::from_secs(25),
            );
            serde_json::from_str::<serde_json::Value>(out.stdout.trim()).ok()
        })
    } else {
        peek(&key)
    };

    let mut info = CloudInfo {
        remote: remote.to_string(),
        ..Default::default()
    };
    if let Some(v) = data {
        let used = v["used"].as_f64().unwrap_or(0.0);
        let total = v["total"].as_f64().unwrap_or(0.0);
        let free = v["free"].as_f64().unwrap_or(0.0);
        info.used = exec::fmt_bytes(used);
        info.free = exec::fmt_bytes(free);
        info.total = exec::fmt_bytes(total);
        info.percent = if total > 0.0 {
            used / total * 100.0
        } else {
            0.0
        };
        info.ok = true;
    }
    info
}

pub fn collect_status(force: bool) -> Status {
    collect(force, true)
}

pub fn collect_fast() -> Status {
    collect(false, false)
}
