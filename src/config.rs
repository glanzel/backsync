use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

fn default_restic_schedule() -> String {
    "12h".into()
}

fn default_restic_delay() -> String {
    "3min".into()
}

fn default_restic_timeout() -> String {
    "12h".into()
}

fn default_sync_schedule() -> String {
    "6h".into()
}

fn default_sync_delay() -> String {
    "2min".into()
}

fn default_sync_timeout() -> String {
    "24h".into()
}

fn default_mode() -> String {
    "sync".into()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub state_dir: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub binary: String,
    pub max_restic_h: u64,
    pub max_sync_h: u64,
    pub watchdog_enabled: bool,
    pub watchdog_schedule: String,
    pub watchdog_startup: String,
    pub disable_legacy: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub alarm_file: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub watchdog_unit: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub watchdog_timer: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            state_dir: String::new(),
            binary: String::new(),
            max_restic_h: 48,
            max_sync_h: 18,
            watchdog_enabled: true,
            watchdog_schedule: "1h".into(),
            watchdog_startup: "10min".into(),
            disable_legacy: true,
            alarm_file: String::new(),
            watchdog_unit: String::new(),
            watchdog_timer: String::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct ResticJob {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub repository: String,
    pub password_file: String,
    pub cache_dir: String,
    pub paths: Vec<String>,
    pub exclude_file: String,
    pub exclude: Vec<String>,
    pub keep_daily: Option<u32>,
    pub keep_weekly: Option<u32>,
    pub keep_monthly: Option<u32>,
    pub keep_yearly: Option<u32>,
    pub schedule: String,
    pub startup: String,
    pub timeout: String,
    pub tags: Vec<String>,
    pub host: String,
    pub one_file_system: bool,
    pub exclude_caches: bool,
    pub unit: String,
    pub timer: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct SyncJob {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub source: String,
    pub target: String,
    pub mode: String,
    pub schedule: String,
    pub startup: String,
    pub timeout: String,
    pub backup_dir: String,
    pub exclude: Vec<String>,
    pub extra_flags: Vec<String>,
    pub transfers: Option<u32>,
    pub checkers: Option<u32>,
    pub unit: String,
    pub timer: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Config {
    pub settings: Settings,
    #[serde(rename = "restic")]
    pub restic_jobs: Vec<ResticJob>,
    #[serde(rename = "sync")]
    pub sync_jobs: Vec<SyncJob>,
}

impl ResticJob {
    pub fn setup_defaults(mut self) -> Self {
        if self.schedule.is_empty() {
            self.schedule = default_restic_schedule();
        }
        if self.startup.is_empty() {
            self.startup = default_restic_delay();
        }
        if self.timeout.is_empty() {
            self.timeout = default_restic_timeout();
        }
        if self.host.is_empty() {
            self.host = hostname();
        }
        self
    }
}

impl SyncJob {
    pub fn setup_defaults(mut self) -> Self {
        if self.mode.is_empty() {
            self.mode = default_mode();
        }
        if self.schedule.is_empty() {
            self.schedule = default_sync_schedule();
        }
        if self.startup.is_empty() {
            self.startup = default_sync_delay();
        }
        if self.timeout.is_empty() {
            self.timeout = default_sync_timeout();
        }
        self
    }
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

pub fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "localhost".into())
}

pub fn config_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("backup-status");
        }
    }
    home().join(".config/backup-status")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn expand(p: &str) -> String {
    if p == "~" {
        return home().to_string_lossy().into_owned();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return home().join(rest).to_string_lossy().into_owned();
    }
    p.to_string()
}

pub fn state_dir(cfg: &Config) -> PathBuf {
    if cfg.settings.state_dir.is_empty() {
        home().join(".local/state/backup-status")
    } else {
        PathBuf::from(expand(&cfg.settings.state_dir))
    }
}

pub fn log_dir(cfg: &Config) -> PathBuf {
    state_dir(cfg).join("logs")
}

pub fn log_path(cfg: &Config, kind: &str, id: &str) -> PathBuf {
    log_dir(cfg).join(format!("{}-{}.log", kind, unit_id(id)))
}

pub fn alarm_path(cfg: &Config) -> PathBuf {
    if !cfg.settings.alarm_file.is_empty() {
        return PathBuf::from(expand(&cfg.settings.alarm_file));
    }
    let legacy = home().join(".local/state/status");
    if legacy.is_dir() {
        legacy.join("ALARM.txt")
    } else {
        state_dir(cfg).join("ALARM.txt")
    }
}

pub fn systemd_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("systemd/user");
        }
    }
    home().join(".config/systemd/user")
}

