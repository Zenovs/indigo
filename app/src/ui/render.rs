//! Zeichnet das Panel mit Cairo/Pango — pixelgenau nach der Design-Vorlage
//! der Web-Version. Ein Pass rechnet Layout UND sammelt Hit-Regionen ein;
//! mit cr=None wird nur gemessen (Fensterhöhe).

use cairo::Context;
use pango::prelude::*;

use crate::state::{App, BarId, FanTarget, Hit, Rect};

pub const PANEL_W: f64 = 280.0;
pub const MARGIN: f64 = 32.0; // schattenrand um das panel
pub const WINDOW_W: i32 = (PANEL_W + 2.0 * MARGIN) as i32;

const PAD_X: f64 = 12.0;
const GAP: f64 = 11.0;
const HEADER_H: f64 = 28.0;
const HEADER_H_COLLAPSED: f64 = 40.0;
const LINE_H: f64 = 15.0;
const BASELINE: f64 = 11.5; // baseline innerhalb einer 15px-zeile
const BAR_GAP: f64 = 4.0;
const BAR_H: f64 = 3.0;
const RADIUS: f64 = 10.0;

// farben (srgb; oklch-werte der vorlage konvertiert)
const BG: (f64, f64, f64, f64) = (16.0 / 255.0, 16.0 / 255.0, 38.0 / 255.0, 0.82);
const FG: (f64, f64, f64) = (217.0 / 255.0, 218.0 / 255.0, 230.0 / 255.0);
const INDIGO: (f64, f64, f64) = (0.4179, 0.4344, 0.9112); // oklch(0.6 0.18 278)
const AMBER: (f64, f64, f64) = (0.9091, 0.6323, 0.1528); // oklch(0.76 0.15 75)
const RED: (f64, f64, f64) = (0.9008, 0.2649, 0.2401); // oklch(0.62 0.2 27)

fn threshold_color(pct: f64) -> (f64, f64, f64) {
    if pct >= 90.0 {
        RED
    } else if pct >= 70.0 {
        AMBER
    } else {
        INDIGO
    }
}

// --- formatierung (identisch zur web-version) ----------------------------

pub fn fmt_pct(v: f32) -> String {
    format!("{}%", v.round() as i64)
}

pub fn fmt_pct_fine(v: f64) -> String {
    if v >= 10.0 {
        format!("{}%", v.round() as i64)
    } else {
        format!("{v:.1}%")
    }
}

pub fn fmt_mbs(v: f64) -> String {
    if v >= 10.0 {
        format!("{} mb/s", v.round() as i64)
    } else {
        format!("{v:.1} mb/s")
    }
}

pub fn fmt_temp(v: f32) -> String {
    format!("{}°", v.round() as i64)
}

pub fn fmt_watt(v: f64) -> String {
    format!("{} w", v.round() as i64)
}

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

pub fn fmt_bytes(v: f64) -> String {
    if v >= GIB {
        format!("{:.1} gb", v / GIB)
    } else {
        format!("{} mb", (v / (1024.0 * 1024.0)).round() as i64)
    }
}

pub fn fmt_ram_abs(used: u64, total: u64) -> String {
    format!("{:.1}/{:.1} gb", used as f64 / GIB, total as f64 / GIB)
}

pub fn fmt_rpm(v: u32) -> String {
    format!("{v} rpm")
}

// --- text-helfer ----------------------------------------------------------

struct Painter<'a> {
    cr: Option<&'a Context>,
    pango: Option<pango::Context>,
    hits: Vec<(Hit, Rect)>,
}

#[derive(Clone, Copy)]
struct Style {
    size: f64,
    medium: bool,
    color: (f64, f64, f64),
    alpha: f64,
    letter_spacing: f64, // in em
}

