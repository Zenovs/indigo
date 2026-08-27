// indigo — frontend renderer: empfängt "stats"-Events vom Rust-Backend

interface FanStat {
  id: string;
  label: string;
  rpm: number | null;
  pct: number | null;
  autoMode: boolean | null;
}

interface Stats {
  cpu: number | null;
  ram: number | null;
  ramUsed: number | null;
  ramTotal: number | null;
  disk: number | null;
  gpu: number | null;
  tempCpu: number | null;
  tempGpu: number | null;
  netUp: number | null;
  netDown: number | null;
  pwr: number | null;
  ip: string | null;
  gpuFan: number | null;
  fans: FanStat[];
}

interface TopEntry {
  name: string;
  value: number;
  unit: 'pct' | 'bytes';
  count: number | null;
}

interface TopList {
  entries: TopEntry[];
  warning: string | null;
}

interface TauriGlobal {
  window: {
    getCurrentWindow(): {
      setSize(size: unknown): Promise<void>;
    };
  };
  dpi: {
    LogicalSize: new (width: number, height: number) => unknown;
  };
  event: {
    listen<T>(event: string, handler: (e: { payload: T }) => void): Promise<() => void>;
  };
  core: {
    invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T>;
  };
}

declare global {
  interface Window {
    __TAURI__: TauriGlobal;
  }
}

const SHADOW_MARGIN = 32; // muss zum body-padding in style.css passen

function el(id: string): HTMLElement {
  const node = document.getElementById(id);
  if (!node) throw new Error(`element fehlt: ${id}`);
  return node;
}

function thresholdColor(pct: number): string {
  if (pct >= 90) return 'oklch(0.62 0.2 27)';
  if (pct >= 70) return 'oklch(0.76 0.15 75)';
  return 'oklch(0.6 0.18 278)';
}

function fmtPct(v: number): string {
  return `${Math.round(v)}%`;
}

function fmtPctFine(v: number): string {
  return v >= 10 ? `${Math.round(v)}%` : `${v.toFixed(1)}%`;
}

function fmtMbs(v: number): string {
  return v >= 10 ? `${Math.round(v)} mb/s` : `${v.toFixed(1)} mb/s`;
}

function fmtTemp(v: number): string {
  return `${Math.round(v)}°`;
}

function fmtWatt(v: number): string {
  return `${Math.round(v)} w`;
}

const GIB = 1024 * 1024 * 1024;

function fmtBytes(v: number): string {
  return v >= GIB ? `${(v / GIB).toFixed(1)} gb` : `${Math.round(v / (1024 * 1024))} mb`;
}

/** "7.8/15.6 gb" — beide werte in gib, eine nachkommastelle */
function fmtRamAbs(used: number, total: number): string {
  return `${(used / GIB).toFixed(1)}/${(total / GIB).toFixed(1)} gb`;
}

function fmtRpm(v: number): string {
  return `${Math.round(v)} rpm`;
}

// dom-schreibzugriffe nur bei tatsächlicher änderung — style-invalidierung
// und repaints sind der teuerste teil des widgets
const textCache = new WeakMap<HTMLElement, string>();

function setText(node: HTMLElement, text: string): void {
  if (textCache.get(node) === text) return;
  textCache.set(node, text);
  node.textContent = text;
}

// weicher balken-übergang in js: 8 schritte über 0.8s statt einer
// css-transition, die 0.8s lang mit voller framerate repainten würde
interface BarState {
  shown: number;
  target: number;
  timer: number | null;
  color: string;
  dim: boolean;
}

const barStates = new WeakMap<HTMLElement, BarState>();
const BAR_STEPS = 8;
const BAR_DURATION_MS = 800;

// ein gemeinsamer takt für alle balken: alle übergänge schreiben im selben
// frame, statt pro balken einen eigenen timer (und eigene repaints) zu haben
interface BarAnim {
  fill: HTMLElement;
  st: BarState;
  from: number;
  step: number;
}

let barAnims: BarAnim[] = [];
let barClock: number | null = null;

