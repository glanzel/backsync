#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::Serialize;
use std::env;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const ALARM_FILE: &str = "/home/glanzel/.local/state/status/ALARM.txt";
const RESTIC_LOG: &str = "/home/glanzel/.local/state/restic/backup.log";
const SYNC_LOG: &str = "/home/glanzel/.local/state/media-sync/sync.log";
const WATCHDOG_LOG: &str = "/home/glanzel/.local/state/watchdog/watchdog.log";

const PATH_ENV: &str = "/home/glanzel/.local/bin:/usr/local/bin:/usr/bin:/bin";
const INSTANCE_LOCK: &str = "/home/glanzel/.local/state/status/.backup-status.lock";

const TTL_RESTIC: Duration = Duration::from_secs(300);
const TTL_CLOUD: Duration = Duration::from_secs(300);

#[derive(Serialize, Default)]
struct UnitInfo {
    state: String,
    result: String,
    last: String,
    next: String,
    tail: String,
}

#[derive(Serialize, Default)]
struct ResticInfo {
    #[serde(flatten)]
    unit: UnitInfo,
    snapshot: String,
    snapshot_id: String,
    snapshot_size: String,
}

#[derive(Serialize, Default)]
struct CloudInfo {
    used: String,
    free: String,
    total: String,
    percent: f64,
}

#[derive(Serialize, Default)]
struct Status {
    restic: ResticInfo,
    sync: UnitInfo,
    watchdog: UnitInfo,
    cloud: CloudInfo,
    alarm: String,
    now: String,
    #[serde(default)]
    partial: bool,
}

struct CachedJson {
    at: Instant,
    value: Option<serde_json::Value>,
}

static RESTIC_CACHE: OnceLock<Mutex<Option<CachedJson>>> = OnceLock::new();
static CLOUD_CACHE: OnceLock<Mutex<Option<CachedJson>>> = OnceLock::new();