const S_TITLE: Style = Style { size: 11.0, medium: true, color: FG, alpha: 0.75, letter_spacing: 0.06 };
const S_LABEL: Style = Style { size: 10.0, medium: false, color: FG, alpha: 0.45, letter_spacing: 0.04 };
const S_VALUE: Style = Style { size: 12.0, medium: false, color: FG, alpha: 1.0, letter_spacing: 0.0 };
const S_VALUE_DIM: Style = Style { size: 12.0, medium: false, color: FG, alpha: 0.45, letter_spacing: 0.0 };
const S_PREFIX: Style = Style { size: 10.0, medium: false, color: FG, alpha: 0.45, letter_spacing: 0.0 };
const S_IP: Style = Style { size: 11.0, medium: false, color: FG, alpha: 0.65, letter_spacing: 0.0 };
const S_TOP_NAME: Style = Style { size: 10.0, medium: false, color: FG, alpha: 0.55, letter_spacing: 0.0 };
const S_TOP_VAL: Style = Style { size: 11.0, medium: false, color: FG, alpha: 1.0, letter_spacing: 0.0 };
const S_TOP_DIM: Style = Style { size: 10.0, medium: false, color: FG, alpha: 0.45, letter_spacing: 0.0 };

impl<'a> Painter<'a> {
    fn layout(&self, text: &str, style: Style, max_w: Option<f64>) -> Option<pango::Layout> {
        let ctx = self.pango.as_ref()?;
        let layout = pango::Layout::new(ctx);
        let mut desc = pango::FontDescription::new();
        desc.set_family("JetBrains Mono");
        desc.set_weight(if style.medium { pango::Weight::Medium } else { pango::Weight::Normal });
        desc.set_absolute_size(style.size * pango::SCALE as f64);
        layout.set_font_description(Some(&desc));
        if style.letter_spacing > 0.0 {
            let attrs = pango::AttrList::new();
            let units = (style.letter_spacing * style.size * pango::SCALE as f64) as i32;
            attrs.insert(pango::AttrInt::new_letter_spacing(units));
            layout.set_attributes(Some(&attrs));
        }
        if let Some(w) = max_w {
            layout.set_width((w * pango::SCALE as f64) as i32);
            layout.set_ellipsize(pango::EllipsizeMode::End);
        }
        layout.set_text(text);
        Some(layout)
    }

    fn text_width(&self, text: &str, style: Style) -> f64 {
        match self.layout(text, style, None) {
            Some(l) => l.pixel_size().0 as f64,
            None => text.chars().count() as f64 * style.size * 0.6, // mess-modus: monospace-schätzung
        }
    }

    /// zeichnet text mit baseline bei `baseline_y`; gibt die breite zurück
    fn text(&self, x: f64, baseline_y: f64, text: &str, style: Style, max_w: Option<f64>) -> f64 {
        let Some(cr) = self.cr else {
            return self.text_width(text, style);
        };
        let Some(layout) = self.layout(text, style, max_w) else {
            return 0.0;
        };
        let baseline_px = layout.baseline() as f64 / pango::SCALE as f64;
        cr.set_source_rgba(style.color.0, style.color.1, style.color.2, style.alpha);
        cr.move_to(x, baseline_y - baseline_px);
        pangocairo::functions::show_layout(cr, &layout);
        layout.pixel_size().0 as f64
    }

    /// rechtsbündig ab `right`; gibt die linke kante zurück
    fn text_right(&self, right: f64, baseline_y: f64, text: &str, style: Style) -> f64 {
        let w = self.text_width(text, style);
        self.text(right - w, baseline_y, text, style, None);
        right - w
    }