function tickBarClock(): void {
  barAnims = barAnims.filter((anim) => {
    anim.step++;
    const t = anim.step / BAR_STEPS;
    const eased = t * t * (3 - 2 * t); // smoothstep, nahe an css "ease"
    anim.st.shown = anim.from + (anim.st.target - anim.from) * eased;
    anim.fill.style.transform = `scaleX(${anim.st.shown / 100})`;
    return anim.step < BAR_STEPS;
  });
  if (barAnims.length === 0 && barClock !== null) {
    clearInterval(barClock);
    barClock = null;
  }
}

function setBar(
  fill: HTMLElement,
  pct: number,
  color: string | null,
  dim: boolean,
  immediate = false,
): void {
  let st = barStates.get(fill);
  if (!st) {
    st = { shown: 0, target: 0, timer: null, color: '', dim: false };
    barStates.set(fill, st);
  }
  if (color !== null && color !== st.color) {
    st.color = color;
    fill.style.background = color;
  }
  if (dim !== st.dim) {
    st.dim = dim;
    fill.classList.toggle('dim', dim);
  }
  const target = Math.round(pct);
  if (target === st.target && !immediate) return;
  st.target = target;
  barAnims = barAnims.filter((a) => a.fill !== fill);
  // kleine drifts (leerlauf-zittern) springen im selben frame wie die
  // text-updates; nur deutliche änderungen bekommen den weichen übergang.
  // hält den leerlauf bei ~1 frame pro tick (harte 1%-cpu-vorgabe)
  if (immediate || Math.abs(target - st.shown) < 4) {
    st.shown = target;
    fill.style.transform = `scaleX(${target / 100})`;
    return;
  }
  barAnims.push({ fill, st, from: st.shown, step: 0 });
  if (barClock === null) {
    barClock = window.setInterval(tickBarClock, BAR_DURATION_MS / BAR_STEPS);
  }
}

function setGauge(name: string, pct: number | null): void {
  const bar = el(`${name}-bar`);
  if (pct === null) {
    setText(el(`${name}-val`), 'n/a');
    setBar(bar, 0, null, false);
    return;
  }
  setText(el(`${name}-val`), fmtPct(pct));
  setBar(bar, pct, thresholdColor(pct), false);
}

function setValue(id: string, v: number | null, fmt: (v: number) => string, empty = 'n/a'): void {
  setText(el(id), v === null ? empty : fmt(v));
}

function render(s: Stats): void {
  lastFans = s.fans;
  if (collapsed) {
    // nur die zwei sichtbaren werte anfassen — alles andere ist
    // display:none und würde trotzdem style-arbeit kosten
    setValue('c-cpu', s.cpu, fmtPct);
    setValue('c-ram', s.ram, fmtPct);
    return;
  }
  setGauge('cpu', s.cpu);
  setGauge('ram', s.ram);
  setText(
    el('ram-abs'),
    s.ramUsed !== null && s.ramTotal !== null ? fmtRamAbs(s.ramUsed, s.ramTotal) : '',
  );
  setGauge('disk', s.disk);
  setGauge('gpu', s.gpu);
  setValue('temp-cpu', s.tempCpu, fmtTemp);
  setValue('temp-gpu', s.tempGpu, fmtTemp);
  // net ist immer vorhanden, nur beim ersten Tick ohne Delta -> "–"
  setValue('net-up', s.netUp, fmtMbs, '–');
  setValue('net-down', s.netDown, fmtMbs, '–');
  setValue('pwr-val', s.pwr, fmtWatt);
  setText(el('ip-val'), s.ip ?? 'n/a');
  renderFans(s);

}

// --- top-10-dropdown für cpu / ram / gpu ---------------------------------

let openKind: string | null = null;
let topListEl: HTMLElement | null = null;
let topFetching = false;
let topTimer: number | null = null;

function toggleTopList(kind: string): void {
  if (topListEl) {
    topListEl.remove();
    topListEl = null;
  }
  if (topTimer !== null) {
    clearInterval(topTimer);
    topTimer = null;
  }
  if (openKind === kind) {
    openKind = null;
    void fitWindow();
    return;
  }
  openKind = kind;
  // eigener takt: stats-events pausieren, wenn sich nichts ändert —
  // die liste soll trotzdem aktuell bleiben, solange sie offen ist
  topTimer = window.setInterval(() => void refreshTopList(), 2000);
  topListEl = document.createElement('div');
  topListEl.className = 'top-list';
  topListEl.innerHTML = '<span class="top-empty">lade …</span>';
  el(`row-${kind}`).appendChild(topListEl);
  void fitWindow();
  void refreshTopList(true);
}

