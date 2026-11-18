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
    pub error: String,
    #[serde(flatten)]
    pub unit: UnitInfo,
}

#[derive(Serialize, Default)]
pub struct SyncStatus {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub source: String,
    pub target: String,
    pub mode: String,
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

fn snapshot_of(v: serde_json::Value) -> Option<SnapshotInfo> {
    let first = v.as_array()?.first()?;
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
            runner::restic_snapshots(cfg, job, Some(1))
                .ok()
                .or_else(|| {
                    runner::restic_snapshots(cfg, job, None).ok().map(|v| {
                        let mut arr: Vec<serde_json::Value> =
                            v.as_array().cloned().unwrap_or_default();
                        arr.sort_by_key(|x| x["time"].as_str().unwrap_or("").to_string());
                        arr.pop().map(|last| vec![last]).unwrap_or_default().into()
                    })
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

    let unit = unit_info(
        &config::restic_unit(job),
        &config::restic_timer(job),
        &config::log_path(cfg, "restic", &job.id).to_string_lossy(),
        5,
    );

    ResticStatus {
        id: job.id.clone(),
        name: job.name.clone(),
        enabled: job.enabled,
        repository: job.repository.clone(),
        paths: job.paths.clone(),
        snapshot,
        error,
        unit,
    }
}

fn sync_status(cfg: &Config, job: &SyncJob) -> SyncStatus {
    let log_path = config::log_path(cfg, "sync", &job.id);
    let mut unit = unit_info(
        &config::sync_unit(job),
        &config::sync_timer(job),
        &log_path.to_string_lossy(),
        5,
    );
    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    if let Some(line) = log
        .lines()
        .rev()
        .find(|l| l.contains("Transferred:") || l.contains("Sync fertig"))
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
