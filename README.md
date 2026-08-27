# indigo

Ein ruhiges Desktop-Widget für Linux, das Systemwerte anzeigt: CPU, RAM,
Disk, GPU, Temperaturen, Netz-Durchsatz, Leistungsaufnahme, IP — und die
Lüfter des Systems anzeigen und steuern kann. Liegt frei auf dem Bildschirm,
immer im Vordergrund, verschiebbar per Ziehen an der Kopfzeile.

Kein Netzwerkzugriff, keine Telemetrie, kein Account. Liest lokal, schreibt
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
  Beenden
- **Tray-Icon**: Ein-/Ausblenden und Beenden

## Voraussetzungen

- Linux mit GTK 3 / WebKitGTK 4.1 (Ubuntu 22.04+, `libwebkit2gtk-4.1-0`,
  `libgtk-3-0`, `libayatana-appindicator3-1`)
- Für GPU-Werte: NVIDIA-Treiber mit NVML (bei installiertem proprietärem
  Treiber vorhanden). Ohne NVIDIA-Karte zeigen die GPU-Zeilen `n/a`
- Unter Wayland läuft das Widget automatisch über XWayland, weil GNOME
  always-on-top für native Wayland-Fenster nicht unterstützt

## Installation

Aus dem Release-Bundle:

```sh
sudo apt install ./indigo_0.1.0_amd64.deb
# oder das AppImage direkt starten:
chmod +x indigo_0.1.0_amd64.AppImage && ./indigo_0.1.0_amd64.AppImage
```

Beim ersten Start richtet indigo den Autostart ein (abschaltbar per
Rechtsklick-Menü). Einstellungen liegen als JSON unter
`~/.config/indigo/settings.json`.

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

Voraussetzungen: Rust (stable), Node (nur für `tsc`), sowie
`libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libayatana-appindicator3-dev
libssl-dev build-essential`.

```sh
npm install
npm run build:ui
cd src-tauri && cargo build --release
# bundles (deb + appimage):
npx tauri build
```

## Ressourcenverbrauch

Das Widget rendert nur, was sich sichtbar ändert: Ticks ohne Änderung der
gerundeten Anzeigewerte werden gar nicht erst ans Frontend geschickt. Im
echten Leerlauf (stabile Werte) liegt der Verbrauch unter 1 % eines Kerns,
kollabiert darunter; ändern sich Werte laufend (System unter Last), kostet
das Nachzeichnen je nach Intervall 2–3 % eines Kerns — der Grossteil davon
ist der WebKitGTK-Renderpfad, nicht die Messung selbst (Sampling: ~4 ms pro
Tick). Grösseres Intervall (Rechtsklick → 2 s / 5 s) senkt den Verbrauch
proportional.

## Architektur

- `src-tauri/` — Rust-Backend (Tauri v2). Liest alle Werte in einem
  Sampler-Thread und pusht sie als ein einziges `stats`-Event ans Frontend.
  Jede Quelle (sysinfo, NVML, hwmon, RAPL) ist einzeln gekapselt und fällt
  einzeln aus (`n/a` statt erfundener Zahlen)
- `ui/` — Frontend, Vanilla TypeScript ohne Framework und ohne Bundler,
  JetBrains Mono lokal eingebettet