async function refreshTopList(force = false): Promise<void> {
  if (!openKind || !topListEl || topFetching) return;
  if (!force && topListEl.childElementCount === 0) return;
  topFetching = true;
  try {
    const list = await window.__TAURI__.core.invoke<TopList>('top_processes', {
      kind: openKind,
    });
    if (!topListEl) return;
    const rendered = list.entries.map((e) => ({
      name: e.name,
      val: e.unit === 'pct' ? fmtPctFine(e.value) : fmtBytes(e.value),
      count: e.count,
    }));
    const signature = JSON.stringify([list.warning, rendered]);
    if (topListEl.dataset.sig === signature) return;
    const prevCount = topListEl.childElementCount;
    topListEl.dataset.sig = signature;
    topListEl.innerHTML = '';
    if (list.warning) {
      const warn = document.createElement('span');
      warn.className = 'top-empty';
      warn.textContent = `! ${list.warning}`;
      topListEl.appendChild(warn);
    }
    if (rendered.length === 0 && !list.warning) {
      topListEl.innerHTML = '<span class="top-empty">keine prozesse</span>';
    }
    for (const entry of rendered) {
      const row = document.createElement('div');
      row.className = 'top-row';
      const name = document.createElement('span');
      name.className = 'top-name';
      name.textContent = entry.name;
      name.title = entry.name;
      const val = document.createElement('span');
      val.className = 'top-val';
      val.textContent = entry.val;
      if (entry.count !== null && entry.count > 1) {
        const count = document.createElement('span');
        count.className = 'top-count';
        count.textContent = ` (${entry.count})`;
        val.appendChild(count);
      }
      row.append(name, val);
      topListEl.appendChild(row);
    }
    if (topListEl.childElementCount !== prevCount) void fitWindow();
  } catch (err) {
    console.error(err);
  } finally {
    topFetching = false;
  }
}

// --- lüfter-sektion (dynamisch, weil die anzahl kanäle je system variiert) ---

interface FanRow {
  mode: HTMLElement;
  rpm: HTMLElement;
  fill: HTMLElement | null;
}

const fanRows = new Map<string, FanRow>();
let groupRow: FanRow | null = null;
let fanSignature = '';
/** id -> zeitstempel, bis zu dem ticks die anzeige nicht überschreiben dürfen */
const fanHold = new Map<string, number>();
/** letzter bekannter zustand pro lüfter, für die umschalt-logik */
let lastFans: FanStat[] = [];

const GROUP_ID = '__group__';

function controllable(fans: FanStat[]): FanStat[] {
  return fans.filter((f) => f.pct !== null && f.autoMode !== null);
}

function invokeSetFan(id: string, pct: number | null, mode: HTMLElement): void {
  const args: Record<string, unknown> = pct === null ? { id } : { id, pct };
  window.__TAURI__.core.invoke('set_fan', args).catch((err) => {
    console.error(err);
    setText(mode, 'denied');
  });
}

function buildFanRows(s: Stats): void {
  const box = el('fans');
  box.innerHTML = '';
  fanRows.clear();
  groupRow = null;

  if (s.gpuFan !== null) {
    const line = document.createElement('div');
    line.className = 'row-line';
    line.innerHTML = '<span class="label">fan gpu</span><span class="value" id="fan-gpu-val">–</span>';
    box.appendChild(line);
  }

  // gruppen-regler, wenn mehr als ein kanal steuerbar ist
  if (controllable(s.fans).length > 1) {
    const { row, parts } = makeFanRow('fans', GROUP_ID, true);
    groupRow = parts;
    box.appendChild(row);
  }

  for (const fan of s.fans) {
    const { row, parts } = makeFanRow(fan.label, fan.id, fan.pct !== null);
    box.appendChild(row);
    fanRows.set(fan.id, parts);
  }

  box.hidden = s.gpuFan === null && s.fans.length === 0;
}

