use crate::config::{self, Config, ResticJob, SyncJob};
use crate::exec::{self, Output};
use std::path::Path;
use std::time::Duration;

pub fn restic_bin(cfg: &Config) -> String {
    if !cfg.settings.binary.is_empty() {
        return config::expand(&cfg.settings.binary);
    }
    if let Ok(p) = std::env::var("BACKUP_STATUS_RESTIC") {
        if !p.is_empty() {
            return p;
        }
    }
    if let Some(p) = exec::which(&["restic"]) {
        return p;
    }
    config::home()
        .join(".local/bin/restic")
        .to_string_lossy()
        .into_owned()
}

pub fn restic_env(_cfg: &Config, job: &ResticJob) -> Vec<(&'static str, String)> {
    let cache = if job.cache_dir.is_empty() {
        config::home()
            .join(".cache/restic")
            .to_string_lossy()
            .into_owned()
    } else {
        config::expand(&job.cache_dir)
    };
    vec![
        ("RESTIC_REPOSITORY", config::expand(&job.repository)),
        ("RESTIC_PASSWORD_FILE", config::expand(&job.password_file)),
        ("RESTIC_CACHE_DIR", cache),
    ]
}

fn parse_duration(s: &str, fallback: Duration) -> Duration {
    let s = s.trim();
    if s.is_empty() {
        return fallback;
    }
    let (num, unit) = s.split_at(s.trim_end_matches(|c: char| !c.is_ascii_digit()).len());
    let _ = unit;
    let n: u64 = match num.trim().parse() {
        Ok(n) => n,
        Err(_) => return fallback,
    };
    let mult = if s.ends_with("ms") {
        1
    } else if s.ends_with('s') {
        1000
    } else if s.ends_with('m') {
        60_000
    } else if s.ends_with('h') {
        3_600_000
    } else if s.ends_with('d') {
        86_400_000
    } else {
        1000
    };
    if s.ends_with("ms") {
        Duration::from_millis(n)
    } else {
        Duration::from_millis(n.saturating_mul(mult))
    }
}

pub fn restic_timeout(job: &ResticJob) -> Duration {
    parse_duration(&job.timeout, Duration::from_secs(12 * 3600))
}

pub fn sync_timeout(job: &SyncJob) -> Duration {
    parse_duration(&job.timeout, Duration::from_secs(24 * 3600))
}

fn opt(v: Option<u32>) -> Option<String> {
    v.map(|x| x.to_string())
}

pub fn restic_args_backup(job: &ResticJob) -> Vec<String> {
    let mut a = vec!["backup".to_string()];
    for p in &job.paths {
        a.push(config::expand(p));
    }
    if !job.exclude_file.is_empty() {
        a.push("--exclude-file".into());
        a.push(config::expand(&job.exclude_file));
    }
    for e in &job.exclude {
        a.push("--exclude".into());
        a.push(e.clone());
    }
    if job.exclude_caches {
        a.push("--exclude-caches".into());
    }
    if job.one_file_system {
        a.push("--one-file-system".into());
    }
    if !job.host.is_empty() {
        a.push("--host".into());
        a.push(job.host.clone());
    }
    for t in &job.tags {
        a.push("--tag".into());
        a.push(t.clone());
    }
    a.push("--verbose".into());
    a
}

pub fn restic_args_forget(job: &ResticJob) -> Vec<String> {
    let mut a = vec!["forget".to_string()];
    if !job.host.is_empty() {
        a.push("--host".into());
        a.push(job.host.clone());
    }
    for t in &job.tags {
        a.push("--tag".into());
        a.push(t.clone());
    }
    if let Some(v) = opt(job.keep_daily) {
        a.push("--keep-daily".into());
        a.push(v);
    }
    if let Some(v) = opt(job.keep_weekly) {
        a.push("--keep-weekly".into());
        a.push(v);
    }
    if let Some(v) = opt(job.keep_monthly) {
        a.push("--keep-monthly".into());
        a.push(v);
    }
    if let Some(v) = opt(job.keep_yearly) {
        a.push("--keep-yearly".into());
        a.push(v);
    }
    a
}

