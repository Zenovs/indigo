//! Fensterstapelung: immer im Vordergrund oder ein normales Fenster, das
//! andere Fenster verdecken dürfen.
//!
//! «Immer oben» läuft normalerweise über EWMH (_NET_WM_STATE_ABOVE). X11-
//! Window-Manager, GNOME und KDE setzen das auch für XWayland-Fenster um.
//! wlroots-Compositors wie labwc melden ABOVE zwar als unterstützt,
//! ignorieren die Anfrage aber. Dort wird das Fenster im Vordergrund-Modus
//! override-redirect: XWayland-Fenster ohne Window-Manager liegen über allen
//! verwalteten Fenstern. Verschieben muss indigo es dort selbst (main.rs) und
//! dabei im nutzbaren Bereich halten — darauf achtet kein Window-Manager mehr.

use std::path::PathBuf;

use gtk::prelude::*;

use crate::ui::render;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    /// _NET_WM_STATE_ABOVE über den Window-Manager
    Ewmh,
    /// override-redirect, am Window-Manager vorbei
    OverrideRedirect,
}

/// Wer am Wayland-Socket lauscht.
#[derive(Debug, PartialEq)]
enum Compositor {
    /// Socket nicht erreichbar: kein laufender Wayland-Compositor, also eine
    /// echte X11-Sitzung mit veraltetem WAYLAND_DISPLAY
    Unreachable,
    /// erreichbar, aber der Prozess lässt sich nicht ermitteln
    Unknown,
    Name(String),
}

/// Methode für die laufende Sitzung; erst nach gtk::init() aufrufen.
pub fn detect() -> Method {
    let x11 = gdk::Display::default()
        .map(|d| d.type_().name() == "GdkX11Display")
        .unwrap_or(false);
    // reines X11: jeder EWMH-WM kann ABOVE. natives Wayland (GDK_BACKEND
    // von aussen erzwungen): override-redirect gibt es dort nicht
    if !x11 || std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Method::Ewmh;
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    method_for(&wayland_compositor(), &desktop)
}

/// EWMH nur, wo der Compositor ABOVE nachweislich umsetzt: GNOME (mutter)
/// und KDE (kwin; der Socket gehört dort kwin_wayland_wrapper). Alles andere
/// bekommt override-redirect — das liegt überall oben, auch wo ABOVE
/// ebenfalls ginge.
fn method_for(compositor: &Compositor, desktop: &str) -> Method {
    let ewmh = match compositor {
        Compositor::Unreachable => true,
        Compositor::Name(name) => {
            // nixos-wrapper heissen «.gnome-shell-wrapped» (comm gekürzt)
            let name = name.trim_start_matches('.');
            name.starts_with("gnome-shell") || name.starts_with("kwin")
        }
        // prozess unbekannt: auf XDG_CURRENT_DESKTOP ausweichen
        Compositor::Unknown => desktop
            .split(':')
            .any(|d| d.eq_ignore_ascii_case("gnome") || d.eq_ignore_ascii_case("kde")),
    };
    if ewmh {
        Method::Ewmh
    } else {
        Method::OverrideRedirect
    }
}

/// Prozessname des Wayland-Compositors, über SO_PEERCRED am Wayland-Socket.
fn wayland_compositor() -> Compositor {
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;

    let Some(display) = std::env::var_os("WAYLAND_DISPLAY").map(PathBuf::from) else {
        return Compositor::Unreachable;
    };
    let socket = if display.is_absolute() {
        display
    } else {
        match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(dir) => PathBuf::from(dir).join(display),
            None => return Compositor::Unreachable,
        }
    };
    let Ok(stream) = UnixStream::connect(socket) else {
        return Compositor::Unreachable;
    };
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let ret = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if ret != 0 || cred.pid <= 0 {
        return Compositor::Unknown;
    }
    match std::fs::read_to_string(format!("/proc/{}/comm", cred.pid)) {
        Ok(comm) => Compositor::Name(comm.trim().to_string()),
        Err(_) => Compositor::Unknown,
    }
}

