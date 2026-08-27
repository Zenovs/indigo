//! indigo — natives GTK3-Widget. Ein Prozess: Sampler-Thread liest die
//! Systemwerte, der GTK-Mainloop zeichnet mit Cairo. Ticks ohne sichtbare
//! Änderung werden übersprungen (Anzeige-Signatur), Animationen laufen mit
//! wenigen Schritten nur solange nötig.

mod autostart;
mod fans;
mod monitor;
mod processes;
mod settings;
mod state;
mod tray;
mod ui {
    pub mod render;
}
mod updater;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};


use gtk::prelude::*;

use state::{App, BarId, FanTarget, Hit};
use ui::render;

const FONT_REGULAR: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");
const FONT_MEDIUM: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Medium.ttf");

fn data_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
            home.join(".local/share")
        })
}

/// eingebettete schrift in den fontconfig-nutzerpfad legen, damit pango
/// "JetBrains Mono" findet — ohne systemweite installation
fn ensure_fonts() {
    let dir = data_dir().join("fonts/indigo");
    let _ = std::fs::create_dir_all(&dir);
    for (name, bytes) in [
        ("JetBrainsMono-Regular.ttf", FONT_REGULAR),
        ("JetBrainsMono-Medium.ttf", FONT_MEDIUM),
    ] {
        let path = dir.join(name);
        if !path.exists() {
            let _ = std::fs::write(&path, bytes);
        }
    }
}