    fn rounded_rect(&self, r: Rect, radius: f64) {
        let Some(cr) = self.cr else { return };
        let (x, y, w, h) = (r.x, r.y, r.w, r.h);
        let rad = radius.min(w / 2.0).min(h / 2.0);
        cr.new_sub_path();
        cr.arc(x + w - rad, y + rad, rad, -0.5 * std::f64::consts::PI, 0.0);
        cr.arc(x + w - rad, y + h - rad, rad, 0.0, 0.5 * std::f64::consts::PI);
        cr.arc(x + rad, y + h - rad, rad, 0.5 * std::f64::consts::PI, std::f64::consts::PI);
        cr.arc(x + rad, y + rad, rad, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
        cr.close_path();
    }

    fn fill_rounded(&self, r: Rect, radius: f64, rgba: (f64, f64, f64, f64)) {
        let Some(cr) = self.cr else { return };
        self.rounded_rect(r, radius);
        cr.set_source_rgba(rgba.0, rgba.1, rgba.2, rgba.3);
        let _ = cr.fill();
    }

    fn hline(&self, x: f64, y: f64, w: f64, alpha: f64) {
        let Some(cr) = self.cr else { return };
        cr.set_source_rgba(1.0, 1.0, 1.0, alpha);
        cr.rectangle(x, y, w, 1.0);
        let _ = cr.fill();
    }

    fn hit(&mut self, hit: Hit, r: Rect) {
        self.hits.push((hit, r));
    }
}

// --- schatten -------------------------------------------------------------

/// weicher schatten als gestapelte, immer grössere rundrechtecke —
/// eine gauss-annäherung, die ohne blur-filter auskommt. wird vom
/// aufrufer in eine surface gecacht.
fn draw_shadow(cr: &Context, panel: Rect) {
    // 0 8px 28px rgba(0,0,0,0.28)
    let layers = 14;
    for i in 0..layers {
        let t = i as f64 / layers as f64; // 0 = aussen
        let spread = 28.0 * (1.0 - t);
        let alpha = 0.28 * (t * t) / layers as f64 * 3.4;
        let r = Rect::new(
            panel.x - spread,
            panel.y - spread + 8.0,
            panel.w + spread * 2.0,
            panel.h + spread * 2.0,
        );
        let rad = RADIUS + spread;
        let (x, y, w, h) = (r.x, r.y, r.w, r.h);
        let rad = rad.min(w / 2.0).min(h / 2.0);
        cr.new_sub_path();
        cr.arc(x + w - rad, y + rad, rad, -0.5 * std::f64::consts::PI, 0.0);
        cr.arc(x + w - rad, y + h - rad, rad, 0.0, 0.5 * std::f64::consts::PI);
        cr.arc(x + rad, y + h - rad, rad, 0.5 * std::f64::consts::PI, std::f64::consts::PI);
        cr.arc(x + rad, y + rad, rad, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
        cr.close_path();
        cr.set_source_rgba(0.0, 0.0, 0.0, alpha);
        let _ = cr.fill();
    }
    // 0 1px 3px rgba(0,0,0,0.2)
    for i in 0..3 {
        let spread = 3.0 - i as f64;
        let alpha = 0.2 / 3.0;
        cr.new_sub_path();
        let r = Rect::new(
            panel.x - spread,
            panel.y - spread + 1.0,
            panel.w + spread * 2.0,
            panel.h + spread * 2.0,
        );
        let rad = (RADIUS + spread).min(r.w / 2.0);
        cr.arc(r.x + r.w - rad, r.y + rad, rad, -0.5 * std::f64::consts::PI, 0.0);
        cr.arc(r.x + r.w - rad, r.y + r.h - rad, rad, 0.0, 0.5 * std::f64::consts::PI);
        cr.arc(r.x + rad, r.y + r.h - rad, rad, 0.5 * std::f64::consts::PI, std::f64::consts::PI);
        cr.arc(r.x + rad, r.y + rad, rad, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
        cr.close_path();
        cr.set_source_rgba(0.0, 0.0, 0.0, alpha);
        let _ = cr.fill();
    }
}

// --- haupt-pass -----------------------------------------------------------

/// panel-höhe für den aktuellen zustand (mess-pass ohne zeichnen)
pub fn panel_height(app: &App) -> f64 {
    let mut painter = Painter { cr: None, pango: None, hits: Vec::new() };
    pass(&mut painter, app, None)
}

/// gecachter statischer hintergrund (schatten + panel + rand): der schatten
/// besteht aus vielen grossflächigen fills und wäre pro frame der teuerste
/// teil — er ändert sich aber nur mit der panel-höhe
#[derive(Default)]
pub struct BgCache {
    key: (bool, i64),
    surface: Option<cairo::Surface>,
}

/// zeichnet alles und liefert die hit-regionen des passes
pub fn draw(
    cr: &Context,
    pango: pango::Context,
    app: &App,
    cache: &mut BgCache,
) -> (Vec<(Hit, Rect)>, f64) {
    let mut painter = Painter { cr: Some(cr), pango: Some(pango), hits: Vec::new() };
    let h = pass(&mut painter, app, Some(cache));
    (painter.hits, h)
}

fn background_surface(cr: &Context, panel: Rect, cache: &mut BgCache, collapsed: bool) -> Option<cairo::Surface> {
    let key = (collapsed, panel.h.round() as i64);
    if cache.key != key || cache.surface.is_none() {
        let w = (panel.w + 2.0 * MARGIN).ceil();
        let h = (panel.h + 2.0 * MARGIN).ceil();
        let target = cr.target();
        let surface = cairo::Surface::create_similar(&target, cairo::Content::ColorAlpha, w as i32, h as i32).ok()?;
        let scr = Context::new(&surface).ok()?;
        let local_panel = Rect::new(MARGIN, MARGIN, panel.w, panel.h);
        draw_shadow(&scr, local_panel);
        // panel-hintergrund + 1px-rand
        let p = Painter { cr: Some(&scr), pango: None, hits: Vec::new() };
        p.fill_rounded(local_panel, RADIUS, BG);
        p.rounded_rect(Rect::new(local_panel.x + 0.5, local_panel.y + 0.5, local_panel.w - 1.0, local_panel.h - 1.0), RADIUS - 0.5);
        scr.set_source_rgba(1.0, 1.0, 1.0, 0.06);
        scr.set_line_width(1.0);
        let _ = scr.stroke();
        cache.key = key;
        cache.surface = Some(surface);
    }
    cache.surface.clone()
}

fn pass(p: &mut Painter, app: &App, cache: Option<&mut BgCache>) -> f64 {
    let header_h = if app.collapsed { HEADER_H_COLLAPSED } else { HEADER_H };

    // erst messen, dann zeichnen: die panel-höhe braucht der hintergrund
    let content_h = if app.collapsed { 0.0 } else { measure_content(p, app) };
    let panel_h = header_h + content_h;
    let panel = Rect::new(MARGIN, MARGIN, PANEL_W, panel_h);

    if let (Some(cr), Some(cache)) = (p.cr, cache) {
        if let Some(surface) = background_surface(cr, panel, cache, app.collapsed) {
            let _ = cr.set_source_surface(&surface, 0.0, 0.0);
            let _ = cr.paint();
            // quelle zurücksetzen, sonst hält cr die surface fest
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        }
    }

    // kopfzeile
    let head_base = panel.y + header_h / 2.0 + 4.0; // optische baseline-mitte
    p.text(panel.x + PAD_X, head_base, "indigo", S_TITLE, None);

    let dot_r = 4.0;
    let dot_cx = panel.x + panel.w - PAD_X - dot_r;
    let dot_cy = panel.y + header_h / 2.0;
    if let Some(cr) = p.cr {
        let alpha = if app.hover == Some(Hit::Dot) { 0.6 } else { 0.28 };
        cr.set_source_rgba(FG.0, FG.1, FG.2, alpha);
        cr.arc(dot_cx, dot_cy, dot_r, 0.0, 2.0 * std::f64::consts::PI);
        let _ = cr.fill();
    }
    // grosszügige trefferfläche um den 8px-punkt
    p.hit(Hit::Dot, Rect::new(dot_cx - 10.0, dot_cy - 10.0, 20.0, 20.0));

    if app.collapsed {
        // cpu/ram mittig in der kopfzeile
        if let Some(stats) = &app.stats {
            let cpu = stats.cpu.map(fmt_pct).unwrap_or_else(|| "–".into());
            let ram = stats.ram.map(fmt_pct).unwrap_or_else(|| "–".into());
            // gesamtbreite für zentrierung berechnen
            let w_total = p.text_width("cpu ", S_PREFIX)
                + p.text_width(&cpu, S_VALUE)
                + 14.0
                + p.text_width("ram ", S_PREFIX)
                + p.text_width(&ram, S_VALUE);
            let mut x = panel.x + (panel.w - w_total) / 2.0;
            x += p.text(x, head_base, "cpu ", S_PREFIX, None);
            x += p.text(x, head_base, &cpu, S_VALUE, None);
            x += 14.0;
            x += p.text(x, head_base, "ram ", S_PREFIX, None);
            p.text(x, head_base, &ram, S_VALUE, None);
        }
        p.hit(Hit::Header, Rect::new(panel.x, panel.y, panel.w - 32.0, header_h));
        return panel_h;
    }

    p.hit(Hit::Header, Rect::new(panel.x, panel.y, panel.w - 32.0, header_h));
    p.hline(panel.x, panel.y + header_h, panel.w, 0.05);

    draw_content(p, app, panel.x, panel.y + header_h + 1.0);
    panel_h
}

/// misst die inhaltshöhe (identische logik wie draw_content, nur geometrie)
fn measure_content(p: &mut Painter, app: &App) -> f64 {
    let mut m = Painter { cr: None, pango: p.pango.clone(), hits: Vec::new() };
    draw_content(&mut m, app, 0.0, 0.0)
}

fn draw_content(p: &mut Painter, app: &App, px: f64, py: f64) -> f64 {
    let x0 = px + PAD_X;
    let w = PANEL_W - 2.0 * PAD_X;
    let right = px + PANEL_W - PAD_X;
    let mut y = py + PAD_X; // padding oben 12

    let stats = app.stats.as_ref();

    // --- gauge-zeilen cpu/ram/disk/gpu ---
    let gauges: [(&str, BarId, Option<f32>, &'static str); 4] = [
        ("cpu", BarId::Cpu, stats.and_then(|s| s.cpu), "cpu"),
        ("ram", BarId::Ram, stats.and_then(|s| s.ram), "ram"),
        ("disk", BarId::Disk, stats.and_then(|s| s.disk), ""),
        ("gpu", BarId::Gpu, stats.and_then(|s| s.gpu), "gpu"),
    ];
    for (label, bar_id, value, top_kind) in gauges {
        let base = y + BASELINE;
        p.text(x0, base, label, S_LABEL, None);
        let text = value.map(fmt_pct).unwrap_or_else(|| "n/a".into());
        let mut left_edge = p.text_right(right, base, &text, S_VALUE);
        if label == "ram" {
            if let Some(s) = stats {
                if let (Some(u), Some(t)) = (s.ram_used, s.ram_total) {
                    left_edge -= 12.0;
                    p.text_right(left_edge, base, &fmt_ram_abs(u, t), S_VALUE_DIM);
                }
            }
        }
        let _ = left_edge;
        if !top_kind.is_empty() {
            p.hit(Hit::TopRow(match top_kind {
                "cpu" => "cpu",
                "ram" => "ram",
                _ => "gpu",
            }), Rect::new(x0, y, w, LINE_H));
        }
        // balken
        let bar_y = y + LINE_H + BAR_GAP;
        p.fill_rounded(Rect::new(x0, bar_y, w, BAR_H), 2.0, (1.0, 1.0, 1.0, 0.07));
        let shown = *app.bar_shown.get(&bar_id).unwrap_or(&0.0);
        if shown > 0.0 && value.is_some() {
            let c = threshold_color(shown);
            p.fill_rounded(Rect::new(x0, bar_y, w * shown / 100.0, BAR_H), 2.0, (c.0, c.1, c.2, 1.0));
        }
        y += LINE_H + BAR_GAP + BAR_H;

        // dropdown unter der zeile
        if let Some(dd) = &app.dropdown {
            if dd.kind == top_kind && !top_kind.is_empty() {
                y = draw_dropdown(p, dd, x0, w, y);
            }
        }
        y += GAP;
    }

    // --- temp ---
    let base = y + BASELINE;
    p.text(x0, base, "temp", S_LABEL, None);
    {
        let cpu_t = stats.and_then(|s| s.temp_cpu).map(fmt_temp).unwrap_or_else(|| "n/a".into());
        let gpu_t = stats.and_then(|s| s.temp_gpu).map(fmt_temp).unwrap_or_else(|| "n/a".into());
        let mut left = p.text_right(right, base, &gpu_t, S_VALUE);
        left = p.text_right(left, base, "gpu ", S_PREFIX);
        left -= 12.0;
        left = p.text_right(left, base, &cpu_t, S_VALUE);
        p.text_right(left, base, "cpu ", S_PREFIX);
    }
    y += LINE_H + GAP;

    // --- net ---
    let base = y + BASELINE;
    p.text(x0, base, "net", S_LABEL, None);
    {
        let up = stats.and_then(|s| s.net_up).map(fmt_mbs).unwrap_or_else(|| "–".into());
        let down = stats.and_then(|s| s.net_down).map(fmt_mbs).unwrap_or_else(|| "–".into());
        let mut left = p.text_right(right, base, &down, S_VALUE);
        left = p.text_right(left, base, "↓ ", S_PREFIX);
        left -= 12.0;
        left = p.text_right(left, base, &up, S_VALUE);
        p.text_right(left, base, "↑ ", S_PREFIX);
    }
    y += LINE_H + GAP;

    // --- pwr ---
    let base = y + BASELINE;
    p.text(x0, base, "pwr", S_LABEL, None);
    let pwr = stats.and_then(|s| s.pwr).map(fmt_watt).unwrap_or_else(|| "n/a".into());
    p.text_right(right, base, &pwr, S_VALUE);
    y += LINE_H;

    // --- lüfter-sektion ---
    let has_fans = stats.map(|s| s.gpu_fan.is_some() || !s.fans.is_empty()).unwrap_or(false);
    if has_fans {
        y += GAP;
        p.hline(x0, y, w, 0.05);
        y += GAP + 1.0;
        let s = stats.unwrap();

        if let Some(gpu_fan) = s.gpu_fan {
            let base = y + BASELINE;
            p.text(x0, base, "fan gpu", S_LABEL, None);
            p.text_right(right, base, &fmt_pct(gpu_fan), S_VALUE);
            y += LINE_H + GAP;
        }

        let controllable: Vec<&crate::fans::FanStat> =
            s.fans.iter().filter(|f| f.pct.is_some()).collect();

        // gruppen-regler bei mehr als einem steuerbaren kanal
        if controllable.len() > 1 {
            let autos = controllable.iter().filter(|f| f.auto_mode != Some(false)).count();
            let avg = controllable.iter().filter_map(|f| f.pct).map(|v| v as f64).sum::<f64>()
                / controllable.len().max(1) as f64;
            let (mode_text, manual) = if let Some((FanTarget::Group, pct)) = &app.drag_fan {
                (fmt_pct(*pct as f32), true)
            } else if autos == controllable.len() {
                ("auto".into(), false)
            } else if autos == 0 {
                (fmt_pct(avg as f32), true)
            } else {
                ("mix".into(), true)
            };
            let shown = if let Some((FanTarget::Group, pct)) = &app.drag_fan {
                *pct
            } else {
                *app.bar_shown.get(&BarId::FanGroup).unwrap_or(&avg)
            };
            y = draw_fan_row(p, app, x0, w, right, y, "fans", &mode_text, manual, None, shown,
                autos > 0 && app.drag_fan.is_none(), FanTarget::Group);
            y += GAP;
        }

        for fan in &s.fans {
            let target = FanTarget::One(fan.id.clone());
            let dragging = matches!(&app.drag_fan, Some((t, _)) if *t == target);
            let rpm_text = fan.rpm.map(fmt_rpm);
            if fan.pct.is_some() {
                let (mode_text, manual) = if let Some((t, pct)) = &app.drag_fan {
                    if *t == target {
                        (fmt_pct(*pct as f32), true)
                    } else {
                        fan_mode_text(fan)
                    }
                } else {
                    fan_mode_text(fan)
                };
                let shown = if dragging {
                    app.drag_fan.as_ref().unwrap().1
                } else {
                    *app.bar_shown.get(&BarId::Fan(fan.id.clone())).unwrap_or(&0.0)
                };
                let dim = fan.auto_mode != Some(false) && !dragging;
                y = draw_fan_row(p, app, x0, w, right, y, &fan.label, &mode_text, manual,
                    rpm_text.as_deref(), shown, dim, target);
            } else {
                // nur drehzahl, keine steuerung
                let base = y + BASELINE;
                p.text(x0, base, &fan.label, S_LABEL, None);
                p.text_right(right, base, rpm_text.as_deref().unwrap_or("n/a"), S_VALUE);
                y += LINE_H;
            }
            y += GAP;
        }
        y -= GAP; // letzte lücke zurücknehmen
    }

    // --- ip ---
    y += GAP;
    p.hline(x0, y, w, 0.05);
    y += 2.0 + 1.0;
    let base = y + BASELINE;
    p.text(x0, base, "ip", S_LABEL, None);
    let ip = stats.and_then(|s| s.ip.clone()).unwrap_or_else(|| "n/a".into());
    p.text_right(right, base, &ip, S_IP);
    y += LINE_H;

    y += PAD_X; // padding unten
    y - py
}

fn fan_mode_text(fan: &crate::fans::FanStat) -> (String, bool) {
    match fan.auto_mode {
        Some(true) => ("auto".into(), false),
        Some(false) => (
            fan.pct.map(|p| fmt_pct(p)).unwrap_or_else(|| "man".into()),
            true,
        ),
        None => (String::new(), false),
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_fan_row(
    p: &mut Painter,
    app: &App,
    x0: f64,
    w: f64,
    right: f64,
    y: f64,
    label: &str,
    mode_text: &str,
    manual: bool,
    rpm: Option<&str>,
    shown_pct: f64,
    dim: bool,
    target: FanTarget,
) -> f64 {
    let base = y + BASELINE;
    p.text(x0, base, label, S_LABEL, None);

    let denied_key = match &target {
        FanTarget::Group => "__group__",
        FanTarget::One(id) => id.as_str(),
    };
    let denied = app.fan_denied_active(denied_key);
    let (mode_text, manual) = if denied {
        ("denied", false)
    } else {
        (mode_text, manual)
    };

    let mut left = right;
    if let Some(rpm) = rpm {
        left = p.text_right(left, base, rpm, S_VALUE);
        left -= 12.0;
    }
    let style = if manual {
        S_VALUE
    } else if app.hover == Some(Hit::FanMode(target.clone())) {
        Style { alpha: 0.75, ..S_VALUE_DIM }
    } else {
        S_VALUE_DIM
    };
    let mode_left = p.text_right(left, base, mode_text, style);
    if !mode_text.is_empty() {
        p.hit(
            Hit::FanMode(target.clone()),
            Rect::new(mode_left - 4.0, y - 2.0, left - mode_left + 8.0, LINE_H + 4.0),
        );
    }

    // slider (balken)
    let bar_y = y + LINE_H + BAR_GAP;
    p.fill_rounded(Rect::new(x0, bar_y, w, BAR_H), 2.0, (1.0, 1.0, 1.0, 0.07));
    if shown_pct > 0.0 {
        let alpha = if dim { 0.45 } else { 1.0 };
        p.fill_rounded(
            Rect::new(x0, bar_y, w * (shown_pct / 100.0).clamp(0.0, 1.0), BAR_H),
            2.0,
            (INDIGO.0, INDIGO.1, INDIGO.2, alpha),
        );
    }
    // grosszügige trefferfläche um den 3px-balken
    p.hit(Hit::FanSlider(target), Rect::new(x0, bar_y - 5.0, w, BAR_H + 10.0));

    y + LINE_H + BAR_GAP + BAR_H
}

fn draw_dropdown(p: &mut Painter, dd: &crate::state::Dropdown, x0: f64, w: f64, y: f64) -> f64 {
    let mut inner_y = y + 6.0; // margin-top
    let pad_v = 8.0;
    let pad_h = 10.0;
    let row_h = 13.0;
    let gap = 5.0;

    // höhe bestimmen
    let (rows, warning): (usize, bool) = match &dd.list {
        Some(list) => (list.entries.len().max(usize::from(list.entries.is_empty())), list.warning.is_some()),
        None => (1, false),
    };
    let n = rows + usize::from(warning);
    let box_h = pad_v * 2.0 + n as f64 * row_h + (n.saturating_sub(1)) as f64 * gap;
    p.fill_rounded(Rect::new(x0, inner_y, w, box_h), 6.0, (1.0, 1.0, 1.0, 0.03));

    let mut ry = inner_y + pad_v;
    let left = x0 + pad_h;
    let right = x0 + w - pad_h;
    match &dd.list {
        None => {
            p.text(left, ry + 10.0, "lade …", S_TOP_DIM, None);
        }
        Some(list) => {
            if let Some(warn) = &list.warning {
                p.text(left, ry + 10.0, &format!("! {warn}"), S_TOP_DIM, Some(right - left));
                ry += row_h + gap;
            }
            if list.entries.is_empty() {
                p.text(left, ry + 10.0, "keine prozesse", S_TOP_DIM, None);
            }
            for entry in &list.entries {
                let base = ry + 10.0;
                let val = if entry.unit == "pct" {
                    fmt_pct_fine(entry.value)
                } else {
                    fmt_bytes(entry.value)
                };
                let mut vleft = right;
                if let Some(count) = entry.count {
                    if count > 1 {
                        vleft = p.text_right(vleft, base, &format!(" ({count})"), S_TOP_DIM);
                    }
                }
                vleft = p.text_right(vleft, base, &val, S_TOP_VAL);
                p.text(left, base, &entry.name, S_TOP_NAME, Some((vleft - left - 8.0).max(20.0)));
                ry += row_h + gap;
            }
        }
    }
    let _ = inner_y;
    inner_y = y + 6.0 + box_h;
    inner_y
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fans::FanStat;
    use crate::monitor::Stats;
    use crate::processes::{TopEntry, TopList};
    use crate::state::Dropdown;

    fn demo_stats() -> Stats {
        Stats {
            cpu: Some(34.0),
            ram: Some(62.0),
            ram_used: Some((9.6 * 1024.0 * 1024.0 * 1024.0) as u64),
            ram_total: Some((15.6 * 1024.0 * 1024.0 * 1024.0) as u64),
            disk: Some(48.0),
            gpu: Some(72.0),
            temp_cpu: Some(52.0),
            temp_gpu: Some(44.0),
            net_up: Some(0.4),
            net_down: Some(3.2),
            pwr: Some(64.0),
            ip: Some("192.168.1.42".into()),
            gpu_fan: Some(35.0),
            fans: vec![
                FanStat { id: "nct:1".into(), label: "fan1".into(), rpm: Some(1240), pct: Some(45.0), auto_mode: Some(true) },
                FanStat { id: "nct:2".into(), label: "fan2".into(), rpm: Some(880), pct: Some(92.0), auto_mode: Some(false) },
            ],
        }
    }

    fn render_to_png(app: &App, path: &str) {
        let h = (panel_height(app) + 2.0 * MARGIN).ceil() as i32;
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, WINDOW_W, h).unwrap();
        let cr = Context::new(&surface).unwrap();
        // dunkler prüf-hintergrund, damit transparenz sichtbar bleibt
        cr.set_source_rgb(0.72, 0.72, 0.74);
        let _ = cr.paint();
        let fontmap = pangocairo::FontMap::default();
        let pctx = fontmap.create_context();
        let mut cache = BgCache::default();
        let _ = draw(&cr, pctx, app, &mut cache);
        drop(cr);
        let mut file = std::fs::File::create(path).unwrap();
        surface.write_to_png(&mut file).unwrap();
        println!("preview: {path}");
    }

    /// rendert beide zustände als png zur sichtprüfung des designs
    #[test]
    fn preview_beider_zustaende() {
        crate::ensure_fonts();
        let dir = std::env::temp_dir().join("indigo-preview");
        let _ = std::fs::create_dir_all(&dir);

        let mut app = App::new(false);
        app.stats = Some(demo_stats());
        for (id, v) in [(BarId::Cpu, 34.0), (BarId::Ram, 62.0), (BarId::Disk, 48.0), (BarId::Gpu, 72.0)] {
            app.set_bar_immediate(id, v);
        }
        app.set_bar_immediate(BarId::Fan("nct:1".into()), 45.0);
        app.set_bar_immediate(BarId::Fan("nct:2".into()), 92.0);
        app.set_bar_immediate(BarId::FanGroup, 68.0);
        render_to_png(&app, dir.join("normal.png").to_str().unwrap());

        app.dropdown = Some(Dropdown {
            kind: "ram",
            list: Some(TopList {
                entries: vec![
                    TopEntry { name: "chrome".into(), value: 3.4 * 1024.0 * 1024.0 * 1024.0, unit: "bytes", count: Some(29) },
                    TopEntry { name: "code".into(), value: 2.1 * 1024.0 * 1024.0 * 1024.0, unit: "bytes", count: Some(37) },
                    TopEntry { name: "ein-sehr-langer-prozessname-zum-testen".into(), value: 0.5 * 1024.0 * 1024.0 * 1024.0, unit: "bytes", count: Some(1) },
                ],
                warning: None,
            }),
        });
        render_to_png(&app, dir.join("dropdown.png").to_str().unwrap());
        app.dropdown = None;

        app.collapsed = true;
        render_to_png(&app, dir.join("kollabiert.png").to_str().unwrap());
    }
}
