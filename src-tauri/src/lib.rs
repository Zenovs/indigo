mod autostart;
mod fans;
mod monitor;
mod processes;
mod settings;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::menu::{CheckMenuItem, ContextMenu, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager, PhysicalPosition, WindowEvent};

use settings::SettingsStore;

pub struct IntervalMs(pub Arc<AtomicU64>);

/// erzwingt den nächsten Emit trotz unveränderter Anzeige-Signatur
/// (z.B. nach dem Aufklappen, damit sofort frische Werte stehen)
pub struct ForceEmit(pub Arc<AtomicBool>);

struct MenuState {
    context: Menu<tauri::Wry>,
    int_items: Vec<(u64, CheckMenuItem<tauri::Wry>)>,
    autostart_item: CheckMenuItem<tauri::Wry>,
}

/// pct gesetzt -> manueller PWM-Wert; pct weggelassen -> Firmware-Automatik.
#[tauri::command]
fn set_fan(
    fanctl: tauri::State<'_, Arc<fans::FanControl>>,
    id: String,
    pct: Option<f32>,
) -> Result<(), String> {
    match pct {
        Some(p) => fanctl.set_manual(&id, p),
        None => fanctl.set_auto(&id),
    }
}

/// Top-10-Prozesse nach cpu, ram oder gpu; async, damit der kurze
/// Doppel-Refresh für CPU-Deltas den Hauptthread nicht blockiert.
#[tauri::command]
async fn top_processes(
    procs: tauri::State<'_, Arc<processes::TopProcs>>,
    kind: String,
) -> Result<processes::TopList, String> {
    procs.top(&kind)
}

#[tauri::command]
fn get_settings(store: tauri::State<'_, Arc<SettingsStore>>) -> settings::Settings {
    store.get()
}

#[tauri::command]
fn set_collapsed(
    store: tauri::State<'_, Arc<SettingsStore>>,
    force: tauri::State<'_, ForceEmit>,
    collapsed: bool,
) {
    store.update(|s| s.collapsed = collapsed);
    force.0.store(true, Ordering::Relaxed);
}

/// Natives Kontextmenü am Mauszeiger öffnen (Rechtsklick im Widget).
#[tauri::command]
fn context_menu(window: tauri::Window, menu_state: tauri::State<'_, MenuState>) {
    let _ = menu_state.context.popup(window);
}

fn apply_interval(
    app: &tauri::AppHandle,
    ms: u64,
) {
    let interval = app.state::<IntervalMs>();
    interval.0.store(ms, Ordering::Relaxed);
    let store = app.state::<Arc<SettingsStore>>();
    store.update(|s| s.interval_ms = ms);
    let menu_state = app.state::<MenuState>();
    for (item_ms, item) in &menu_state.int_items {
        let _ = item.set_checked(*item_ms == ms);
    }
}

fn toggle_autostart(app: &tauri::AppHandle) {
    let store = app.state::<Arc<SettingsStore>>();
    let enable = !autostart::is_enabled();
    let result = if enable {
        autostart::enable()
    } else {
        autostart::disable()
    };
    match result {
        Ok(()) => store.update(|s| s.autostart = Some(enable)),
        Err(e) => eprintln!("autostart: {e}"),
    }
    let menu_state = app.state::<MenuState>();
    let _ = menu_state.autostart_item.set_checked(autostart::is_enabled());
}

fn toggle_visibility(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let visible = window.is_visible().unwrap_or(true);
        let _ = if visible { window.hide() } else { window.show() };
    }
}