pub fn sync_args(job: &SyncJob, stamp: &str) -> Vec<String> {
    let mode = if job.mode == "copy" { "copy" } else { "sync" };
    let mut a = vec![
        mode.to_string(),
        config::expand(&job.source),
        config::expand(&job.target),
    ];
    if !job.backup_dir.is_empty() {
        let dir = job.backup_dir.replace("{timestamp}", stamp);
        let dir = if dir.starts_with('/') || dir.contains(':') {
            dir
        } else {
            format!("{}/{}", job.target.trim_end_matches('/'), dir)
        };
        a.push("--backup-dir".into());
        a.push(dir);
    }
    a.push("--size-only".into());
    for e in &job.exclude {
        a.push("--exclude".into());
        a.push(e.clone());
    }
    if let Some(t) = job.transfers {
        a.push("--transfers".into());
        a.push(t.to_string());
    }
    if let Some(c) = job.checkers {
        a.push("--checkers".into());
        a.push(c.to_string());
    }
    a.push("--retries".into());
    a.push("3".into());
    a.push("--low-level-retries".into());
    a.push("10".into());
    a.push("--stats".into());
    a.push("2m".into());
    a.push("--stats-one-line".into());
    a.push("-v".into());
    for f in &job.extra_flags {
        a.push(f.clone());
    }
    a
}

pub fn network_ok(timeout: Duration) -> bool {
    crate::exec::run(
        "rclone",
        &[
            "about",
            "pcloud:",
            "--timeout",
            "20s",
            "--contimeout",
            "10s",
        ],
        &[],
        timeout,
    )
    .ok()
}

pub fn run_restic_job(cfg: &Config, job: &ResticJob) -> Result<i32, String> {
    let bin = restic_bin(cfg);
    let env = restic_env(cfg, job);
    let log = config::log_path(cfg, "restic", &job.id);
    let env_refs: Vec<(&str, String)> = env.clone();
    let args = restic_args_backup(job);
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let code = exec::run_logged(&bin, &args_ref, &env_refs, restic_timeout(job), &log)?;
    if code != 0 {
        return Ok(code);
    }
    let has_policy = job.keep_daily.is_some()
        || job.keep_weekly.is_some()
        || job.keep_monthly.is_some()
        || job.keep_yearly.is_some();
    if !has_policy {
        return Ok(0);
    }
    let forget = restic_args_forget(job);
    let forget_ref: Vec<&str> = forget.iter().map(|s| s.as_str()).collect();
    let code = exec::run_logged(
        &bin,
        &forget_ref,
        &env_refs,
        Duration::from_secs(3600),
        &log,
    )?;
    Ok(code)
}

pub fn run_sync_job(cfg: &Config, job: &SyncJob) -> Result<i32, String> {
    let log = config::log_path(cfg, "sync", &job.id);
    let stamp = exec::now_iso()
        .replace([':', '-'], "")
        .chars()
        .take(15)
        .collect::<String>();
    let args = sync_args(job, &stamp);
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    exec::run_logged("rclone", &args_ref, &[], sync_timeout(job), &log)
}