function makeFanRow(
  labelText: string,
  id: string,
  withSlider: boolean,
): { row: HTMLElement; parts: FanRow } {
  const row = document.createElement('div');
  row.className = 'row row-bar';
  const line = document.createElement('div');
  line.className = 'row-line';
  const label = document.createElement('span');
  label.className = 'label';
  label.textContent = labelText;
  const value = document.createElement('span');
  value.className = 'value value-pair';
  const mode = document.createElement('span');
  mode.className = 'fan-mode';
  const rpm = document.createElement('span');
  value.append(mode, rpm);
  line.append(label, value);
  row.appendChild(line);

  let fill: HTMLElement | null = null;
  if (withSlider) {
    const hit = document.createElement('div');
    hit.className = 'slider-hit';
    const bar = document.createElement('div');
    bar.className = 'bar';
    fill = document.createElement('div');
    fill.className = 'bar-fill';
    bar.appendChild(fill);
    hit.appendChild(bar);
    row.appendChild(hit);
    attachSlider(hit, bar, fill, id, mode);
    mode.addEventListener('click', () => toggleFanMode(id, mode));
    mode.title = 'klick: auto/manuell umschalten';
  }

  return { row, parts: { mode, rpm, fill } };
}

/** auto <-> manuell umschalten; manuell startet beim aktuellen pwm-wert */
function toggleFanMode(id: string, mode: HTMLElement): void {
  const hold = performance.now() + 3000;
  if (id === GROUP_ID) {
    const fans = controllable(lastFans);
    const allAuto = fans.every((f) => f.autoMode !== false);
    fanHold.set(GROUP_ID, hold);
    for (const fan of fans) {
      fanHold.set(fan.id, hold);
      invokeSetFan(fan.id, allAuto ? (fan.pct ?? 50) : null, mode);
    }
    setMode(mode, allAuto ? 'man' : 'auto', allAuto);
    return;
  }
  const fan = lastFans.find((f) => f.id === id);
  if (!fan) return;
  fanHold.set(id, hold);
  if (fan.autoMode === false) {
    setMode(mode, 'auto', false);
    invokeSetFan(id, null, mode);
  } else {
    const pct = fan.pct ?? 50;
    setMode(mode, fmtPct(pct), true);
    invokeSetFan(id, pct, mode);
  }
}

function renderFans(s: Stats): void {
  lastFans = s.fans;
  const signature = `${s.gpuFan !== null}|${s.fans
    .map((f) => `${f.id}${f.pct !== null}`)
    .join(',')}`;
  if (signature !== fanSignature) {
    fanSignature = signature;
    buildFanRows(s);
    void fitWindow();
  }

  if (s.gpuFan !== null) {
    const gpuVal = document.getElementById('fan-gpu-val');
    if (gpuVal) gpuVal.textContent = fmtPct(s.gpuFan);
  }

  const now = performance.now();
  for (const fan of s.fans) {
    const row = fanRows.get(fan.id);
    if (!row) continue;
    setText(row.rpm, fan.rpm === null ? 'n/a' : fmtRpm(fan.rpm));
    if ((fanHold.get(fan.id) ?? 0) > now) continue; // nutzer interagiert gerade
    if (fan.pct !== null && row.fill) {
      setBar(row.fill, fan.pct, null, fan.autoMode !== false);
    }
    if (fan.autoMode === null) {
      setMode(row.mode, '', false);
    } else if (fan.autoMode) {
      setMode(row.mode, 'auto', false);
    } else {
      setMode(row.mode, fan.pct === null ? 'man' : fmtPct(fan.pct), true);
    }
  }

  if (groupRow && (fanHold.get(GROUP_ID) ?? 0) <= now) {
    const fans = controllable(s.fans);
    const autos = fans.filter((f) => f.autoMode !== false).length;
    const avg = fans.reduce((sum, f) => sum + (f.pct ?? 0), 0) / Math.max(fans.length, 1);
    if (groupRow.fill) {
      setBar(groupRow.fill, avg, null, autos > 0);
    }
    if (autos === fans.length) {
      setMode(groupRow.mode, 'auto', false);
    } else if (autos === 0) {
      setMode(groupRow.mode, fmtPct(avg), true);
    } else {
      setMode(groupRow.mode, 'mix', true);
    }
  }
}