pub fn run() {
    // GNOME ignoriert always-on-top für native Wayland-Clients; über
    // XWayland (x11-Backend) funktioniert es. Nur setzen, wenn der Nutzer
    // nichts anderes erzwungen hat.
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var_os("GDK_BACKEND").is_none()
    {
        std::env::set_var("GDK_BACKEND", "x11");
    }
    // bekannter workaround für webkitgtk auf nvidia: der dmabuf-renderer
    // landet dort in teuren software-pfaden
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    let store = Arc::new(SettingsStore::load());
    let initial = store.get();

    let interval = Arc::new(AtomicU64::new(initial.interval_ms.clamp(250, 60_000)));
    let sampler_interval = interval.clone();
    let force_emit = Arc::new(AtomicBool::new(false));
    let sampler_force = force_emit.clone();
    let fanctl = Arc::new(fans::FanControl::new());
    let sampler_fanctl = fanctl.clone();
    let store_for_events = store.clone();

    tauri::Builder::default()
        .manage(IntervalMs(interval))
        .manage(ForceEmit(force_emit))
        .manage(fanctl)
        .manage(Arc::new(processes::TopProcs::new()))
        .manage(store)
        .invoke_handler(tauri::generate_handler![
            set_fan,
            top_processes,
            get_settings,
            set_collapsed,
            context_menu
        ])
        .setup(move |app| {
            let store = app.state::<Arc<SettingsStore>>().inner().clone();
            let initial = store.get();

            // beim allerersten Start Autostart aktivieren (Vorgabe:
            // "startet mit dem System"); danach entscheidet der Nutzer
            if initial.autostart.is_none() {
                match autostart::enable() {
                    Ok(()) => store.update(|s| s.autostart = Some(true)),
                    Err(e) => eprintln!("autostart: {e}"),
                }
            } else if autostart::is_enabled() {
                // Eintrag auffrischen, falls der Binary-Pfad gewechselt hat
                let _ = autostart::enable();
            }

            // gespeicherte Position wiederherstellen
            if let Some(window) = app.get_webview_window("main") {
                if let (Some(x), Some(y)) = (initial.x, initial.y) {
                    let _ = window.set_position(PhysicalPosition::new(x, y));
                }
            }

            // Kontextmenü: Intervall 1/2/5 s, Autostart, Beenden
            let current_ms = initial.interval_ms;
            let mut int_items = Vec::new();
            for (ms, label) in [
                (1000u64, "intervall 1 s"),
                (2000, "intervall 2 s"),
                (5000, "intervall 5 s"),
            ] {
                let item = CheckMenuItem::with_id(
                    app,
                    format!("interval-{ms}"),
                    label,
                    true,
                    ms == current_ms,
                    None::<&str>,
                )?;
                int_items.push((ms, item));
            }
            let autostart_item = CheckMenuItem::with_id(
                app,
                "autostart",
                "autostart",
                true,
                autostart::is_enabled(),
                None::<&str>,
            )?;
            let quit_item = MenuItem::with_id(app, "quit", "beenden", true, None::<&str>)?;
            let context = Menu::with_items(
                app,
                &[
                    &int_items[0].1,
                    &int_items[1].1,
                    &int_items[2].1,
                    &PredefinedMenuItem::separator(app)?,
                    &autostart_item,
                    &PredefinedMenuItem::separator(app)?,
                    &quit_item,
                ],
            )?;
            app.manage(MenuState {
                context,
                int_items,
                autostart_item,
            });

            // Tray: Ein-/Ausblenden und Beenden (Linux-Trays sind menübasiert)
            let tray_toggle =
                MenuItem::with_id(app, "tray-toggle", "ein-/ausblenden", true, None::<&str>)?;
            let tray_quit = MenuItem::with_id(app, "tray-quit", "beenden", true, None::<&str>)?;
            let tray_menu = Menu::with_items(app, &[&tray_toggle, &tray_quit])?;
            let mut tray = TrayIconBuilder::with_id("indigo")
                .menu(&tray_menu)
                .tooltip("indigo");
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;

            // Sampler-Thread: liest alle Werte und pusht ein Event pro Tick
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let mut sampler = monitor::Sampler::new();
                let mut last_sig = String::new();
                let mut skipped: u32 = 0;
                loop {
                    std::thread::sleep(Duration::from_millis(
                        sampler_interval.load(Ordering::Relaxed),
                    ));
                    let mut stats = sampler.sample();
                    stats.fans = sampler_fanctl.read();
                    // ticks ohne sichtbare änderung überspringen: das spart
                    // eval, dom-arbeit und repaint im webview. alle 8 ticks
                    // trotzdem senden, damit ein frisch geladenes frontend
                    // sicher werte bekommt
                    let sig = stats.display_signature();
                    let forced = sampler_force.swap(false, Ordering::Relaxed);
                    if sig == last_sig && skipped < 8 && !forced {
                        skipped += 1;
                        continue;
                    }
                    last_sig = sig;
                    skipped = 0;
                    let _ = handle.emit("stats", &stats);
                }
            });
            Ok(())
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
            "interval-1000" => apply_interval(app, 1000),
            "interval-2000" => apply_interval(app, 2000),
            "interval-5000" => apply_interval(app, 5000),
            "autostart" => toggle_autostart(app),
            "tray-toggle" => toggle_visibility(app),
            "quit" | "tray-quit" => {
                app.state::<Arc<SettingsStore>>().flush();
                app.exit(0);
            }
            _ => {}
        })
        .on_window_event(move |window, event| match event {
            WindowEvent::Moved(pos) => {
                let (x, y) = (pos.x, pos.y);
                store_for_events.update_throttled(|s| {
                    s.x = Some(x);
                    s.y = Some(y);
                });
            }
            WindowEvent::Destroyed => {
                let _ = window; // position ist bereits im store
                store_for_events.flush();
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("error while building indigo")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                app.state::<Arc<SettingsStore>>().flush();
            }
        });
}