pub fn run_watchdog(cfg: &Config) -> Result<i32, String> {
    let log = config::log_path(cfg, "watchdog", "main");
    let alarm = config::alarm_path(cfg);
    let mut problems: Vec<String> = Vec::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    for job in cfg.restic_jobs.iter().filter(|j| j.enabled) {
        let unit = config::restic_unit(job);
        let state = prop(&unit, "ActiveState");
        let result = prop(&unit, "Result");
        if state == "active" || state == "activating" {
            continue;
        }
        if !result.is_empty() && result != "success" {
            problems.push(format!(
                "{}: letzter Lauf fehlgeschlagen (Result: {})",
                job.name, result
            ));
            continue;
        }
        if let Some(age) = age_hours(&prop(&unit, "ExecMainExitTimestamp"), now) {
            if age >= cfg.settings.max_restic_h {
                problems.push(format!(
                    "{}: letzter Erfolg vor {} h (Grenze {} h)",
                    job.name, age, cfg.settings.max_restic_h
                ));
            }
        }
    }

    for job in cfg.sync_jobs.iter().filter(|j| j.enabled) {
        let unit = config::sync_unit(job);
        let state = prop(&unit, "ActiveState");
        let result = prop(&unit, "Result");
        if state == "active" || state == "activating" {
            continue;
        }
        if !result.is_empty() && result != "success" {
            problems.push(format!(
                "{}: letzter Lauf fehlgeschlagen (Result: {})",
                job.name, result
            ));
            continue;
        }
        if let Some(age) = age_hours(&prop(&unit, "ExecMainExitTimestamp"), now) {
            if age >= cfg.settings.max_sync_h {
                problems.push(format!(
                    "{}: letzter Erfolg vor {} h (Grenze {} h)",
                    job.name, age, cfg.settings.max_sync_h
                ));
            }
        }
    }

    let needs_net = !cfg.restic_jobs.is_empty() || !cfg.sync_jobs.is_empty();
    if needs_net && !network_ok(Duration::from_secs(60)) {
        problems.push("pCloud/Remote nicht erreichbar".into());
    }

    let text = problems.join("\n");
    let _ = std::fs::create_dir_all(config::state_dir(cfg));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .map_err(|e| format!("{}: {}", log.display(), e))?;
    use std::io::Write;
    let _ = writeln!(file, "=== {}", exec::now_iso());

    if problems.is_empty() {
        let _ = std::fs::remove_file(&alarm);
        let _ = writeln!(file, "OK: alles aktuell");
        return Ok(0);
    }
    let _ = writeln!(file, "PROBLEM:\n{}", text);
    let msg = format!(
        "Backup-Problem am {}:\n\n{}",
        exec::human(&exec::now_iso()),
        text
    );
    let _ = std::fs::write(&alarm, &msg);
    let _ = exec::run(
        "notify-send",
        &[
            "-u",
            "critical",
            "-a",
            "Backup",
            "-i",
            "dialog-error",
            "Backup-Problem",
            &msg,
        ],
        &[],
        Duration::from_secs(10),
    );
    Ok(1)
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

fn age_hours(ts: &str, now: u64) -> Option<u64> {
    let ts = ts.trim();
    if ts.is_empty() || ts == "n/a" {
        return None;
    }
    let out = exec::run("date", &["-d", ts, "+%s"], &[], Duration::from_secs(5));
    let secs: u64 = out.stdout.trim().parse().ok()?;
    Some(now.saturating_sub(secs) / 3600)
}

pub fn run_job_by_id(cfg: &Config, kind: &str, id: &str) -> Result<i32, String> {
    match kind {
        "restic" => {
            let job = config::find_restic(cfg, id)
                .ok_or_else(|| format!("Restic-Job {} nicht gefunden", id))?;
            run_restic_job(cfg, job)
        }
        "sync" => {
            let job = config::find_sync(cfg, id)
                .ok_or_else(|| format!("Sync-Job {} nicht gefunden", id))?;
            run_sync_job(cfg, job)
        }
        "watchdog" => run_watchdog(cfg),
        _ => Err(format!("unbekannte Art: {}", kind)),
    }
}

pub fn restic_snapshots(
    cfg: &Config,
    job: &ResticJob,
    limit: Option<u32>,
) -> Result<serde_json::Value, String> {
    let bin = restic_bin(cfg);
    let env = restic_env(cfg, job);
    let env_ref: Vec<(&str, String)> = env;
    let mut args = vec!["snapshots".to_string(), "--json".to_string()];
    if let Some(n) = limit {
        args.push("--latest".into());
        args.push(n.to_string());
    }
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let out = exec::run(&bin, &args_ref, &env_ref, Duration::from_secs(120));
    if !out.ok() {
        return Err(out.stderr.trim().to_string());
    }
    serde_json::from_str(&out.stdout).map_err(|e| format!("restic-Ausgabe: {}", e))
}

pub fn restic_ls(
    cfg: &Config,
    job: &ResticJob,
    snapshot: &str,
    dir: &str,
    recursive: bool,
) -> Result<Vec<serde_json::Value>, String> {
    let bin = restic_bin(cfg);
    let env = restic_env(cfg, job);
    let mut args = vec!["ls".to_string(), "--json".to_string(), "-l".to_string()];
    if recursive {
        args.push("--recursive".into());
    }
    args.push(snapshot.to_string());
    if !dir.is_empty() {
        args.push(dir.to_string());
    }
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let out = exec::run(&bin, &args_ref, &env, Duration::from_secs(180));
    if !out.ok() {
        return Err(out.stderr.trim().to_string());
    }
    let mut nodes = Vec::new();
    for line in out.stdout.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if v.get("message_type").and_then(|x| x.as_str()) == Some("node") {
                nodes.push(v);
            }
        }
    }
    Ok(nodes)
}