fn main() {
    // GNOME ignoriert always-on-top für native Wayland-Clients; über
    // XWayland funktioniert es. Nur setzen, wenn nichts anderes erzwungen ist.
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var_os("GDK_BACKEND").is_none() {
        std::env::set_var("GDK_BACKEND", "x11");
    }
    ensure_fonts();

    let store = Arc::new(settings::SettingsStore::load());
    let initial = store.get();

    // beim allerersten Start Autostart aktivieren; sonst Eintrag auffrischen,
    // falls der Binary-Pfad gewechselt hat
    if initial.autostart.is_none() {
        match autostart::enable() {
            Ok(()) => store.update(|s| s.autostart = Some(true)),
            Err(e) => eprintln!("autostart: {e}"),
        }
    } else if autostart::is_enabled() {
        // nur reparieren, wenn das eingetragene binary nicht mehr existiert —
        // sonst würde jeder start eines alten binaries den eintrag kapern
        let stale = autostart::exec_path().map(|p| !p.exists()).unwrap_or(true);
        if stale {
            let _ = autostart::enable();
        }
    }

    // zweite instanz still beenden — sonst kämpfen zwei prozesse um
    // settings, updater-downloads und lüfter-holds
    let Some(_instance_lock) = acquire_instance_lock() else {
        eprintln!("indigo läuft bereits");
        return;
    };

    if gtk::init().is_err() {
        eprintln!("gtk konnte nicht initialisiert werden");
        return;
    }

    let interval = Arc::new(AtomicU64::new(initial.interval_ms.clamp(250, 60_000)));
    let force_emit = Arc::new(AtomicBool::new(false));
    let fanctl = Arc::new(fans::FanControl::new());
    let topprocs = Arc::new(processes::TopProcs::new());
    let app = Rc::new(RefCell::new(App::new(initial.collapsed)));

    // --- fenster ---------------------------------------------------------
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_title("indigo");
    window.set_decorated(false);
    window.set_keep_above(true);
    window.set_skip_taskbar_hint(true);
    window.set_skip_pager_hint(true);
    window.set_accept_focus(false);
    window.set_app_paintable(true);
    window.set_type_hint(gdk::WindowTypeHint::Utility);
    window.set_default_size(render::WINDOW_W, 300);
    if let Some(visual) = WidgetExt::screen(&window).and_then(|s| s.rgba_visual()) {
        window.set_visual(Some(&visual));
    }

    let area = gtk::DrawingArea::new();
    area.add_events(
        gdk::EventMask::BUTTON_PRESS_MASK
            | gdk::EventMask::BUTTON_RELEASE_MASK
            | gdk::EventMask::POINTER_MOTION_MASK
            | gdk::EventMask::LEAVE_NOTIFY_MASK,
    );
    window.add(&area);

    // fenstergrösse dem inhalt nachführen
    let last_size = Rc::new(Cell::new((0i32, 0i32)));
    let sync_size = {
        let app = app.clone();
        let window = window.clone();
        let last_size = last_size.clone();
        move || {
            let h = (render::panel_height(&app.borrow()) + 2.0 * render::MARGIN).ceil() as i32;
            let size = (render::WINDOW_W, h);
            if last_size.get() != size {
                last_size.set(size);
                window.resize(size.0, size.1);
            }
        }
    };

    // --- zeichnen --------------------------------------------------------
    {
        let app = app.clone();
        let bg_cache = Rc::new(RefCell::new(render::BgCache::default()));
        area.connect_draw(move |widget, cr| {
            cr.set_operator(cairo::Operator::Source);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
            let _ = cr.paint();
            cr.set_operator(cairo::Operator::Over);
            let (hits, _) = {
                let st = app.borrow();
                render::draw(cr, widget.pango_context(), &st, &mut bg_cache.borrow_mut())
            };
            app.borrow_mut().hits = hits;
            glib::Propagation::Proceed
        });
    }

    // --- animations-takt (läuft nur solange animationen aktiv sind) ------
    let anim_running = Rc::new(Cell::new(false));
    let kick_anims = {
        let app = app.clone();
        let area = area.clone();
        let anim_running = anim_running.clone();
        move || {
            if anim_running.get() || app.borrow().anims.is_empty() {
                return;
            }
            anim_running.set(true);
            let app = app.clone();
            let area = area.clone();
            let anim_running = anim_running.clone();
            glib::timeout_add_local(Duration::from_millis(state::ANIM_STEP_MS as u64), move || {
                let more = app.borrow_mut().tick_anims();
                area.queue_draw();
                if more {
                    glib::ControlFlow::Continue
                } else {
                    anim_running.set(false);
                    glib::ControlFlow::Break
                }
            });
        }
    };

    // --- sampler-thread --------------------------------------------------
    let (stats_tx, stats_rx) = glib::MainContext::channel::<monitor::Stats>(glib::Priority::DEFAULT);
    {
        let interval = interval.clone();
        let force = force_emit.clone();
        let fanctl = fanctl.clone();
        std::thread::spawn(move || {
            let mut sampler = monitor::Sampler::new();
            let mut last_sig = String::new();
            let mut skipped: u32 = 0;
            loop {
                std::thread::sleep(Duration::from_millis(interval.load(Ordering::Relaxed)));
                let mut stats = sampler.sample();
                stats.fans = fanctl.read();
                let sig = stats.display_signature();
                let forced = force.swap(false, Ordering::Relaxed);
                if sig == last_sig && skipped < 8 && !forced {
                    skipped += 1;
                    continue;
                }
                last_sig = sig;
                skipped = 0;
                if stats_tx.send(stats).is_err() {
                    return;
                }
            }
        });
    }
    {
        let app = app.clone();
        let area = area.clone();
        let sync_size = sync_size.clone();
        let kick_anims = kick_anims.clone();
        stats_rx.attach(None, move |mut stats| {
            {
                let mut st = app.borrow_mut();
                for (id, value) in [
                    (BarId::Cpu, stats.cpu),
                    (BarId::Ram, stats.ram),
                    (BarId::Disk, stats.disk),
                    (BarId::Gpu, stats.gpu),
                ] {
                    match value {
                        Some(v) => st.set_bar_target(id, v as f64),
                        None => st.set_bar_immediate(id, 0.0),
                    }
                }
                // während eines holds (drag / frischer set) bleiben mode und
                // pct der anzeige stehen — der nächste freie tick korrigiert
                if let Some(prev) = &st.stats {
                    let held: Vec<usize> = stats
                        .fans
                        .iter()
                        .enumerate()
                        .filter(|(_, f)| st.fan_held(&f.id))
                        .map(|(i, _)| i)
                        .collect();
                    for i in held {
                        let id = stats.fans[i].id.clone();
                        if let Some(old) = prev.fans.iter().find(|f| f.id == id) {
                            stats.fans[i].auto_mode = old.auto_mode;
                            stats.fans[i].pct = old.pct;
                        }
                    }
                }
                let mut group_sum = 0.0;
                let mut group_n = 0usize;
                for fan in &stats.fans {
                    if let (Some(pct), Some(_)) = (fan.pct, fan.auto_mode) {
                        group_sum += pct as f64;
                        group_n += 1;
                        if !st.fan_held(&fan.id) {
                            st.set_bar_target(BarId::Fan(fan.id.clone()), pct as f64);
                        }
                    }
                }
                if group_n > 1 && !st.fan_held("__group__") {
                    st.set_bar_target(BarId::FanGroup, group_sum / group_n as f64);
                }
                st.stats = Some(stats);
            }
            sync_size();
            kick_anims();
            area.queue_draw();
            glib::ControlFlow::Continue
        });
    }

    // --- top-listen ------------------------------------------------------
    let (top_tx, top_rx) = glib::MainContext::channel::<(&'static str, processes::TopList)>(glib::Priority::DEFAULT);
    let fetch_top = {
        let topprocs = topprocs.clone();
        move |kind: &'static str, tx: glib::Sender<(&'static str, processes::TopList)>| {
            let topprocs = topprocs.clone();
            std::thread::spawn(move || {
                if let Ok(list) = topprocs.top(kind) {
                    let _ = tx.send((kind, list));
                }
            });
        }
    };
    {
        let app = app.clone();
        let area = area.clone();
        let sync_size = sync_size.clone();
        top_rx.attach(None, move |(kind, list)| {
            {
                let mut st = app.borrow_mut();
                match &mut st.dropdown {
                    Some(dd) if dd.kind == kind => dd.list = Some(list),
                    _ => return glib::ControlFlow::Continue,
                }
            }
            sync_size();
            area.queue_draw();
            glib::ControlFlow::Continue
        });
    }

    // --- lüfter-aktionen -------------------------------------------------
    // sysfs-schreibzugriffe sind schnell; direkt im mainloop ausführen und
    // die anzeige optimistisch nachziehen — der nächste tick korrigiert
    let apply_fan = {
        let fanctl = fanctl.clone();
        let app = app.clone();
        let force = force_emit.clone();
        move |target: &FanTarget, pct: Option<f64>| {
            let ids: Vec<String> = {
                let st = app.borrow();
                match target {
                    FanTarget::One(id) => vec![id.clone()],
                    FanTarget::Group => st
                        .stats
                        .as_ref()
                        .map(|s| {
                            s.fans
                                .iter()
                                .filter(|f| f.pct.is_some() && f.auto_mode.is_some())
                                .map(|f| f.id.clone())
                                .collect()
                        })
                        .unwrap_or_default(),
                }
            };
            let mut applied: Vec<String> = Vec::new();
            let mut denied: Vec<String> = Vec::new();
            for id in &ids {
                let result = match pct {
                    Some(p) => fanctl.set_manual(id, p as f32),
                    None => fanctl.set_auto(id),
                };
                match result {
                    Ok(()) => applied.push(id.clone()),
                    Err(e) => {
                        eprintln!("indigo-fan: {e}");
                        denied.push(id.clone());
                    }
                }
            }
            // anzeige: erfolge optimistisch übernehmen, fehlschläge als
            // "denied" markieren — nie so tun, als hätte das setzen geklappt
            let mut st = app.borrow_mut();
            let until = Instant::now() + Duration::from_secs(3);
            if let Some(stats) = &mut st.stats {
                for fan in stats.fans.iter_mut().filter(|f| applied.contains(&f.id)) {
                    match pct {
                        Some(p) => {
                            fan.auto_mode = Some(false);
                            fan.pct = Some(p as f32);
                        }
                        None => fan.auto_mode = Some(true),
                    }
                }
            }
            for id in &applied {
                st.fan_hold.insert(id.clone(), until);
                if let Some(p) = pct {
                    st.set_bar_immediate(BarId::Fan(id.clone()), p);
                }
            }
            for id in &denied {
                st.fan_denied.insert(id.clone(), until);
            }
            if *target == FanTarget::Group {
                if !denied.is_empty() {
                    st.fan_denied.insert("__group__".into(), until);
                }
                if !applied.is_empty() {
                    st.fan_hold.insert("__group__".into(), until);
                    if let Some(p) = pct {
                        st.set_bar_immediate(BarId::FanGroup, p);
                    }
                }
            }
            if !applied.is_empty() {
                force.store(true, Ordering::Relaxed);
            }
        }
    };

    // --- kontextmenü -----------------------------------------------------
    let menu_slot: Rc<RefCell<Option<gtk::Menu>>> = Rc::new(RefCell::new(None));
    let show_menu = {
        let store = store.clone();
        let interval = interval.clone();
        let menu_slot = menu_slot.clone();
        move |time: u32| {
            let menu = gtk::Menu::new();
            let current = interval.load(Ordering::Relaxed);
            for (ms, label) in [(1000u64, "intervall 1 s"), (2000, "intervall 2 s"), (5000, "intervall 5 s")] {
                let item = gtk::CheckMenuItem::with_label(label);
                item.set_active(current == ms);
                let store = store.clone();
                let interval = interval.clone();
                item.connect_activate(move |_| {
                    interval.store(ms, Ordering::Relaxed);
                    store.update(|s| s.interval_ms = ms);
                });
                menu.append(&item);
            }
            menu.append(&gtk::SeparatorMenuItem::new());

            let auto_item = gtk::CheckMenuItem::with_label("autostart");
            auto_item.set_active(autostart::is_enabled());
            {
                let store = store.clone();
                auto_item.connect_activate(move |_| {
                    let enable = !autostart::is_enabled();
                    let result = if enable { autostart::enable() } else { autostart::disable() };
                    match result {
                        Ok(()) => store.update(|s| s.autostart = Some(enable)),
                        Err(e) => eprintln!("autostart: {e}"),
                    }
                });
            }
            menu.append(&auto_item);

            let upd_item = gtk::CheckMenuItem::with_label("auto-update");
            upd_item.set_active(store.get().autoupdate);
            {
                let store = store.clone();
                upd_item.connect_activate(move |_| {
                    store.update(|s| s.autoupdate = !s.autoupdate);
                });
            }
            menu.append(&upd_item);

            menu.append(&gtk::SeparatorMenuItem::new());
            let quit_item = gtk::MenuItem::with_label("beenden");
            {
                let store = store.clone();
                quit_item.connect_activate(move |_| {
                    store.flush();
                    gtk::main_quit();
                });
            }
            menu.append(&quit_item);

            menu.show_all();
            menu.popup_easy(3, time);
            *menu_slot.borrow_mut() = Some(menu);
        }
    };

    // --- eingabe ---------------------------------------------------------
    let refresh_running = Rc::new(Cell::new(false));
    {
        let app = app.clone();
        let area_c = area.clone();
        let window = window.clone();
        let store = store.clone();
        let force = force_emit.clone();
        let sync_size = sync_size.clone();
        let fetch_top = fetch_top.clone();
        let top_tx = top_tx.clone();
        let apply_fan = apply_fan.clone();
        let show_menu = show_menu.clone();
        let refresh_running = refresh_running.clone();
        area.connect_button_press_event(move |_, ev| {
            let (x, y) = ev.position();
            if ev.button() == 3 {
                // laufenden slider-drag committen — das menü schluckt sonst
                // das button-1-release und der drag bliebe hängen
                let drag = app.borrow_mut().drag_fan.take();
                if let Some((target, pct)) = drag {
                    apply_fan(&target, Some(pct));
                }
                show_menu(ev.time());
                return glib::Propagation::Stop;
            }
            if ev.button() != 1 {
                return glib::Propagation::Proceed;
            }
            let hit = app.borrow().hit_at(x, y);
            match hit {
                Some(Hit::Dot) => {
                    {
                        let mut st = app.borrow_mut();
                        st.collapsed = !st.collapsed;
                        st.dropdown = None;
                        let collapsed = st.collapsed;
                        store.update(|s| s.collapsed = collapsed);
                    }
                    force.store(true, Ordering::Relaxed);
                    sync_size();
                    area_c.queue_draw();
                }
                Some(Hit::TopRow(kind)) => {
                    let opened = {
                        let mut st = app.borrow_mut();
                        match &st.dropdown {
                            Some(dd) if dd.kind == kind => {
                                st.dropdown = None;
                                false
                            }
                            _ => {
                                st.dropdown = Some(state::Dropdown { kind, list: None });
                                true
                            }
                        }
                    };
                    if opened {
                        fetch_top(kind, top_tx.clone());
                        // eigener 2s-takt, solange die liste offen ist
                        if !refresh_running.get() {
                            refresh_running.set(true);
                            let app = app.clone();
                            let fetch_top = fetch_top.clone();
                            let top_tx = top_tx.clone();
                            let refresh_running = refresh_running.clone();
                            glib::timeout_add_local(Duration::from_secs(2), move || {
                                let kind = app.borrow().dropdown.as_ref().map(|d| d.kind);
                                match kind {
                                    Some(k) => {
                                        fetch_top(k, top_tx.clone());
                                        glib::ControlFlow::Continue
                                    }
                                    None => {
                                        refresh_running.set(false);
                                        glib::ControlFlow::Break
                                    }
                                }
                            });
                        }
                    }
                    sync_size();
                    area_c.queue_draw();
                }
                Some(Hit::FanMode(target)) => {
                    let make_manual = {
                        let st = app.borrow();
                        let stats = st.stats.as_ref();
                        match (&target, stats) {
                            (FanTarget::One(id), Some(s)) => s
                                .fans
                                .iter()
                                .find(|f| f.id == *id)
                                .map(|f| f.auto_mode != Some(false))
                                .unwrap_or(true),
                            (FanTarget::Group, Some(s)) => s
                                .fans
                                .iter()
                                .filter(|f| f.pct.is_some())
                                .all(|f| f.auto_mode != Some(false)),
                            _ => true,
                        }
                    };
                    match (&target, make_manual) {
                        (FanTarget::Group, true) => {
                            // jeder lüfter friert bei SEINEM aktuellen wert ein —
                            // der mode-wechsel ändert keine drehzahlen
                            let pairs: Vec<(String, f64)> = {
                                let st = app.borrow();
                                st.stats
                                    .as_ref()
                                    .map(|s| {
                                        s.fans
                                            .iter()
                                            .filter(|f| f.pct.is_some())
                                            .map(|f| (f.id.clone(), f.pct.unwrap_or(50.0) as f64))
                                            .collect()
                                    })
                                    .unwrap_or_default()
                            };
                            for (id, own_pct) in &pairs {
                                apply_fan(&FanTarget::One(id.clone()), Some(*own_pct));
                            }
                            if !pairs.is_empty() {
                                let avg =
                                    pairs.iter().map(|(_, p)| p).sum::<f64>() / pairs.len() as f64;
                                let mut st = app.borrow_mut();
                                st.fan_hold
                                    .insert("__group__".into(), Instant::now() + Duration::from_secs(3));
                                st.set_bar_immediate(BarId::FanGroup, avg);
                            }
                        }
                        (FanTarget::One(id), true) => {
                            let own = {
                                let st = app.borrow();
                                st.stats
                                    .as_ref()
                                    .and_then(|s| s.fans.iter().find(|f| f.id == *id))
                                    .and_then(|f| f.pct)
                                    .unwrap_or(50.0) as f64
                            };
                            apply_fan(&target, Some(own));
                        }
                        (_, false) => apply_fan(&target, None),
                    }
                    area_c.queue_draw();
                }
                Some(Hit::FanSlider(target)) => {
                    let pct = slider_pct(x);
                    {
                        let mut st = app.borrow_mut();
                        let ids: Vec<String> = st
                            .stats
                            .as_ref()
                            .map(|s| s.fans.iter().map(|f| f.id.clone()).collect())
                            .unwrap_or_default();
                        st.hold_fan(&target, Instant::now() + Duration::from_secs(3600), &ids);
                        st.drag_fan = Some((target, pct));
                    }
                    area_c.queue_draw();
                }
                Some(Hit::Header) => {
                    window.begin_move_drag(1, ev.root().0 as i32, ev.root().1 as i32, ev.time());
                }
                None => {}
            }
            glib::Propagation::Stop
        });
    }
    {
        let app = app.clone();
        let area_c = area.clone();
        area.connect_motion_notify_event(move |widget, ev| {
            let (x, _y) = ev.position();
            let mut st = app.borrow_mut();
            if let Some((target, _)) = &st.drag_fan {
                // release verpasst (grab durch menü o.ä.) -> drag verwerfen
                if !ev.state().contains(gdk::ModifierType::BUTTON1_MASK) {
                    st.drag_fan = None;
                    st.fan_hold.clear();
                    drop(st);
                    area_c.queue_draw();
                    return glib::Propagation::Proceed;
                }
                let target = target.clone();
                st.drag_fan = Some((target, slider_pct(x)));
                drop(st);
                area_c.queue_draw();
                return glib::Propagation::Proceed;
            }
            let hit = st.hit_at(x, ev.position().1);
            let hover = match hit {
                Some(Hit::Dot) => Some(Hit::Dot),
                Some(Hit::FanMode(t)) => Some(Hit::FanMode(t)),
                Some(h @ (Hit::TopRow(_) | Hit::FanSlider(_) | Hit::Header)) => Some(h),
                None => None,
            };
            if st.hover != hover {
                st.hover = hover.clone();
                drop(st);
                set_cursor(widget, &hover);
                area_c.queue_draw();
            }
            glib::Propagation::Proceed
        });
    }
    {
        let app = app.clone();
        let area_c = area.clone();
        let apply_fan = apply_fan.clone();
        area.connect_button_release_event(move |_, _| {
            let drag = app.borrow_mut().drag_fan.take();
            if let Some((target, pct)) = drag {
                apply_fan(&target, Some(pct));
                area_c.queue_draw();
            }
            glib::Propagation::Proceed
        });
    }
    {
        let app = app.clone();
        let area_c = area.clone();
        area.connect_leave_notify_event(move |widget, _| {
            let mut st = app.borrow_mut();
            if st.hover.is_some() && st.drag_fan.is_none() {
                st.hover = None;
                drop(st);
                set_cursor(widget, &None);
                area_c.queue_draw();
            }
            glib::Propagation::Proceed
        });
    }

    // --- tray ------------------------------------------------------------
    let (tray_tx, tray_rx) = glib::MainContext::channel::<tray::TrayMsg>(glib::Priority::DEFAULT);
    tray::spawn(tray_tx);
    {
        let window = window.clone();
        let store = store.clone();
        tray_rx.attach(None, move |msg| {
            match msg {
                tray::TrayMsg::ToggleVisible => {
                    if window.is_visible() {
                        window.hide();
                    } else {
                        window.show();
                    }
                }
                tray::TrayMsg::Quit => {
                    store.flush();
                    gtk::main_quit();
                }
            }
            glib::ControlFlow::Continue
        });
    }

    // --- auto-update -----------------------------------------------------
    if store.get().autoupdate {
        let (upd_tx, upd_rx) = glib::MainContext::channel::<updater::UpdateEvent>(glib::Priority::DEFAULT);
        updater::spawn_check(upd_tx);
        let store = store.clone();
        let started = Instant::now();
        upd_rx.attach(None, move |ev| {
            match ev {
                updater::UpdateEvent::ReadyRestart(path) => {
                    if started.elapsed() < Duration::from_secs(90) {
                        eprintln!("indigo-update: neue version installiert, starte neu");
                        store.flush();
                        restart_self(&path);
                    } else {
                        eprintln!("indigo-update: neue version aktiv ab dem nächsten start");
                    }
                }
                updater::UpdateEvent::InstalledAt(path) => {
                    eprintln!(
                        "indigo-update: neue version installiert nach {} (aktiv ab dem nächsten start)",
                        path.display()
                    );
                }
            }
            glib::ControlFlow::Continue
        });
    }

    // --- position & lebenszyklus -----------------------------------------
    if let (Some(x), Some(y)) = (initial.x, initial.y) {
        window.move_(x, y);
    }
    {
        let store = store.clone();
        window.connect_configure_event(move |win, _| {
            let (x, y) = win.position();
            store.update_throttled(|s| {
                s.x = Some(x);
                s.y = Some(y);
            });
            false
        });
    }
    {
        let store = store.clone();
        window.connect_delete_event(move |_, _| {
            store.flush();
            gtk::main_quit();
            glib::Propagation::Proceed
        });
    }

    sync_size();
    window.show_all();
    if let (Some(x), Some(y)) = (initial.x, initial.y) {
        window.move_(x, y); // mancher wm ignoriert move vor dem mapping
    }

    gtk::main();
    store.flush();
}