pub fn unit_id(id: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for c in id.to_lowercase().chars() {
        let ok = c.is_ascii_alphanumeric() || c == '-' || c == '_';
        if ok {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "job".into()
    } else {
        trimmed
    }
}

pub fn restic_unit(job: &ResticJob) -> String {
    if !job.unit.is_empty() {
        job.unit.clone()
    } else {
        format!("backup-status-restic-{}.service", unit_id(&job.id))
    }
}

pub fn restic_timer(job: &ResticJob) -> String {
    if !job.timer.is_empty() {
        job.timer.clone()
    } else if !job.unit.is_empty() {
        job.unit.replace(".service", ".timer")
    } else {
        format!("backup-status-restic-{}.timer", unit_id(&job.id))
    }
}

pub fn sync_unit(job: &SyncJob) -> String {
    if !job.unit.is_empty() {
        job.unit.clone()
    } else {
        format!("backup-status-sync-{}.service", unit_id(&job.id))
    }
}

pub fn sync_timer(job: &SyncJob) -> String {
    if !job.timer.is_empty() {
        job.timer.clone()
    } else if !job.unit.is_empty() {
        job.unit.replace(".service", ".timer")
    } else {
        format!("backup-status-sync-{}.timer", unit_id(&job.id))
    }
}

pub fn watchdog_unit(cfg: &Config) -> String {
    if cfg.settings.watchdog_unit.is_empty() {
        "backup-status-watchdog.service".into()
    } else {
        cfg.settings.watchdog_unit.clone()
    }
}

pub fn watchdog_timer(cfg: &Config) -> String {
    if cfg.settings.watchdog_timer.is_empty() {
        "backup-status-watchdog.timer".into()
    } else {
        cfg.settings.watchdog_timer.clone()
    }
}

pub fn load() -> Config {
    let path = config_path();
    if let Ok(text) = fs::read_to_string(&path) {
        if let Ok(cfg) = toml::from_str::<Config>(&text) {
            return normalize(cfg);
        }
        eprintln!("Konfiguration unlesbar, nutze Vorgaben: {}", path.display());
    }
    let cfg = seed();
    let _ = save(&cfg);
    normalize(cfg)
}

pub fn save(cfg: &Config) -> Result<(), String> {
    let cfg = normalize(cfg.clone());
    validate(&cfg)?;
    let dir = config_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("Ordner {}: {}", dir.display(), e))?;
    let path = config_path();
    if path.exists() {
        let _ = fs::copy(&path, path.with_extension("toml.bak"));
    }
    let text = toml::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text).map_err(|e| format!("Schreiben {}: {}", tmp.display(), e))?;
    fs::rename(&tmp, &path).map_err(|e| format!("Ersetzen {}: {}", path.display(), e))?;
    Ok(())
}

fn normalize(mut cfg: Config) -> Config {
    cfg.restic_jobs = cfg
        .restic_jobs
        .into_iter()
        .map(|j| j.setup_defaults())
        .collect();
    cfg.sync_jobs = cfg
        .sync_jobs
        .into_iter()
        .map(|j| j.setup_defaults())
        .collect();
    for j in &mut cfg.restic_jobs {
        j.id = j.id.trim().to_string();
        if j.name.trim().is_empty() {
            j.name = j.id.clone();
        }
    }
    for j in &mut cfg.sync_jobs {
        j.id = j.id.trim().to_string();
        if j.name.trim().is_empty() {
            j.name = j.id.clone();
        }
    }
    if cfg.settings.watchdog_schedule.is_empty() {
        cfg.settings.watchdog_schedule = "1h".into();
    }
    if cfg.settings.watchdog_startup.is_empty() {
        cfg.settings.watchdog_startup = "10min".into();
    }
    cfg
}

fn validate(cfg: &Config) -> Result<(), String> {
    let mut ids = HashSet::new();
    for j in &cfg.restic_jobs {
        if j.id.is_empty() {
            return Err("Restic-Job ohne Kennung".into());
        }
        if !ids.insert(format!("restic:{}", unit_id(&j.id))) {
            return Err(format!("Doppelte Kennung: {}", j.id));
        }
        if j.enabled {
            if j.repository.trim().is_empty() {
                return Err(format!("{}: Repository fehlt", j.name));
            }
            if j.password_file.trim().is_empty() {
                return Err(format!("{}: Passwortdatei fehlt", j.name));
            }
            if j.paths.iter().all(|p| p.trim().is_empty()) {
                return Err(format!("{}: keine Quellordner", j.name));
            }
        }
    }
    for j in &cfg.sync_jobs {
        if j.id.is_empty() {
            return Err("Sync-Job ohne Kennung".into());
        }
        if !ids.insert(format!("sync:{}", unit_id(&j.id))) {
            return Err(format!("Doppelte Kennung: {}", j.id));
        }
        if j.enabled {
            if j.source.trim().is_empty() || j.target.trim().is_empty() {
                return Err(format!("{}: Quelle oder Ziel fehlt", j.name));
            }
            if j.mode != "sync" && j.mode != "copy" {
                return Err(format!("{}: Modus muss sync oder copy sein", j.name));
            }
        }
    }
    Ok(())
}

