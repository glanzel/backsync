use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

pub fn path_env() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        "/usr/local/bin:/usr/bin:/bin:/snap/bin".into()
    } else {
        format!("{}/.local/bin:/usr/local/bin:/usr/bin:/bin:/snap/bin", home)
    }
}

#[derive(Default, Debug)]
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

fn base(cmd: &str, args: &[&str], envs: &[(&str, String)]) -> Command {
    let mut c = Command::new(cmd);
    c.args(args)
        .env("PATH", path_env())
        .stdin(Stdio::null())
        // Eigene Prozessgruppe, damit beim Timeout auch rclone-Kinder beendet werden.
        .process_group(0);
    for (k, v) in envs {
        c.env(k, v);
    }
    c
}

fn kill_group(pgid: i32) {
    let _ = Command::new("kill")
        .args(["-TERM", &format!("-{}", pgid)])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

pub fn run(cmd: &str, args: &[&str], envs: &[(&str, String)], timeout: Duration) -> Output {
    let mut child = match base(cmd, args, envs)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return Output {
                code: -1,
                stdout: String::new(),
                stderr: format!("{}: {}", cmd, e),
            }
        }
    };
    let pgid = child.id() as i32;
    let (tx, rx) = mpsc::channel();
    let (tx2, rx2) = mpsc::channel();
    if let Some(mut out) = child.stdout.take() {
        thread::spawn(move || {
            let mut s = String::new();
            let _ = out.read_to_string(&mut s);
            let _ = tx.send(s);
        });
    }
    if let Some(mut err) = child.stderr.take() {
        thread::spawn(move || {
            let mut s = String::new();
            let _ = err.read_to_string(&mut s);
            let _ = tx2.send(s);
        });
    }

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_group(pgid);
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }

    let stdout = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
    let stderr = rx2.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
    let code = if timed_out {
        124
    } else {
        child
            .try_wait()
            .ok()
            .flatten()
            .and_then(|s| s.code())
            .unwrap_or(-1)
    };
    Output {
        code,
        stdout,
        stderr: if timed_out {
            format!("Zeitüberschreitung nach {}s\n{}", timeout.as_secs(), stderr)
        } else {
            stderr
        },
    }
}

/// Führt ein Kommando aus und schreibt Ausgabe laufend in eine Logdatei.
/// Rückgabe: Exit-Code (124 bei Timeout).
pub fn run_logged(
    cmd: &str,
    args: &[&str],
    envs: &[(&str, String)],
    timeout: Duration,
    logfile: &Path,
) -> Result<i32, String> {
    if let Some(dir) = logfile.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("Logordner {}: {}", dir.display(), e))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(logfile)
        .map_err(|e| format!("Logdatei {}: {}", logfile.display(), e))?;
    let _ = writeln!(file, "=== {} start: {} {}", now_iso(), cmd, args.join(" "));

    let mut child = base(cmd, args, envs)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {}", cmd, e))?;
    let pgid = child.id() as i32;

    let mut writers = Vec::new();
    if let Some(out) = child.stdout.take() {
        let f = file.try_clone().map_err(|e| format!("Logdatei: {}", e))?;
        writers.push(thread::spawn(move || copy_stream(out, f)));
    }
    if let Some(err) = child.stderr.take() {
        let f = file.try_clone().map_err(|e| format!("Logdatei: {}", e))?;
        writers.push(thread::spawn(move || copy_stream(err, f)));
    }

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_group(pgid);
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                thread::sleep(Duration::from_millis(150));
            }
            Err(_) => break,
        }
    }
    for w in writers {
        let _ = w.join();
    }
    let code = if timed_out {
        124
    } else {
        child
            .try_wait()
            .ok()
            .flatten()
            .and_then(|s| s.code())
            .unwrap_or(-1)
    };
    let _ = writeln!(file, "=== {} ende: Code {}", now_iso(), code);
    Ok(code)
}

fn copy_stream<R: Read>(mut r: R, mut file: std::fs::File) {
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let _ = file.write_all(&buf[..n]);
                let _ = file.flush();
            }
        }
    }
}

pub fn now_iso() -> String {
    run("date", &["-Is"], &[], Duration::from_secs(5))
        .stdout
        .trim()
        .to_string()
}

/// Formatiert einen ISO-Zeitstempel für die Anzeige. Ohne Netz, nur lokal.
pub fn human(ts: &str) -> String {
    if ts.is_empty() || ts == "n/a" {
        return "–".into();
    }
    let out = run(
        "date",
        &["-d", ts, "+%d.%m.%Y %H:%M"],
        &[],
        Duration::from_secs(5),
    );
    if out.stdout.trim().is_empty() {
        ts.into()
    } else {
        out.stdout.trim().into()
    }
}

pub fn tail(path: &Path, n: usize) -> String {
    let content = fs::read_to_string(path).unwrap_or_default();
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

pub fn fmt_bytes(b: f64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{:.1} {}", v, units[i])
}

pub fn which(candidates: &[&str]) -> Option<String> {
    for c in candidates {
        let out = run(
            "sh",
            &["-c", &format!("command -v {}", c)],
            &[],
            Duration::from_secs(3),
        );
        let p = out.stdout.trim();
        if !p.is_empty() {
            return Some(p.to_string());
        }
    }
    None
}
