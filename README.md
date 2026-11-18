# backup-status

Verwaltung für **Backup (restic)** und **Sync (rclone)** – mit Oberfläche oder
rein über die Konfigurationsdatei. Alles, was die App tut, steht in einer
TOML-Datei; die Oberfläche ist nur ein bequemer Editor dafür.

- **Backup** mit restic: verschlüsselte Snapshots, Aufbewahrung, Ausschlüsse
- **Sync** mit rclone: `sync` (Ziel angleichen) oder `copy` (nichts löschen),
  optional mit Sicherungsordner für gelöschte Dateien
- **Wiederherstellen**: Snapshots durchsuchen, Dateien ansehen, einzeln oder
  komplett zurückholen – inklusive Probelauf
- Erzeugt aus der Konfiguration **systemd-Timer** (kein Cron, kein Dauerprozess)
- Funktioniert vollständig **ohne Oberfläche** (`backup-status run …`)

## Konfiguration

```
~/.config/backup-status/config.toml
```

Beim ersten Start wird die Datei aus einer vorhandenen Installation erzeugt
(`~/.config/restic/sources.txt`, `pcloud`-Remote, `~/media`). Danach ist sie die
einzige Quelle der Wahrheit.

```toml
[settings]
max_restic_h = 48          # Wächter: Alarm, wenn Backup älter
max_sync_h = 18
watchdog_enabled = true
watchdog_schedule = "1h"
watchdog_startup = "10min"
disable_legacy = true      # alte Units beim Anwenden abschalten

[[restic]]
id = "dokumente"
name = "Dokumente & Code"
enabled = true
repository = "rclone:pcloud:backups/restic"
password_file = "/home/glanzel/.config/restic/pcloud-repo.pass"
cache_dir = "/home/glanzel/.cache/restic"
paths = ["/home/glanzel/dok", "/var/code"]
exclude_file = "/home/glanzel/.config/restic/exclude.txt"
exclude = []
keep_daily = 7
keep_weekly = 4
keep_monthly = 12
keep_yearly = 2
schedule = "12h"           # systemd: OnUnitActiveSec
startup = "3min"           # systemd: OnStartupSec
timeout = "12h"
tags = ["scheduled"]
host = "glanzen2"
one_file_system = true
exclude_caches = true

[[sync]]
id = "medien"
name = "Musik & Medien"
enabled = true
source = "/home/glanzel/media"
target = "pcloud:media"
mode = "sync"              # oder "copy"
schedule = "6h"
startup = "2min"
timeout = "24h"
backup_dir = ".deleted/{timestamp}"
exclude = [".deleted/**"]
transfers = 4
checkers = 8
```

Mehrere `[[restic]]`- und `[[sync]]`-Blöcke sind erlaubt – jeder Job bekommt
eigene Units, eigenes Log und eigene Kennung.

## Ohne Oberfläche

```bash
backup-status list                          # konfigurierte Jobs
backup-status add-path dokumente ~/fotos    # Ordner hinzufügen (landet in der TOML)
backup-status run restic dokumente          # Backup jetzt
backup-status run sync medien               # Sync jetzt
backup-status watchdog                      # Wächter einmal prüfen
backup-status apply                         # systemd-Units schreiben + Timer aktivieren
backup-status disable-legacy                # restic-backup/media-sync/backup-watchdog abschalten
backup-status status [--full]               # Status als JSON
backup-status snapshots dokumente
backup-status ls dokumente latest /home/glanzel/dok
backup-status search dokumente latest rechnung.pdf
backup-status restore dokumente latest ~/wiederhergestellt --include /home/glanzel/dok/Steuer
```

`run` und `restore` schreiben nach `~/.local/state/backup-status/logs/`.
`status` ist das, was auch die Oberfläche nutzt.

## Oberfläche

Fünf Bereiche: **Übersicht**, **Backup**, **Sync**, **Wiederherstellen**,
**Einstellungen**.

- Jobs anlegen, Ordner hinzufügen/entfernen, Zeitpläne und Aufbewahrung ändern
  → wird sofort in `config.toml` gespeichert
- Wiederherstellen: Snapshot wählen, im Snapshot browsen oder nach Dateien
  suchen, Zielordner wählen, optional Probelauf
- Einstellungen: Wächter-Schwellen, Units schreiben/entfernen, alte Units
  abschalten

Die Oberfläche liest und schreibt nur die Konfiguration. Wer die TOML direkt
bearbeitet, bekommt dieselben Ergebnisse.

## systemd

`backup-status apply` erzeugt pro aktivem Job:

```
~/.config/systemd/user/backup-status-restic-<id>.{service,timer}
~/.config/systemd/user/backup-status-sync-<id>.{service,timer}
~/.config/systemd/user/backup-status-watchdog.{service,timer}
```

Timer werden aktiviert (`enable --now`), Dienste laufen als `oneshot` mit
`Restart=on-failure`. Der Wächter schreibt Alarm nach
`~/.local/state/backup-status/ALARM.txt` und schickt `notify-send`.

## Bauen

```bash
cargo build --release
cp target/release/backup-status ~/.local/bin/
```

Benötigt `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `libsoup-3.0-dev` zum Bauen,
`restic`, `rclone`, `systemctl` und `zenity` (Ordnerauswahl) im Betrieb.

## Aufbau

| Datei | Zweck |
|---|---|
| `src/config.rs` | TOML laden/speichern, Vorgaben, Validierung |
| `src/exec.rs` | Kommandos mit Timeout und Prozessgruppe, Logs |
| `src/runner.rs` | restic/rclone aufrufen, Wächter, Restore |
| `src/status.rs` | Status für Oberfläche und CLI (mit Cache) |
| `src/units.rs` | systemd-Units aus der Konfiguration erzeugen |
| `src/main.rs` | Tauri-Commands und CLI |
| `ui/index.html` | Oberfläche (HTML/CSS/JS, kein Bundler) |

Fenster startet unsichtbar und zeigt sich nach dem ersten Render;
`WEBKIT_DISABLE_DMABUF_RENDERER=1` verhindert weiße Flächen auf NVIDIA/X11.