/// `snapshot` leer lassen, um über alle Snapshots zu suchen.
pub fn restic_search(
    cfg: &Config,
    job: &ResticJob,
    snapshot: &str,
    pattern: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let bin = restic_bin(cfg);
    let env = restic_env(cfg, job);
    let mut args = vec!["find".to_string(), "--json".to_string(), "-l".to_string()];
    if !snapshot.trim().is_empty() {
        args.push("--snapshot".into());
        args.push(snapshot.to_string());
    }
    if !job.host.is_empty() && snapshot.trim().is_empty() {
        args.push("--host".into());
        args.push(job.host.clone());
    }
    args.push(pattern.to_string());
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let out = exec::run(&bin, &args_ref, &env, Duration::from_secs(180));
    if !out.ok() {
        return Err(out.stderr.trim().to_string());
    }
    Ok(serde_json::from_str(&out.stdout).unwrap_or_default())
}

/// Sucht eine Datei über alle Snapshots und liefert flache Treffer.
pub fn restic_history(
    cfg: &Config,
    job: &ResticJob,
    pattern: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let blocks = restic_search(cfg, job, "", pattern)?;
    let mut out = Vec::new();
    for block in blocks {
        let sid = block["snapshot"].as_str().unwrap_or("").to_string();
        let short: String = sid.chars().take(8).collect();
        for m in block["matches"].as_array().cloned().unwrap_or_default() {
            out.push(serde_json::json!({
                "snapshot": sid,
                "short_id": short,
                "path": m["path"],
                "name": m["path"].as_str().unwrap_or("").rsplit('/').next().unwrap_or(""),
                "type": m["type"],
                "size": m["size"],
                "mtime": m["mtime"],
            }));
        }
    }
    Ok(out)
}

/// Prüft das Repository (restic check, optional mit Datenprüfung).
pub fn restic_check(cfg: &Config, job: &ResticJob, read_data: bool) -> Result<i32, String> {
    let bin = restic_bin(cfg);
    let env = restic_env(cfg, job);
    let log = config::log_path(cfg, "check", &job.id);
    let mut args = vec!["check".to_string()];
    if read_data {
        args.push("--read-data-subset".into());
        args.push("5%".into());
    }
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    exec::run_logged(&bin, &args_ref, &env, Duration::from_secs(12 * 3600), &log)
}

/// Entfernt nicht mehr benötigte Daten (restic prune).
pub fn restic_prune(cfg: &Config, job: &ResticJob) -> Result<i32, String> {
    let bin = restic_bin(cfg);
    let env = restic_env(cfg, job);
    let log = config::log_path(cfg, "prune", &job.id);
    let args = ["prune".to_string()];
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    exec::run_logged(&bin, &args_ref, &env, Duration::from_secs(12 * 3600), &log)
}

/// Legt ein neues Repository an.
pub fn restic_init(cfg: &Config, job: &ResticJob) -> Result<i32, String> {
    let bin = restic_bin(cfg);
    let env = restic_env(cfg, job);
    let log = config::log_path(cfg, "init", &job.id);
    let args = ["init".to_string()];
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    exec::run_logged(&bin, &args_ref, &env, Duration::from_secs(600), &log)
}

pub fn restic_restore(
    cfg: &Config,
    job: &ResticJob,
    snapshot: &str,
    target: &str,
    include: Option<&str>,
    dry_run: bool,
) -> Result<i32, String> {
    let bin = restic_bin(cfg);
    let env = restic_env(cfg, job);
    let log = config::log_path(cfg, "restore", &job.id);
    let target = config::expand(target);
    let _ = std::fs::create_dir_all(&target);
    let mut args = vec![
        "restore".to_string(),
        snapshot.to_string(),
        "--target".to_string(),
        target,
    ];
    if let Some(i) = include {
        if !i.trim().is_empty() {
            args.push("--include".into());
            args.push(i.to_string());
        }
    }
    if dry_run {
        args.push("--dry-run".into());
        args.push("-v".into());
    }
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    exec::run_logged(&bin, &args_ref, &env, Duration::from_secs(6 * 3600), &log)
}

pub fn open_in_filemanager(path: &str) -> Result<(), String> {
    let p = config::expand(path);
    if !Path::new(&p).exists() {
        if let Some(parent) = Path::new(&p).parent() {
            if parent.exists() {
                return exec::run(
                    "xdg-open",
                    &[parent.to_string_lossy().as_ref()],
                    &[],
                    Duration::from_secs(10),
                )
                .ok()
                .then_some(())
                .ok_or_else(|| "xdg-open fehlgeschlagen".into());
            }
        }
        return Err(format!("Pfad existiert nicht: {}", p));
    }
    let out: Output = exec::run("xdg-open", &[&p], &[], Duration::from_secs(10));
    if out.ok() {
        Ok(())
    } else {
        Err(out.stderr)
    }
}