/// x-position im fenster -> slider-prozent (0..100)
fn slider_pct(x: f64) -> f64 {
    let x0 = render::MARGIN + 12.0;
    let w = render::PANEL_W - 24.0;
    (((x - x0) / w) * 100.0).clamp(0.0, 100.0)
}

fn set_cursor(widget: &gtk::DrawingArea, hover: &Option<Hit>) {
    let Some(gdk_window) = widget.window() else { return };
    let name = match hover {
        Some(Hit::Header) => Some("grab"),
        Some(Hit::Dot) | Some(Hit::TopRow(_)) | Some(Hit::FanMode(_)) | Some(Hit::FanSlider(_)) => {
            Some("pointer")
        }
        None => None,
    };
    let cursor = name.and_then(|n| gdk::Cursor::from_name(&gdk_window.display(), n));
    gdk_window.set_cursor(cursor.as_ref());
}

/// prozess durch das frisch installierte binary ersetzen. der pfad kommt
/// vom updater — current_exe() liefert nach dem rename "<pfad> (deleted)"
fn restart_self(path: &std::path::Path) {
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(path).exec();
    eprintln!("indigo-update: neustart fehlgeschlagen: {err}");
}

/// exklusiver flock auf eine lock-datei; None wenn schon eine instanz läuft
fn acquire_instance_lock() -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join("indigo.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&path)
        .ok()?;
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if ret == 0 {
        Some(file)
    } else {
        None
    }
}