/// Stapelung umsetzen. Für override-redirect muss das Fenster realisiert
/// sein; ist es schon sichtbar, wird es kurz neu gemappt — Window-Manager
/// und Compositor werten override-redirect nur beim Mappen aus. `size` ist
/// die Inhaltsgrösse, auf die das Fenster dabei zurückgesetzt wird.
pub fn apply(window: &gtk::Window, method: Method, on_top: bool, size: (i32, i32)) {
    match method {
        Method::Ewmh => window.set_keep_above(on_top),
        Method::OverrideRedirect => {
            let Some(gdk_window) = window.window() else { return };
            let visible = window.is_visible();
            let (x, y) = window.position();
            if visible {
                window.hide();
            }
            gdk_window.set_override_redirect(on_top);
            // eine vom wm aufgezwungene grösse (maximiert, eingerastet) käme
            // sonst als unsichtbare, klickfangende fläche mit
            window.unmaximize();
            if size.0 > 0 && size.1 > 0 {
                window.resize(size.0, size.1);
            }
            if visible {
                // vor dem mappen setzen (PPosition für den wm) und danach
                // noch einmal — mancher wm ignoriert move vor dem mapping
                window.move_(x, y);
                window.show();
                window.move_(x, y);
            }
        }
    }
}

/// Fensterposition so begrenzen, dass die Kopfzeile ganz im nutzbaren
/// Bereich des nächstgelegenen Monitors liegt — sonst liesse sich das
/// Widget unter eine Leiste oder nach einem Monitorwechsel ganz aus dem Bild
/// schieben, ohne Griff zum Zurückholen.
pub fn clamp_position(x: i32, y: i32) -> (i32, i32) {
    clamp_into(&usable_areas(), x, y)
}

/// Startposition rechts im nutzbaren Bereich des Hauptmonitors, das Panel
/// unterhalb von Titel- und Werkzeugleiste maximierter Fenster.
pub fn default_position() -> Option<(i32, i32)> {
    let display = gdk::Display::default()?;
    let monitor = display.primary_monitor().or_else(|| display.monitor(0))?;
    let geometry = monitor.geometry();
    let area = net_workarea()
        .and_then(|w| geometry.intersect(&w))
        .unwrap_or(geometry);
    let margin = render::MARGIN as i32;
    Some((
        area.x() + area.width() - render::WINDOW_W - 8,
        area.y() + 120 - margin,
    ))
}

/// Monitorflächen ohne Leisten, als (x, y, breite, höhe).
fn usable_areas() -> Vec<(i32, i32, i32, i32)> {
    let Some(display) = gdk::Display::default() else {
        return Vec::new();
    };
    let workarea = net_workarea();
    (0..display.n_monitors())
        .filter_map(|i| display.monitor(i))
        .map(|monitor| {
            let g = monitor.geometry();
            let a = workarea.as_ref().and_then(|w| g.intersect(w)).unwrap_or(g);
            (a.x(), a.y(), a.width(), a.height())
        })
        .collect()
}

/// _NET_WORKAREA direkt von der Wurzel lesen: gdk wertet es nur aus, wenn
/// der Window-Manager es in _NET_SUPPORTED führt — wlroots setzt es, führt
/// es aber nicht.
fn net_workarea() -> Option<gdk::Rectangle> {
    let root = gdk::Screen::default()?.root_window()?;
    let (_, format, data) = gdk::property_get(
        &root,
        &gdk::Atom::intern("_NET_WORKAREA"),
        &gdk::Atom::intern("CARDINAL"),
        0,
        16, // die ersten vier 32-bit-werte (x, y, breite, höhe), in bytes
        0,
    )?;
    // format 32 liefert gdk wie xlib als c_long-array
    let step = std::mem::size_of::<libc::c_long>();
    if format != 32 {
        return None;
    }
    let value = |i: usize| -> Option<i32> {
        let chunk = data.get(i * step..(i + 1) * step)?;
        match step {
            8 => Some(i64::from_ne_bytes(chunk.try_into().ok()?) as i32),
            4 => Some(i32::from_ne_bytes(chunk.try_into().ok()?)),
            _ => None,
        }
    };
    let (x, y, w, h) = (value(0)?, value(1)?, value(2)?, value(3)?);
    (w > 0 && h > 0).then(|| gdk::Rectangle::new(x, y, w, h))
}