/// Führt ein Kommando mit Timeout aus. Läuft nie länger als `timeout`,
/// damit die Oberfläche auch bei hängendem pCloud/rclone bedienbar bleibt.
fn sh(cmd: &str, args: &[&str], envs: &[(&str, &str)], timeout: Duration) -> String {
    let mut c = Command::new(cmd);
    c.args(args)
        .env("PATH", PATH_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // eigene Prozessgruppe, damit beim Timeout auch rclone-Kinder beendet werden
        .process_group(0);
    for (k, v) in envs {
        c.env(k, v);
    }

    let mut child = match c.spawn() {
        Ok(child) => child,
        Err(_) => return String::new(),
    };

    let pgid = child.id() as i32;

    let stdout = child.stdout.take();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut out) = stdout {
            let _ = out.read_to_string(&mut s);
        }
        let _ = tx.send(s);
    });

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_group(pgid);
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                thread::sleep(Duration::from_millis(60));
            }
            Err(_) => break,
        }
    }

    rx.recv_timeout(Duration::from_secs(3))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn kill_group(pgid: i32) {
    let _ = Command::new("kill")
        .args(["-TERM", &format!("-{}", pgid)])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn prop(unit: &str, name: &str) -> String {
    sh(
        "systemctl",
        &["--user", "show", unit, &format!("-p{}", name), "--value"],
        &[],
        Duration::from_secs(5),
    )
}

fn human(ts: &str) -> String {
    if ts.is_empty() || ts == "n/a" {
        return "–".into();
    }
    let out = sh(
        "date",
        &["-d", ts, "+%d.%m.%Y %H:%M"],
        &[],
        Duration::from_secs(5),
    );
    if out.is_empty() {
        ts.into()
    } else {
        out
    }
}

fn next_run(timer: &str) -> String {
    let out = sh(
        "systemctl",
        &["--user", "list-timers", timer, "--no-pager"],
        &[],
        Duration::from_secs(5),
    );
    let line = out.lines().nth(1).unwrap_or("").trim();
    if line.is_empty() || line.starts_with('-') {
        return "–".into();
    }
    line.split_whitespace()
        .take(3)
        .collect::<Vec<_>>()
        .join(" ")
}

fn tail(path: &str, n: usize) -> String {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    content
        .lines()
        .rev()
        .take(n)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

fn unit_info(unit: &str, timer: &str) -> UnitInfo {
    UnitInfo {
        state: prop(unit, "ActiveState"),
        result: prop(unit, "Result"),
        last: human(&prop(unit, "ExecMainExitTimestamp")),
        next: next_run(timer),
        tail: String::new(),
    }
}

fn fmt_bytes(b: f64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{:.1} {}", v, units[i])
}

/// Liefert das gecachte Ergebnis von `f`, wenn es jünger als `ttl` ist.
/// `force` erzwingt eine neue Abfrage (Knopf „Neu laden").
fn cached(
    slot: &'static OnceLock<Mutex<Option<CachedJson>>>,
    ttl: Duration,
    force: bool,
    f: impl FnOnce() -> Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    let mutex = slot.get_or_init(|| Mutex::new(None));
    let mut guard = mutex.lock().unwrap_or_else(|e| e.into_inner());

    if !force {
        if let Some(c) = guard.as_ref() {
            // Fehlschläge ("nicht erreichbar") nur kurz zwischenspeichern,
            // damit die Anzeige nach Netzrückkehr schnell wieder stimmt.
            let effective = if c.value.is_none() {
                ttl.min(Duration::from_secs(20))
            } else {
                ttl
            };
            if c.at.elapsed() < effective {
                return c.value.clone();
            }
        }
    }

    let value = f();
    *guard = Some(CachedJson {
        at: Instant::now(),
        value: value.clone(),
    });
    value
}

/// Nur ansehen, nie blockieren.
fn peek(slot: &'static OnceLock<Mutex<Option<CachedJson>>>) -> Option<serde_json::Value> {
    slot.get()?
        .lock()
        .ok()?
        .as_ref()
        .and_then(|c| c.value.clone())
}

fn restic_env() -> [(&'static str, &'static str); 3] {
    [
        ("RESTIC_REPOSITORY", "rclone:pcloud:backups/restic"),
        (
            "RESTIC_PASSWORD_FILE",
            "/home/glanzel/.config/restic/pcloud-repo.pass",
        ),
        ("RESTIC_CACHE_DIR", "/home/glanzel/.cache/restic"),
    ]
}

fn collect_status(force: bool) -> Status {
    collect(force, true)
}

/// Schneller Teil ohne Netzzugriff: systemd-Zustände, Logs, Alarm.
fn collect_fast() -> Status {
    collect(false, false)
}

fn collect(force: bool, network: bool) -> Status {
    let mut s = Status {
        partial: !network,
        ..Default::default()
    };

    let restic_data = if network {
        cached(&RESTIC_CACHE, TTL_RESTIC, force, || {
            let out = sh(
                "restic",
                &["snapshots", "--latest", "1", "--json"],
                &restic_env(),
                Duration::from_secs(120),
            );
            serde_json::from_str::<serde_json::Value>(&out).ok()
        })
    } else {
        peek(&RESTIC_CACHE)
    };

    if let Some(v) = restic_data {
        if let Some(first) = v.as_array().and_then(|a| a.first()) {
            let time = first["time"].as_str().unwrap_or("");
            s.restic.snapshot = human(time.replace('T', " ").trim_end_matches("Z"));
            s.restic.snapshot_id = first["short_id"].as_str().unwrap_or("").to_string();
            let bytes = first["summary"]["total_bytes_processed"]
                .as_f64()
                .unwrap_or(0.0);
            s.restic.snapshot_size = fmt_bytes(bytes);
        }
    }

    let mut r = unit_info("restic-backup.service", "restic-backup.timer");
    r.tail = tail(RESTIC_LOG, 4);
    s.restic.unit = r;

    let mut y = unit_info("media-sync.service", "media-sync.timer");
    y.tail = tail(SYNC_LOG, 4);
    // letzte Fortschrittszeile statt der letzten Logzeile
    let log = std::fs::read_to_string(SYNC_LOG).unwrap_or_default();
    if let Some(line) = log
        .lines()
        .rev()
        .find(|l| l.contains("Transferred:") || l.contains("Sync fertig"))
    {
        y.tail = line.trim().to_string();
    }
    s.sync = y;

    s.watchdog = unit_info("backup-watchdog.service", "backup-watchdog.timer");
    s.watchdog.tail = tail(WATCHDOG_LOG, 3);

    let cloud_data = if network {
        cached(&CLOUD_CACHE, TTL_CLOUD, force, || {
            let out = sh(
                "rclone",
                &["about", "pcloud:", "--json"],
                &[],
                Duration::from_secs(20),
            );
            serde_json::from_str::<serde_json::Value>(&out).ok()
        })
    } else {
        peek(&CLOUD_CACHE)
    };

    if let Some(v) = cloud_data {
        let used = v["used"].as_f64().unwrap_or(0.0);
        let total = v["total"].as_f64().unwrap_or(0.0);
        let free = v["free"].as_f64().unwrap_or(0.0);
        s.cloud = CloudInfo {
            used: fmt_bytes(used),
            free: fmt_bytes(free),
            total: fmt_bytes(total),
            percent: if total > 0.0 {
                used / total * 100.0
            } else {
                0.0
            },
        };
    }

    s.alarm = std::fs::read_to_string(ALARM_FILE)
        .unwrap_or_default()
        .trim()
        .to_string();
    s.now = sh("date", &["+%d.%m.%Y %H:%M:%S"], &[], Duration::from_secs(5));
    s
}

/// Statusabfrage läuft auf dem Blocking-Threadpool, nicht auf dem GTK-Hauptthread.
/// Sonst friert das Fenster ein, während restic/rclone mit pCloud sprechen.
#[tauri::command]
async fn status() -> Status {
    tauri::async_runtime::spawn_blocking(|| collect_status(false))
        .await
        .unwrap_or_default()
}

/// Wird vom Frontend aufgerufen, sobald der erste Inhalt gezeichnet ist.
/// Das Fenster startet unsichtbar, damit nie ein weißes Fenster aufblitzt.
#[tauri::command]
fn ready(window: tauri::WebviewWindow) {
    let _ = window.show();
}

/// Sofortige Anzeige ohne Netzzugriff (Fenster ist nach Millisekunden gefüllt).
#[tauri::command]
async fn status_fast() -> Status {
    tauri::async_runtime::spawn_blocking(collect_fast)
        .await
        .unwrap_or_default()
}

/// Wie `status`, aber ohne Cache (Knopf „Neu laden").
#[tauri::command]
async fn refresh() -> Status {
    tauri::async_runtime::spawn_blocking(|| collect_status(true))
        .await
        .unwrap_or_default()
}

fn start_unit(name: &str) -> Result<(), String> {
    let unit = match name {
        "restic" => "restic-backup.service",
        "sync" => "media-sync.service",
        "watchdog" => "backup-watchdog.service",
        _ => return Err(format!("unbekannte Aktion: {}", name)),
    };

    let mut child = Command::new("systemctl")
        .args(["--user", "start", "--no-block", unit])
        .env("PATH", PATH_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;

    thread::spawn(move || {
        let _ = child.wait();
    });

    Ok(())
}

#[tauri::command]
async fn action(name: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || start_unit(&name))
        .await
        .map_err(|e| e.to_string())?
}

/// Nur ein Statusfenster gleichzeitig. Ein zweiter Start holt das vorhandene
/// Fenster nach vorn, statt ein weiteres zu öffnen.
fn focus_existing() -> bool {
    Command::new("wmctrl")
        .args(["-a", "Backup-Status"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn single_instance() -> Option<std::fs::File> {
    use std::io::Write;

    let path = std::path::Path::new(INSTANCE_LOCK);
    if let Ok(pid) = std::fs::read_to_string(path) {
        if let Ok(pid) = pid.trim().parse::<u32>() {
            // Nur blockieren, wenn der PID wirklich noch unser Prozess ist.
            // So stört eine veraltete Lock-Datei (nach Absturz, PID-Wiederverwendung)
            // den Start nicht.
            let comm = std::fs::read_to_string(format!("/proc/{}/comm", pid))
                .unwrap_or_default()
                .trim()
                .to_string();
            if pid != std::process::id() && comm == "backup-status" {
                return None;
            }
        }
    }

    let mut file = std::fs::File::create(path).ok()?;
    let _ = write!(file, "{}", std::process::id());
    Some(file)
}

fn main() {
    // Kopfmodus für Tests: backup-status --print
    if env::args().any(|a| a == "--print") {
        println!(
            "{}",
            serde_json::to_string_pretty(&collect_status(false)).unwrap_or_default()
        );
        return;
    }

    let _instance = match single_instance() {
        Some(f) => f,
        None => {
            focus_existing();
            return;
        }
    };

    // WebKitGTK bleibt auf manchen NVIDIA-Setups (hier: GeForce MX150 + X11)
    // mit dem DMA-BUF-Renderer weiß. Software-Rendering ist für dieses
    // schlichte Fenster völlig ausreichend und immer korrekt.
    if env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    tauri::Builder::default()
        // Falls das Frontend nie "ready" meldet (z. B. WebKit-Fehler),
        // wird das Fenster trotzdem nach kurzer Zeit sichtbar.
        .setup(|app| {
            use tauri::Manager;
            let window = app.get_webview_window("main");
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(1500));
                if let Some(w) = window {
                    let _ = w.show();
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            status,
            status_fast,
            refresh,
            action,
            ready
        ])
        .run(tauri::generate_context!())
        .expect("Fehler beim Start der Tauri-App");
}