fn seed() -> Config {
    let mut cfg = Config::default();
    let h = home();
    let sys = systemd_dir();

    // Bestehende Installation übernehmen: gleiche Unit-Namen behalten, damit
    // der Tray und andere Helfer unverändert weiterfunktionieren.
    let legacy_restic = sys.join("restic-backup.service").exists();
    let legacy_sync = sys.join("media-sync.service").exists();
    let legacy_watchdog = sys.join("backup-watchdog.service").exists();

    let legacy_alarm = h.join(".local/state/status");
    if legacy_alarm.is_dir() {
        cfg.settings.alarm_file = legacy_alarm
            .join("ALARM.txt")
            .to_string_lossy()
            .into_owned();
    }
    if legacy_watchdog {
        cfg.settings.watchdog_unit = "backup-watchdog.service".into();
        cfg.settings.watchdog_timer = "backup-watchdog.timer".into();
    }

    let restic_conf = h.join(".config/restic");
    let sources = restic_conf.join("sources.txt");
    let mut paths = Vec::new();
    if let Ok(text) = fs::read_to_string(&sources) {
        for line in text.lines() {
            let l = line.trim();
            if !l.is_empty() && !l.starts_with('#') {
                paths.push(l.to_string());
            }
        }
    }
    if paths.is_empty() && h.join("dok").is_dir() {
        paths.push(h.join("dok").to_string_lossy().into_owned());
    }

    let pass = restic_conf.join("pcloud-repo.pass");
    if pass.exists() && !paths.is_empty() {
        let exclude = restic_conf.join("exclude.txt");
        cfg.restic_jobs.push(
            ResticJob {
                id: "dokumente".into(),
                name: "Dokumente & Code".into(),
                enabled: true,
                repository: "rclone:pcloud:backups/restic".into(),
                password_file: pass.to_string_lossy().into_owned(),
                cache_dir: h.join(".cache/restic").to_string_lossy().into_owned(),
                paths,
                exclude_file: if exclude.exists() {
                    exclude.to_string_lossy().into_owned()
                } else {
                    String::new()
                },
                keep_daily: Some(7),
                keep_weekly: Some(4),
                keep_monthly: Some(12),
                keep_yearly: Some(2),
                tags: vec!["scheduled".into()],
                one_file_system: true,
                exclude_caches: true,
                unit: if legacy_restic {
                    "restic-backup.service".into()
                } else {
                    String::new()
                },
                timer: if legacy_restic {
                    "restic-backup.timer".into()
                } else {
                    String::new()
                },
                ..Default::default()
            }
            .setup_defaults(),
        );
    }

    let media = h.join("media");
    if media.is_dir() {
        cfg.sync_jobs.push(
            SyncJob {
                id: "medien".into(),
                name: "Musik & Medien".into(),
                enabled: true,
                source: media.to_string_lossy().into_owned(),
                target: "pcloud:media".into(),
                backup_dir: ".deleted/{timestamp}".into(),
                exclude: vec![".deleted/**".into()],
                transfers: Some(4),
                checkers: Some(8),
                unit: if legacy_sync {
                    "media-sync.service".into()
                } else {
                    String::new()
                },
                timer: if legacy_sync {
                    "media-sync.timer".into()
                } else {
                    String::new()
                },
                ..Default::default()
            }
            .setup_defaults(),
        );
    }

    cfg
}

pub fn add_path(job_id: &str, paths: &[String]) -> Result<Config, String> {
    let mut cfg = load();
    let job = cfg
        .restic_jobs
        .iter_mut()
        .find(|j| j.id == job_id)
        .ok_or_else(|| format!("Restic-Job {} nicht gefunden", job_id))?;
    for p in paths {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        if !job.paths.iter().any(|x| x == p) {
            job.paths.push(p.to_string());
        }
    }
    save(&cfg)?;
    Ok(cfg)
}

pub fn remove_path(job_id: &str, path: &str) -> Result<Config, String> {
    let mut cfg = load();
    let job = cfg
        .restic_jobs
        .iter_mut()
        .find(|j| j.id == job_id)
        .ok_or_else(|| format!("Restic-Job {} nicht gefunden", job_id))?;
    job.paths.retain(|p| p != path);
    save(&cfg)?;
    Ok(cfg)
}

pub fn find_restic<'a>(cfg: &'a Config, id: &str) -> Option<&'a ResticJob> {
    cfg.restic_jobs.iter().find(|j| j.id == id)
}

pub fn find_sync<'a>(cfg: &'a Config, id: &str) -> Option<&'a SyncJob> {
    cfg.sync_jobs.iter().find(|j| j.id == id)
}