/// Kopfzeile (Panelbreite × HEADER_H, um MARGIN ins Fenster versetzt) ganz
/// in den nächstgelegenen der Bereiche schieben; ohne Bereiche unverändert.
fn clamp_into(areas: &[(i32, i32, i32, i32)], x: i32, y: i32) -> (i32, i32) {
    let margin = render::MARGIN as i32;
    let (w, h) = (render::PANEL_W as i32, render::HEADER_H as i32);
    areas
        .iter()
        .map(|&(ax, ay, aw, ah)| {
            let cx = x.clamp(ax - margin, (ax + aw - w - margin).max(ax - margin));
            let cy = y.clamp(ay - margin, (ay + ah - h - margin).max(ay - margin));
            (cx, cy)
        })
        .min_by_key(|&(cx, cy)| (cx - x).abs() + (cy - y).abs())
        .unwrap_or((x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methode_je_compositor() {
        let name = |n: &str| Compositor::Name(n.into());
        // compositor bekannt: nur gnome-shell und kwin setzen ABOVE um
        assert_eq!(method_for(&name("gnome-shell"), ""), Method::Ewmh);
        assert_eq!(method_for(&name(".gnome-shell-wr"), ""), Method::Ewmh);
        assert_eq!(method_for(&name("kwin_wayland"), "KDE"), Method::Ewmh);
        assert_eq!(method_for(&name("kwin_wayland_wr"), "KDE"), Method::Ewmh);
        assert_eq!(method_for(&name("labwc"), "labwc:wlroots"), Method::OverrideRedirect);
        assert_eq!(method_for(&name("sway"), "sway"), Method::OverrideRedirect);
        // budgie 10.10 läuft auf labwc, meldet sich aber als «Budgie:GNOME»
        assert_eq!(method_for(&name("labwc"), "Budgie:GNOME"), Method::OverrideRedirect);

        // kein compositor erreichbar: echte x11-sitzung
        assert_eq!(method_for(&Compositor::Unreachable, "XFCE"), Method::Ewmh);

        // compositor unbekannt: XDG_CURRENT_DESKTOP entscheidet
        assert_eq!(method_for(&Compositor::Unknown, "ubuntu:GNOME"), Method::Ewmh);
        assert_eq!(method_for(&Compositor::Unknown, "KDE"), Method::Ewmh);
        assert_eq!(method_for(&Compositor::Unknown, "labwc:wlroots"), Method::OverrideRedirect);
        assert_eq!(method_for(&Compositor::Unknown, ""), Method::OverrideRedirect);
    }

    #[test]
    fn kopfzeile_bleibt_erreichbar() {
        let m = render::MARGIN as i32;
        let w = render::PANEL_W as i32;
        let h = render::HEADER_H as i32;
        // wie hier: 1920x1200, oben 40 px leiste
        let pi = [(0, 40, 1920, 1160)];
        assert_eq!(clamp_into(&pi, 1200, 270), (1200, 270), "drinnen bleibt alles");
        assert_eq!(clamp_into(&pi, 1200, -26), (1200, 40 - m), "nicht unter die leiste");
        assert_eq!(clamp_into(&pi, 1900, 270), (1920 - w - m, 270), "nicht rechts hinaus");
        assert_eq!(clamp_into(&pi, -500, 1500), (-m, 1200 - h - m), "ecke unten links");

        // zwei monitore nebeneinander: der nächstgelegene gewinnt
        let zwei = [(0, 0, 1920, 1080), (1920, 0, 1280, 1024)];
        assert_eq!(clamp_into(&zwei, 3300, 100), (1920 + 1280 - w - m, 100));
        assert_eq!(clamp_into(&zwei, 1000, 1100), (1000, 1080 - h - m));

        // schmaler als das panel: wenigstens links bündig
        assert_eq!(clamp_into(&[(0, 0, 200, 600)], 50, 50), (-m, 50));
        // ohne monitorinfo unverändert
        assert_eq!(clamp_into(&[], 5000, -5000), (5000, -5000));
    }
}
