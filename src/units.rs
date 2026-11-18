use crate::config::{self, Config};
use crate::exec;
use std::fs;
use std::path::{Path, PathBuf};

pub fn unit_path(name: &str) -> PathBuf {
    config::systemd_dir().join(name)
}

fn write_unit(name: &str, content: &str) -> Result<PathBuf, String> {
    let dir = config::systemd_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
    let path = dir.join(name);
    let tmp = dir.join(format!("{}.tmp", name));
    fs::write(&tmp, content).map_err(|e| format!("{}: {}", tmp.display(), e))?;
    fs::rename(&tmp, &path).map_err(|e| format!("{}: {}", path.display(), e))?;
    Ok(path)
}

fn bin_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            config::home()
                .join(".local/bin/backup-status")
                .to_string_lossy()
                .into_owned()
        })
}

fn log_file(cfg: &Config, kind: &str, id: &str) -> String {
    config::log_path(cfg, kind, id)
        .to_string_lossy()
        .into_owned()
}

fn restic_service(cfg: &Config, id: &str) -> String {
    let job = config::find_restic(cfg, id).expect("job");
    let log = log_file(cfg, "restic", id);
    let mut s = String::new();
    s.push_str("[Unit]\n");
    s.push_str(&format!(
        "Description=Backup-Status: restic „{}“\n",
        job.name
    ));
    s.push_str("StartLimitIntervalSec=3h\nStartLimitBurst=8\n");
    s.push_str("OnFailure=notify-failure@%n.service\n\n");
    s.push_str("[Service]\nType=oneshot\n");
    s.push_str(&format!("ExecStart={} run restic {}\n", bin_path(), job.id));
    s.push_str(&format!("TimeoutStartSec={}\n", job.timeout));
    s.push_str("Nice=10\nIOSchedulingClass=idle\n");
    s.push_str("Restart=on-failure\nRestartSec=15min\n");
    s.push_str(&format!("StandardOutput=append:{}\n", log));
    s.push_str(&format!("StandardError=append:{}\n", log));
    s
}

fn restic_timer(cfg: &Config, id: &str) -> String {
    let job = config::find_restic(cfg, id).expect("job");
    let mut s = String::new();
    s.push_str("[Unit]\n");
    s.push_str(&format!(
        "Description=Zeitplan für restic „{}“\n\n",
        job.name
    ));
    s.push_str("[Timer]\n");
    s.push_str(&format!("OnStartupSec={}\n", job.startup));
    s.push_str(&format!("OnUnitActiveSec={}\n", job.schedule));
    s.push_str("RandomizedDelaySec=5min\nAccuracySec=1min\n");
    s.push_str(&format!("Unit={}\n\n", config::restic_unit(job)));
    s.push_str("[Install]\nWantedBy=timers.target\n");
    s
}

fn sync_service(cfg: &Config, id: &str) -> String {
    let job = config::find_sync(cfg, id).expect("job");
    let log = log_file(cfg, "sync", id);
    let mut s = String::new();
    s.push_str("[Unit]\n");
    s.push_str(&format!("Description=Backup-Status: Sync „{}“\n", job.name));
    s.push_str("Wants=network-online.target\nAfter=network-online.target\n");
    s.push_str("StartLimitIntervalSec=2h\nStartLimitBurst=4\n");
    s.push_str("OnFailure=notify-failure@%n.service\n\n");
    s.push_str("[Service]\nType=oneshot\n");
    s.push_str(&format!("ExecStart={} run sync {}\n", bin_path(), job.id));
    s.push_str(&format!("TimeoutStartSec={}\n", job.timeout));
    s.push_str("Nice=10\nIOSchedulingClass=idle\n");
    s.push_str("Restart=on-failure\nRestartSec=30min\n");
    s.push_str(&format!("StandardOutput=append:{}\n", log));
    s.push_str(&format!("StandardError=append:{}\n", log));
    s
}

fn sync_timer(cfg: &Config, id: &str) -> String {
    let job = config::find_sync(cfg, id).expect("job");
    let mut s = String::new();
    s.push_str("[Unit]\n");
    s.push_str(&format!("Description=Zeitplan für Sync „{}“\n\n", job.name));
    s.push_str("[Timer]\n");
    s.push_str(&format!("OnStartupSec={}\n", job.startup));
    s.push_str(&format!("OnUnitActiveSec={}\n", job.schedule));
    s.push_str("RandomizedDelaySec=10min\nAccuracySec=1min\n");
    s.push_str(&format!("Unit={}\n\n", config::sync_unit(job)));
    s.push_str("[Install]\nWantedBy=timers.target\n");
    s
}

fn watchdog_service(cfg: &Config) -> String {
    let log = log_file(cfg, "watchdog", "main");
    let mut s = String::new();
    s.push_str("[Unit]\nDescription=Backup-Status: Wächter\n");
    s.push_str("Wants=network-online.target\nAfter=network-online.target\n");
    s.push_str("StartLimitIntervalSec=2h\nStartLimitBurst=6\n");
    s.push_str("OnFailure=notify-failure@%n.service\n\n");
    s.push_str("[Service]\nType=oneshot\n");
    s.push_str(&format!("ExecStart={} run watchdog\n", bin_path()));
    s.push_str("Nice=15\nIOSchedulingClass=idle\nTimeoutStartSec=10min\n");
    s.push_str("Restart=on-failure\nRestartSec=20min\n");
    s.push_str(&format!("StandardOutput=append:{}\n", log));
    s.push_str(&format!("StandardError=append:{}\n", log));
    s
}

