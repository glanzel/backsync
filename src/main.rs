#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod exec;
mod runner;
mod status;
mod units;

use config::Config;
use serde::Serialize;
use std::env;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

#[derive(Serialize)]
struct RestoreResult {
    code: i32,
    target: String,
    log: String,
}

#[tauri::command]
async fn status() -> status::Status {
    tauri::async_runtime::spawn_blocking(|| status::collect_status(false))
        .await
        .unwrap_or_default()
}

#[tauri::command]
async fn status_fast() -> status::Status {
    tauri::async_runtime::spawn_blocking(status::collect_fast)
        .await
        .unwrap_or_default()
}

#[tauri::command]
async fn refresh() -> status::Status {
    tauri::async_runtime::spawn_blocking(|| status::collect_status(true))
        .await
        .unwrap_or_default()
}

#[tauri::command]
fn ready(window: tauri::WebviewWindow) {
    let _ = window.show();
}

#[tauri::command]
fn config_get() -> serde_json::Value {
    let cfg = config::load();
    serde_json::to_value(&cfg).unwrap_or_default()
}

#[tauri::command]
fn config_path() -> String {
    config::config_path().to_string_lossy().into_owned()
}

#[tauri::command]
async fn config_save(cfg: serde_json::Value) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let parsed: Config = serde_json::from_value(cfg).map_err(|e| e.to_string())?;
        config::save(&parsed)?;
        Ok(serde_json::to_value(config::load()).unwrap_or_default())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn apply_units() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let cfg = config::load();
        units::install(&cfg)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn remove_units() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        let cfg = config::load();
        units::uninstall(&cfg)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn disable_legacy_units() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let cfg = config::load();
        units::disable_legacy(&cfg)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn run_job(kind: String, id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let cfg = config::load();
        let unit = match kind.as_str() {
            "restic" => config::find_restic(&cfg, &id).map(config::restic_unit),
            "sync" => config::find_sync(&cfg, &id).map(config::sync_unit),
            "watchdog" => Some(config::watchdog_unit(&cfg)),
            _ => None,
        }
        .ok_or_else(|| format!("unbekannter Job: {} {}", kind, id))?;

        let installed = units::is_installed(&unit);
        if installed {
            let mut child = Command::new("systemctl")
                .args(["--user", "start", "--no-block", &unit])
                .env("PATH", exec::path_env())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| e.to_string())?;
            thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(());
        }

        // Nicht installiert: direkt im Hintergrund ausführen und loggen.
        thread::spawn(move || {
            let cfg = config::load();
            let _ = runner::run_job_by_id(&cfg, &kind, &id);
        });
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn pick_folder() -> Option<String> {
    tauri::async_runtime::spawn_blocking(|| {
        let out = Command::new("zenity")
            .args([
                "--file-selection",
                "--directory",
                "--title=Ordner auswählen",
            ])
            .stdin(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if p.is_empty() {
            None
        } else {
            Some(p)
        }
    })
    .await
    .ok()
    .flatten()
}

#[tauri::command]
async fn add_paths(job_id: String, paths: Vec<String>) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let cfg = config::add_path(&job_id, &paths)?;
        Ok(serde_json::to_value(&cfg).unwrap_or_default())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn remove_path(job_id: String, path: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let cfg = config::remove_path(&job_id, &path)?;
        Ok(serde_json::to_value(&cfg).unwrap_or_default())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn snapshots(job_id: String, limit: Option<u32>) -> Result<Vec<serde_json::Value>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let cfg = config::load();
        let job = config::find_restic(&cfg, &job_id)
            .ok_or_else(|| format!("Restic-Job {} nicht gefunden", job_id))?;
        let v = runner::restic_snapshots(&cfg, job, limit)?;
        let mut arr = v.as_array().cloned().unwrap_or_default();
        arr.sort_by_key(|x| x["time"].as_str().unwrap_or("").to_string());
        arr.reverse();
        Ok(arr)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn snapshot_files(
    job_id: String,
    snapshot: String,
    dir: String,
) -> Result<Vec<serde_json::Value>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let cfg = config::load();
        let job = config::find_restic(&cfg, &job_id)
            .ok_or_else(|| format!("Restic-Job {} nicht gefunden", job_id))?;
        runner::restic_ls(&cfg, job, &snapshot, &dir, false)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn search_files(
    job_id: String,
    snapshot: String,
    pattern: String,
) -> Result<Vec<serde_json::Value>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let cfg = config::load();
        let job = config::find_restic(&cfg, &job_id)
            .ok_or_else(|| format!("Restic-Job {} nicht gefunden", job_id))?;
        runner::restic_search(&cfg, job, &snapshot, &pattern)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn restore(
    job_id: String,
    snapshot: String,
    target: String,
    include: Option<String>,
    dry_run: bool,
) -> Result<RestoreResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let cfg = config::load();
        let job = config::find_restic(&cfg, &job_id)
            .ok_or_else(|| format!("Restic-Job {} nicht gefunden", job_id))?;
        let code =
            runner::restic_restore(&cfg, job, &snapshot, &target, include.as_deref(), dry_run)?;
        let log = exec::tail(&config::log_path(&cfg, "restore", &job.id), 12);
        Ok(RestoreResult {
            code,
            target: config::expand(&target),
            log,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn open_path(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || runner::open_in_filemanager(&path))
        .await
        .map_err(|e| e.to_string())?
}

fn focus_existing() -> bool {
    Command::new("wmctrl")
        .args(["-a", "Backup-Status"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn instance_lock_path() -> std::path::PathBuf {
    config::state_dir(&config::load()).join(".ui.lock")
}

fn single_instance() -> Option<std::fs::File> {
    use std::io::Write;
    let path = instance_lock_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(pid) = std::fs::read_to_string(&path) {
        if let Ok(pid) = pid.trim().parse::<u32>() {
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

fn print_json<T: Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn cli() -> Option<i32> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || args[0] == "ui" {
        return None;
    }
    match args[0].as_str() {
        "--print" | "status" => {
            let wait = args.iter().any(|a| a == "--full");
            let s = if wait {
                status::collect_status(true)
            } else {
                status::collect_fast()
            };
            print_json(&s);
            Some(0)
        }
        "list" => {
            let cfg = config::load();
            let mut lines = Vec::new();
            for j in &cfg.restic_jobs {
                lines.push(format!(
                    "restic\t{}\t{}\t{}\t{}",
                    j.id,
                    if j.enabled { "an" } else { "aus" },
                    j.name,
                    j.paths.join(", ")
                ));
            }
            for j in &cfg.sync_jobs {
                lines.push(format!(
                    "sync\t{}\t{}\t{}\t{} -> {}",
                    j.id,
                    if j.enabled { "an" } else { "aus" },
                    j.name,
                    j.source,
                    j.target
                ));
            }
            println!("{}", lines.join("\n"));
            Some(0)
        }
        "config-path" => {
            println!("{}", config::config_path().display());
            Some(0)
        }
        "run" => {
            let cfg = config::load();
            let kind = args.get(1).map(|s| s.as_str()).unwrap_or("");
            let id = args.get(2).map(|s| s.as_str()).unwrap_or("");
            match runner::run_job_by_id(&cfg, kind, id) {
                Ok(code) => Some(code),
                Err(e) => {
                    eprintln!("{}", e);
                    Some(2)
                }
            }
        }
        "watchdog" => {
            let cfg = config::load();
            match runner::run_watchdog(&cfg) {
                Ok(code) => Some(code),
                Err(e) => {
                    eprintln!("{}", e);
                    Some(2)
                }
            }
        }
        "apply" => {
            let cfg = config::load();
            match units::install(&cfg) {
                Ok(list) => {
                    println!("geschrieben: {}", list.join(", "));
                    Some(0)
                }
                Err(e) => {
                    eprintln!("{}", e);
                    Some(2)
                }
            }
        }
        "disable-legacy" => match units::disable_legacy(&config::load()) {
            Ok(list) => {
                println!("deaktiviert: {}", list.join(", "));
                Some(0)
            }
            Err(e) => {
                eprintln!("{}", e);
                Some(2)
            }
        },
        "add-path" => {
            // backup-status add-path <job> <pfad>...
            let job = args.get(1).cloned().unwrap_or_default();
            let paths = args[2..].to_vec();
            match config::add_path(&job, &paths) {
                Ok(_) => Some(0),
                Err(e) => {
                    eprintln!("{}", e);
                    Some(2)
                }
            }
        }
        "snapshots" => {
            let cfg = config::load();
            let job = args.get(1).cloned().unwrap_or_default();
            match config::find_restic(&cfg, &job) {
                Some(j) => match runner::restic_snapshots(&cfg, j, None) {
                    Ok(v) => {
                        print_json(&v);
                        Some(0)
                    }
                    Err(e) => {
                        eprintln!("{}", e);
                        Some(2)
                    }
                },
                None => {
                    eprintln!("Job {} nicht gefunden", job);
                    Some(2)
                }
            }
        }
        "ls" => {
            // backup-status ls <job> <snapshot> [pfad]
            let cfg = config::load();
            let job = args.get(1).cloned().unwrap_or_default();
            let snap = args.get(2).cloned().unwrap_or_else(|| "latest".into());
            let dir = args.get(3).cloned().unwrap_or_default();
            match config::find_restic(&cfg, &job) {
                Some(j) => match runner::restic_ls(&cfg, j, &snap, &dir, false) {
                    Ok(v) => {
                        print_json(&v);
                        Some(0)
                    }
                    Err(e) => {
                        eprintln!("{}", e);
                        Some(2)
                    }
                },
                None => {
                    eprintln!("Job {} nicht gefunden", job);
                    Some(2)
                }
            }
        }
        "search" => {
            // backup-status search <job> <snapshot> <muster>
            let cfg = config::load();
            let job = args.get(1).cloned().unwrap_or_default();
            let snap = args.get(2).cloned().unwrap_or_else(|| "latest".into());
            let pat = args.get(3).cloned().unwrap_or_default();
            match config::find_restic(&cfg, &job) {
                Some(j) => match runner::restic_search(&cfg, j, &snap, &pat) {
                    Ok(v) => {
                        print_json(&v);
                        Some(0)
                    }
                    Err(e) => {
                        eprintln!("{}", e);
                        Some(2)
                    }
                },
                None => {
                    eprintln!("Job {} nicht gefunden", job);
                    Some(2)
                }
            }
        }
        "restore" => {
            // backup-status restore <job> <snapshot> <ziel> [--include pfad] [--dry-run]
            let cfg = config::load();
            let job = args.get(1).cloned().unwrap_or_default();
            let snap = args.get(2).cloned().unwrap_or_default();
            let target = args.get(3).cloned().unwrap_or_default();
            let dry = args.iter().any(|a| a == "--dry-run");
            let include = args
                .iter()
                .position(|a| a == "--include")
                .and_then(|i| args.get(i + 1))
                .cloned();
            match config::find_restic(&cfg, &job) {
                Some(j) => {
                    match runner::restic_restore(&cfg, j, &snap, &target, include.as_deref(), dry) {
                        Ok(code) => Some(code),
                        Err(e) => {
                            eprintln!("{}", e);
                            Some(2)
                        }
                    }
                }
                None => {
                    eprintln!("Job {} nicht gefunden", job);
                    Some(2)
                }
            }
        }
        "-h" | "--help" | "help" => {
            println!(
                "backup-status – Backup- und Sync-Verwaltung\n\n\
                 Aufruf ohne Argumente startet die Oberfläche.\n\n\
                 Befehle:\n\
                   status [--full]              Status als JSON\n\
                   list                         konfigurierte Jobs\n\
                   add-path <job> <pfad>...     Ordner zum Backup hinzufügen\n\
                   run restic|sync|watchdog <id>\n\
                   watchdog                     Wächter einmal ausführen\n\
                   apply                        systemd-Units schreiben/aktivieren\n\
                   disable-legacy               alte Units abschalten\n\
                   snapshots <job>              Snapshots als JSON\n\
                   ls <job> <snapshot> [pfad]   Dateien im Snapshot\n\
                   search <job> <snapshot> <muster>\n\
                   restore <job> <snapshot> <ziel> [--include pfad] [--dry-run]\n\
                   config-path                  Pfad der Konfigurationsdatei"
            );
            Some(0)
        }
        other => {
            eprintln!("Unbekannter Befehl: {}", other);
            Some(2)
        }
    }
}

fn run_ui() {
    let _instance = match single_instance() {
        Some(f) => f,
        None => {
            focus_existing();
            return;
        }
    };

    if env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    tauri::Builder::default()
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
            ready,
            config_get,
            config_path,
            config_save,
            apply_units,
            remove_units,
            disable_legacy_units,
            run_job,
            pick_folder,
            add_paths,
            remove_path,
            snapshots,
            snapshot_files,
            search_files,
            restore,
            open_path
        ])
        .run(tauri::generate_context!())
        .expect("Fehler beim Start der Tauri-App");
}

fn main() {
    static CLI_RESULT: OnceLock<Option<i32>> = OnceLock::new();
    let code = *CLI_RESULT.get_or_init(cli);
    if let Some(c) = code {
        std::process::exit(c);
    }
    run_ui();
}
