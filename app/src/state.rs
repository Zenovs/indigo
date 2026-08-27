//! Zentraler UI-Zustand. Render und Input teilen sich die Hit-Regionen,
//! die beim Zeichnen berechnet werden — eine Quelle für Geometrie.

use std::collections::HashMap;
use std::time::Instant;

use crate::monitor::Stats;
use crate::processes::TopList;

#[derive(Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// Balken-Identität für Animationen und angezeigte Werte.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum BarId {
    Cpu,
    Ram,
    Disk,
    Gpu,
    FanGroup,
    Fan(String),
}

/// Klickbare Bereiche, beim Zeichnen eingesammelt.
#[derive(Clone, PartialEq, Debug)]
pub enum Hit {
    Header,
    Dot,
    TopRow(&'static str),
    FanMode(FanTarget),
    FanSlider(FanTarget),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FanTarget {
    Group,
    One(String),
}

pub struct Dropdown {
    pub kind: &'static str,
    pub list: Option<TopList>,
}

pub struct Anim {
    pub id: BarId,
    pub from: f64,
    pub target: f64,
    pub step: u32,
}

pub const ANIM_STEPS: u32 = 8;
pub const ANIM_STEP_MS: u32 = 100;
/// kleine drifts springen ohne animation (leerlauf bleibt bei einem
/// repaint pro tick)
pub const ANIM_MIN_DELTA: f64 = 4.0;

pub struct App {
    pub stats: Option<Stats>,
    pub collapsed: bool,
    pub hover: Option<Hit>,
    pub dropdown: Option<Dropdown>,
    /// aktuell gezeichnete balkenbreite in % (animationsstand)
    pub bar_shown: HashMap<BarId, f64>,
    pub anims: Vec<Anim>,
    /// lüfter-id -> bis wann ticks die anzeige nicht überschreiben
    pub fan_hold: HashMap<String, Instant>,
    /// lüfter-id -> bis wann "denied" angezeigt wird (sysfs-schreibfehler)
    pub fan_denied: HashMap<String, Instant>,
    /// aktiver slider-drag mit vorschau-prozent
    pub drag_fan: Option<(FanTarget, f64)>,
    /// hit-regionen des letzten draw-passes
    pub hits: Vec<(Hit, Rect)>,
    pub started: Instant,
}

impl App {
    pub fn new(collapsed: bool) -> Self {
        Self {
            stats: None,
            collapsed,
            hover: None,
            dropdown: None,
            bar_shown: HashMap::new(),
            anims: Vec::new(),
            fan_hold: HashMap::new(),
            fan_denied: HashMap::new(),
            drag_fan: None,
            hits: Vec::new(),
            started: Instant::now(),
        }
    }

    /// Zielwert für einen Balken setzen; grosse Sprünge werden animiert.
    pub fn set_bar_target(&mut self, id: BarId, pct: f64) {
        let target = pct.clamp(0.0, 100.0).round();
        let shown = *self.bar_shown.get(&id).unwrap_or(&0.0);
        if (target - shown).abs() < f64::EPSILON {
            return;
        }
        // eine laufende animation mit demselben ziel weiterlaufen lassen,
        // statt sie bei jedem tick ab der zwischenposition neu zu starten
        if self.anims.iter().any(|a| a.id == id && a.target == target) {
            return;
        }
        self.anims.retain(|a| a.id != id);
        if (target - shown).abs() < ANIM_MIN_DELTA {
            self.bar_shown.insert(id, target);
            return;
        }
        self.anims.push(Anim {
            id,
            from: shown,
            target,
            step: 0,
        });
    }

    pub fn set_bar_immediate(&mut self, id: BarId, pct: f64) {
        let target = pct.clamp(0.0, 100.0).round();
        self.anims.retain(|a| a.id != id);
        self.bar_shown.insert(id, target);
    }

    /// Einen Animationsschritt rechnen; true solange weitere folgen.
    pub fn tick_anims(&mut self) -> bool {
        let mut done: Vec<BarId> = Vec::new();
        for anim in &mut self.anims {
            anim.step += 1;
            let t = anim.step as f64 / ANIM_STEPS as f64;
            let eased = t * t * (3.0 - 2.0 * t); // smoothstep, nahe css "ease"
            let value = anim.from + (anim.target - anim.from) * eased;
            self.bar_shown.insert(anim.id.clone(), value);
            if anim.step >= ANIM_STEPS {
                done.push(anim.id.clone());
            }
        }
        self.anims.retain(|a| !done.contains(&a.id));
        !self.anims.is_empty()
    }

    pub fn hit_at(&self, x: f64, y: f64) -> Option<Hit> {
        // spätere regionen liegen visuell oben (dot vor header)
        self.hits
            .iter()
            .rev()
            .find(|(_, r)| r.contains(x, y))
            .map(|(h, _)| h.clone())
    }

    pub fn hold_fan(&mut self, target: &FanTarget, until: Instant, all_ids: &[String]) {
        match target {
            FanTarget::One(id) => {
                self.fan_hold.insert(id.clone(), until);
            }
            FanTarget::Group => {
                self.fan_hold.insert("__group__".into(), until);
                for id in all_ids {
                    self.fan_hold.insert(id.clone(), until);
                }
            }
        }
    }

    pub fn fan_held(&self, id: &str) -> bool {
        self.fan_hold
            .get(id)
            .map(|t| *t > Instant::now())
            .unwrap_or(false)
    }

    pub fn fan_denied_active(&self, id: &str) -> bool {
        self.fan_denied
            .get(id)
            .map(|t| *t > Instant::now())
            .unwrap_or(false)
    }
}
