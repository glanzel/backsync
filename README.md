# backup-status

Statusfenster als **Tauri-2-App** (Rust + WebView). Zeigt Karten für
restic-Backup, Media-Sync und Wächter, die pCloud-Belegung, einen Alarm-Banner
und Aktionsknöpfe.

- Kein Webserver, kein offener Port – die Oberfläche ruft Tauri-Commands auf
- Aktualisiert alle 20 s (JS-Intervall), Knopf „Neu laden" für sofort
- Start über Tray-Menü „Status öffnen" oder Anwendungsmenü „Backup-Status"
- Fenster erscheint erst, wenn der erste Inhalt gezeichnet ist (kein weißer Blitz)

## Warum es nicht mehr „antwortet nicht" / weiß bleibt

- `status`/`refresh` laufen als `async` Commands auf dem Blocking-Pool, **nicht**
  auf dem GTK-Hauptthread. Netzabfragen können das Fenster nicht mehr einfrieren.
- `status_fast` liefert sofort systemd-Zustände, Logs und Alarm; restic/rclone
  werden danach nachgeladen (pCloud-Abfragen sind teils 15–60 s langsam).
- Alle externen Kommandos (`restic`, `rclone`, `systemctl`, `date`) haben
  Timeouts und laufen in einer eigenen Prozessgruppe, die beim Abbruch
  vollständig beendet wird.
- Ergebnisse werden 5 min gecacht (Fehlschläge nur 20 s), „Neu laden" erzwingt
  eine neue Abfrage.
- `WEBKIT_DISABLE_DMABUF_RENDERER=1` wird gesetzt: der WebKit-DMA-BUF-Renderer
  bleibt auf NVIDIA/X11 (GeForce MX150) sporadisch weiß.
- Das Fenster startet unsichtbar und zeigt sich nach dem ersten Render bzw.
  spätestens nach 1,5 s.

## Kopfmodus (für Tests und Skripte)

```bash
~/.local/bin/backup-status --print
```

Gibt den kompletten Status als JSON aus, ohne Fenster zu öffnen.

## Bauen und installieren

```bash
cargo build --release
cp target/release/backup-status ~/.local/bin/
```

## Aufbau

| Datei | Zweck |
|---|---|
| `src/main.rs` | Commands `status`, `status_fast`, `refresh`, `action`, `ready`; Abfrage von systemd/restic/rclone |
| `ui/index.html` | Oberfläche (HTML/CSS/JS, kein Bundler) |
| `tauri.conf.json` | Fenster, `withGlobalTauri`, Icon |
| `capabilities/default.json` | `core:default` für das Fenster |
| `icons/icon.png` | Fenster-/Menüicon |

## Abhängigkeiten

`libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `libsoup-3.0-dev` (Build).
WebKit-Prozesse im Betrieb sind normal (`WebKitWebProcess`).
