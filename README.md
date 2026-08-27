# indigo

Ein ruhiges Desktop-Widget für Linux, nativ in GTK 3 gezeichnet (Rust +
Cairo, kein Browser). Zeigt Systemwerte: CPU, RAM, Disk, GPU, Temperaturen,
Netz-Durchsatz, Leistungsaufnahme, IP — und kann die Lüfter des Systems
anzeigen und steuern, einzeln oder als Gruppe. Liegt frei auf dem
Bildschirm, immer im Vordergrund, verschiebbar per Ziehen an der Kopfzeile.

Die einzige Netzwerkverbindung ist die abschaltbare Update-Prüfung gegen
GitHub (siehe unten). Keine Telemetrie, kein Account. Liest lokal, schreibt
lokal.

## Bedienung

- **Ziehen an der Kopfzeile** verschiebt das Widget; Position wird gespeichert
- **Klick auf den Punkt** rechts oben: kollabiert auf eine Zeile (nur cpu/ram)
  und zurück
- **Klick auf cpu, ram oder gpu**: klappt die zehn Programme mit dem höchsten
  Verbrauch aus. RAM ist der residente Anteil (Pss, geteilte Seiten anteilig),
  gleichnamige Prozesse werden summiert — `chrome  3.4 gb (29)`; bei gpu zählt
  der belegte GPU-Speicher. Die ram-Zeile zeigt zusätzlich belegt/gesamt
  (`7.8/15.6 gb`, berechnet als MemTotal − MemAvailable)
- **Lüfter**: Balken ziehen = manueller PWM-Wert; Klick auf `auto`/Prozentwert
  rechts schaltet zwischen Firmware-Automatik und manuell um. Die Zeile `fans`
  steuert alle Lüfter gemeinsam
- **Rechtsklick**: Aktualisierungsintervall (1 s / 2 s / 5 s), Autostart,
  Auto-Update, Beenden
- **Tray-Icon**: Ein-/Ausblenden und Beenden

## Auto-Update

Beim Start prüft indigo die GitHub-Releases von `Zenovs/indigo`. Gibt es
eine neuere Version, lädt es das Binary nach `~/.local`, verifiziert die
Prüfsumme und ersetzt sich selbst. Abschaltbar per Rechtsklick-Menü
(«auto-update»); ohne Netz passiert still nichts. Das ist die einzige
Netzwerkverbindung des Tools — es werden keine Daten gesendet.

## Installation

Aus den GitHub-Releases, als Debian-Paket:

```sh
sudo apt install ./indigo_*_amd64.deb
```

Oder das rohe Binary direkt:

```sh
install -Dm755 indigo-x86_64-linux ~/.local/bin/indigo
```

Beim ersten Start richtet indigo den Autostart ein (abschaltbar per
Rechtsklick-Menü). Einstellungen liegen als JSON unter
`~/.config/indigo/settings.json`.

## Voraussetzungen

- Linux mit GTK 3 (`libgtk-3-0`, auf den meisten Desktops vorhanden)
- Für GPU-Werte: NVIDIA-Treiber mit NVML (bei installiertem proprietärem
  Treiber vorhanden). Ohne NVIDIA-Karte zeigen die GPU-Zeilen `n/a`
- Unter Wayland läuft das Widget automatisch über XWayland, weil GNOME
  always-on-top für native Wayland-Fenster nicht unterstützt

## Mainboard-Lüfter freischalten

Drehzahlen und PWM-Steuerung brauchen den Super-I/O-Treiber (`nct6775`) und
Schreibrecht auf die PWM-Dateien. Einmalig:

```sh
sudo sh packaging/setup-fan-control.sh
```

Das Skript lädt den Treiber (auch bei künftigen Boots), installiert eine
udev-Regel mit Schreibrecht für die eigene Nutzergruppe und listet die
gefundenen Lüfter. Das laufende Widget zeigt sie ohne Neustart an.

Auf vielen ASUS-Boards reserviert das BIOS den Sensorchip per ACPI — das
Skript erkennt das, trägt dann `acpi_enforce_resources=lax` in
`/etc/default/grub` ein (Backup: `grub.indigo-backup`) und verlangt einen
Reboot. Danach erscheinen die Lüfter automatisch.

Der GPU-Lüfter wird angezeigt, ist aber nicht steuerbar — NVIDIA verlangt
dafür root-Rechte.

## CPU-Leistungsaufnahme (RAPL) freischalten

`pwr` summiert GPU-Leistung (NVML) und CPU-Package (RAPL). RAPL ist ab Werk
nur für root lesbar (Seitenkanal-Härtung des Kernels). Wer den Wert möchte:

```sh
sudo chmod o+r /sys/class/powercap/intel-rapl:*/energy_uj
```

(gilt bis zum Reboot; dauerhaft per udev-Regel analog zum Lüfter-Skript).
Ohne Freigabe zeigt `pwr` nur die GPU-Leistung.

## Selbst bauen

Voraussetzungen: Rust (stable) und `libgtk-3-dev`. Kein Node, kein Bundler.

```sh
cd app && cargo build --release
```

## Ressourcenverbrauch

Ein einzelner Prozess, natives Cairo-Rendering — kein WebView, kein zweiter
Prozess. Gezeichnet wird nur, was sich sichtbar ändert: Ticks, bei denen
sich keine gerundete Anzeige bewegt, werden übersprungen. Ein grösseres
Intervall (Rechtsklick → 2 s / 5 s) senkt den Verbrauch entsprechend.

## Architektur

- `app/src/monitor.rs` — Sampler. Jede Quelle (sysinfo, NVML, hwmon, RAPL)
  ist einzeln gekapselt und fällt einzeln aus (`n/a` statt erfundener Zahlen)
- `app/src/fans.rs` — hwmon-Lüfter lesen und steuern
- `app/src/processes.rs` — Top-Listen; RAM ist Pss aus `smaps_rollup`,
  Fallback VmRSS — nie `/proc/pid/task`
- `app/src/ui/` — Fenster und Zeichnen, direkt mit Cairo/Pango
- `app/src/updater.rs` — Update-Prüfung gegen GitHub-Releases
- `app/src/tray.rs` — Tray-Icon über ksni

v0.1 war ein Tauri-Frontend (WebKitGTK); es liegt in der Git-Historie.

## Schriftlizenz

JetBrains Mono ist eingebettet, lizenziert unter der SIL Open Font License
(`app/assets/fonts/OFL.txt`).