fn watchdog_timer(cfg: &Config) -> String {
    let mut s = String::new();
    s.push_str("[Unit]\nDescription=Backup-Status: Wächter-Zeitplan\n\n");
    s.push_str("[Timer]\n");
    s.push_str(&format!("OnStartupSec={}\n", cfg.settings.watchdog_startup));
    s.push_str(&format!(
        "OnUnitActiveSec={}\n",
        cfg.settings.watchdog_schedule
    ));
    s.push_str("RandomizedDelaySec=5min\nAccuracySec=1min\n");
    s.push_str(&format!("Unit={}\n\n", config::watchdog_unit(cfg)));
    s.push_str("[Install]\nWantedBy=timers.target\n");
    s
}

/// Schreibt alle Units und aktiviert die Timer. Gibt die Liste der
/// geschriebenen Dateien zurück.
pub fn install(cfg: &Config) -> Result<Vec<String>, String> {
    let mut written = Vec::new();

    for job in &cfg.restic_jobs {
        if !job.enabled {
            continue;
        }
        let svc = config::restic_unit(job);
        let tmr = config::restic_timer(job);
        write_unit(&svc, &restic_service(cfg, &job.id))?;
        write_unit(&tmr, &restic_timer(cfg, &job.id))?;
        written.push(svc);
        written.push(tmr);
    }

    for job in &cfg.sync_jobs {
        if !job.enabled {
            continue;
        }
        let svc = config::sync_unit(job);
        let tmr = config::sync_timer(job);
        write_unit(&svc, &sync_service(cfg, &job.id))?;
        write_unit(&tmr, &sync_timer(cfg, &job.id))?;
        written.push(svc);
        written.push(tmr);
    }

    if cfg.settings.watchdog_enabled {
        let svc = config::watchdog_unit(cfg);
        let tmr = config::watchdog_timer(cfg);
        write_unit(&svc, &watchdog_service(cfg))?;
        write_unit(&tmr, &watchdog_timer(cfg))?;
        written.push(svc);
        written.push(tmr);
    }

    let _ = exec::run(
        "systemctl",
        &["--user", "daemon-reload"],
        &[],
        std::time::Duration::from_secs(15),
    );

    if cfg.settings.disable_legacy {
        for name in legacy_leftovers(cfg) {
            let _ = exec::run(
                "systemctl",
                &["--user", "disable", "--now", &name],
                &[],
                std::time::Duration::from_secs(15),
            );
        }
    }

    for name in &written {
        if name.ends_with(".timer") {
            let _ = exec::run(
                "systemctl",
                &["--user", "enable", "--now", name],
                &[],
                std::time::Duration::from_secs(15),
            );
        }
    }

    Ok(written)
}

const LEGACY: [&str; 3] = [
    "restic-backup.timer",
    "media-sync.timer",
    "backup-watchdog.timer",
];

/// Alte Timer, die von der aktuellen Konfiguration nicht verwendet werden.
/// Werden bestehende Namen übernommen, bleiben sie unangetastet.
pub fn legacy_leftovers(cfg: &Config) -> Vec<String> {
    let mut used: Vec<String> = Vec::new();
    for job in &cfg.restic_jobs {
        used.push(config::restic_timer(job));
    }
    for job in &cfg.sync_jobs {
        used.push(config::sync_timer(job));
    }
    used.push(config::watchdog_timer(cfg));

    LEGACY
        .iter()
        .filter(|name| !used.iter().any(|u| u == *name))
        .map(|name| name.to_string())
        .collect()
}

/// Schaltet alte Timer ab, die nicht mehr zur Konfiguration gehören.
pub fn disable_legacy(cfg: &Config) -> Result<Vec<String>, String> {
    let mut done = Vec::new();
    for name in legacy_leftovers(cfg) {
        let _ = exec::run(
            "systemctl",
            &["--user", "disable", "--now", &name],
            &[],
            std::time::Duration::from_secs(15),
        );
        done.push(name);
    }
    Ok(done)
}

pub fn uninstall(cfg: &Config) -> Result<(), String> {
    let dir = config::systemd_dir();
    let mut names: Vec<String> = Vec::new();
    for job in &cfg.restic_jobs {
        names.push(config::restic_unit(job));
        names.push(config::restic_timer(job));
    }
    for job in &cfg.sync_jobs {
        names.push(config::sync_unit(job));
        names.push(config::sync_timer(job));
    }
    names.push(config::watchdog_unit(cfg));
    names.push(config::watchdog_timer(cfg));

    if let Ok(entries) = fs::read_dir(&dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("backup-status-") && !names.contains(&name) {
                names.push(name);
            }
        }
    }

    for name in names {
        let path = dir.join(&name);
        if path.exists() {
            let _ = exec::run(
                "systemctl",
                &["--user", "disable", "--now", &name],
                &[],
                std::time::Duration::from_secs(15),
            );
            let _ = fs::remove_file(&path);
        }
    }
    let _ = exec::run(
        "systemctl",
        &["--user", "daemon-reload"],
        &[],
        std::time::Duration::from_secs(15),
    );
    Ok(())
}

pub fn is_installed(name: &str) -> bool {
    Path::new(&unit_path(name)).exists()
}