function setMode(node: HTMLElement, text: string, manual: boolean): void {
  setText(node, text);
  if (node.classList.contains('manual') !== manual) {
    node.classList.toggle('manual', manual);
  }
}

function attachSlider(
  hit: HTMLElement,
  bar: HTMLElement,
  fill: HTMLElement,
  id: string,
  mode: HTMLElement,
): void {
  let dragging = false;

  const pctFromEvent = (e: PointerEvent): number => {
    const rect = bar.getBoundingClientRect();
    return Math.max(0, Math.min(100, ((e.clientX - rect.left) / rect.width) * 100));
  };

  const preview = (pct: number): void => {
    setBar(fill, pct, null, false, true); // beim ziehen sofort, ohne easing
    setMode(mode, fmtPct(pct), true);
  };

  const holdIds = (until: number): void => {
    fanHold.set(id, until);
    if (id === GROUP_ID) {
      for (const fan of controllable(lastFans)) fanHold.set(fan.id, until);
    }
  };

  hit.addEventListener('pointerdown', (e) => {
    dragging = true;
    hit.setPointerCapture(e.pointerId);
    holdIds(Number.POSITIVE_INFINITY);
    preview(pctFromEvent(e));
  });
  hit.addEventListener('pointermove', (e) => {
    if (dragging) preview(pctFromEvent(e));
  });
  hit.addEventListener('pointerup', (e) => {
    if (!dragging) return;
    dragging = false;
    const pct = pctFromEvent(e);
    preview(pct);
    // dem chip zeit geben, den wert zu übernehmen, bevor ticks wieder rendern
    holdIds(performance.now() + 3000);
    if (id === GROUP_ID) {
      for (const fan of controllable(lastFans)) invokeSetFan(fan.id, pct, mode);
    } else {
      invokeSetFan(id, pct, mode);
    }
  });
}

let lastWindowSize = '';

// fensterhöhe an den inhalt anpassen (breite bleibt fix)
async function fitWindow(): Promise<void> {
  const panel = el('panel');
  const { LogicalSize } = window.__TAURI__.dpi;
  const win = window.__TAURI__.window.getCurrentWindow();
  const width = panel.offsetWidth + SHADOW_MARGIN * 2;
  const height = panel.offsetHeight + SHADOW_MARGIN * 2;
  const size = `${width}x${height}`;
  if (size === lastWindowSize) return;
  lastWindowSize = size;
  await win.setSize(new LogicalSize(width, height));
}

// --- zustand normal/kollabiert -------------------------------------------

let collapsed = false;

function applyCollapsed(value: boolean): void {
  collapsed = value;
  if (collapsed && barClock !== null) {
    clearInterval(barClock);
    barClock = null;
    barAnims = [];
  }
  el('panel').classList.toggle('collapsed', collapsed);
  void fitWindow();
}

function toggleCollapsed(): void {
  applyCollapsed(!collapsed);
  window.__TAURI__.core.invoke('set_collapsed', { collapsed }).catch(console.error);
}

interface SettingsPayload {
  collapsed: boolean;
}

function initClickHandlers(): void {
  for (const line of document.querySelectorAll<HTMLElement>('.expandable')) {
    const kind = line.dataset.kind;
    if (kind) line.addEventListener('click', () => toggleTopList(kind));
  }
  el('toggle').addEventListener('click', toggleCollapsed);
  // rechtsklick: natives kontextmenü statt webview-menü
  window.addEventListener('contextmenu', (e) => {
    e.preventDefault();
    window.__TAURI__.core.invoke('context_menu').catch(console.error);
  });
}

async function init(): Promise<void> {
  initClickHandlers();
  try {
    const settings = await window.__TAURI__.core.invoke<SettingsPayload>('get_settings');
    if (settings.collapsed) applyCollapsed(true);
  } catch (err) {
    console.error(err);
  }
  await window.__TAURI__.event.listen<Stats>('stats', (e) => render(e.payload));
  await document.fonts.ready;
  await fitWindow();
  // sicherheitsnetz gegen layout-messungen vor fertigem font-rendering
  setTimeout(() => void fitWindow(), 2500);
}

init().catch((err) => console.error(err));

export {};
