// Webview: ER diagram in SVG. Measures boxes with the editor font, lets the engine lay them out (through the
// extension), then draws only what's in view: boxes and relationship curves outside the viewport aren't in the DOM, and
// far out rows become bars (or disappear), so ~300 tables stay smooth. Pan, zoom, drag, hover, search, inspector,
// export.
import './er.css';
import type { Entity, ErFromWebview, ErToWebview, Point, Relationship, SchemaModel, Size } from '../src/views/protocol.ts';
import { SVG_NS, button, cssVar, el, host, measurer, svg } from './host.ts';
import {
  Metrics,
  badge,
  boundsOf,
  centreOn,
  compactRows,
  curve,
  curvePath,
  ellipsize,
  entitySize,
  exportScale,
  fit,
  headerColor,
  hull,
  intersects,
  marks,
  neighbours,
  search,
  visibleAttributes,
  worldViewport,
  zoomAt,
  type Measure,
  type Rect,
  type View,
} from './lib/er.ts';

interface UiState {
  view: View;
  selected: Selected;
  inspector: boolean;
}

type Selected = { kind: 'entity'; id: string } | { kind: 'rel'; id: string } | undefined;
type Detail = 'full' | 'greek' | 'outline';

const vscode = host<ErToWebview, ErFromWebview, UiState>();

let model: SchemaModel | undefined;
let byId = new Map<string, Entity>();
let relsOf = new Map<string, Relationship[]>();
let sizes: Record<string, Size> = {};
let positions: Record<string, Point> = {};
let view: View = vscode.state().view ?? { x: 0, y: 0, zoom: 1 };
let hasView = !!vscode.state().view;
let selected: Selected = vscode.state().selected;
let hovered: string | undefined;
let matches: string[] = [];
let matchIndex = -1;
let detail: Detail = 'full';

// MARK: - DOM skeleton

const titleEl = el('span', { class: 'title' });
const searchInput = el('input', { class: 'search', type: 'search', placeholder: 'Find table…', 'aria-label': 'Find table' });
const matchLabel = el('span', { class: 'matches' });
const zoomLabel = el('span', { class: 'zoom', title: 'Zoom' });
const inspectorBtn = button('Inspector', 'Show or hide the inspector', () => toggleInspector());
const toolbar = el(
  'div',
  { class: 'toolbar' },
  titleEl,
  searchInput,
  matchLabel,
  el('span', { class: 'spacer' }),
  button('−', 'Zoom out', () => zoomBy(1 / 1.25)),
  zoomLabel,
  button('+', 'Zoom in', () => zoomBy(1.25)),
  button('Fit', 'Fit the whole diagram', () => fitAll()),
  button('Auto Layout', 'Lay out again (discards moved positions)', () => model && vscode.post({ type: 'autoLayout' })),
  button('Reload', 'Read the schema again', () => vscode.post({ type: 'reload' })),
  button('SVG', 'Export as SVG', () => void exportAs('svg')),
  button('PNG', 'Export as PNG', () => void exportAs('png')),
  inspectorBtn,
);
const progress = el('div', { class: 'progress', hidden: '' });
const bannerText = el('span');
const bannerBtn = el('button', { class: 'btn', type: 'button' });
const banner = el('div', { class: 'banner', hidden: '' }, bannerText, bannerBtn);
const edgeLayer = svg('g', { class: 'edges' });
const entityLayer = svg('g', { class: 'entities' });
const world = svg('g', { class: 'world' }, edgeLayer, entityLayer);
const canvas = svg('svg', { class: 'er', role: 'img', 'aria-label': 'ER diagram' }, world);
const stage = el('div', { class: 'stage', tabindex: '0' }, canvas);
const inspector = el('aside', { class: 'inspector' });
const main = el('div', { class: 'main' }, stage, inspector);
document.getElementById('app')!.append(toolbar, progress, banner, main);
inspector.hidden = vscode.state().inspector === false;
inspectorBtn.classList.toggle('on', !inspector.hidden);

// MARK: - Messages

vscode.listen((m) => {
  switch (m.type) {
    case 'init':
      titleEl.textContent = m.title;
      break;
    case 'loading':
      showBanner(m.message, 'Cancel', () => vscode.post({ type: 'cancel' }));
      progress.hidden = false;
      break;
    case 'error':
      progress.hidden = true;
      showBanner(m.message, 'Retry', () => vscode.post({ type: 'reload' }), true);
      break;
    case 'model':
      setModel(m.model);
      if (m.positions && m.sizes) {
        sizes = m.sizes;
        setPositions(m.positions);
      } else {
        sizes = measureAll(m.model);
        showBanner(`Laying out ${m.model.entities.length} tables…`);
        progress.hidden = false;
        vscode.post({ type: 'sizes', sizes });
      }
      break;
    case 'layout':
      setPositions(m.positions);
      break;
    case 'exported':
      showBanner(`Exported to ${m.path}`, 'OK', () => (banner.hidden = true));
      break;
  }
});
vscode.post({ type: 'ready' });

function showBanner(text: string, action?: string, run?: () => void, error = false) {
  banner.hidden = false;
  banner.classList.toggle('error', error);
  bannerText.textContent = text;
  bannerBtn.hidden = !action;
  bannerBtn.textContent = action ?? '';
  bannerBtn.onclick = run ?? null;
}

function setModel(m: SchemaModel) {
  model = m;
  byId = new Map(m.entities.map((e) => [e.id, e]));
  relsOf = new Map();
  for (const r of m.relationships) {
    for (const id of new Set([r.fromEntity, r.toEntity])) {
      if (!relsOf.has(id)) relsOf.set(id, []);
      relsOf.get(id)!.push(r);
    }
  }
  if (selected && !(selected.kind === 'entity' ? byId.has(selected.id) : m.relationships.some((r) => r.id === selected!.id)))
    selected = undefined;
  clearRendered();
  updateSearch();
}

function setPositions(p: Record<string, Point>) {
  positions = { ...p };
  progress.hidden = true;
  if (!banner.classList.contains('error')) banner.hidden = true;
  if (!hasView) fitAll();
  else applyView();
  renderInspector();
}

// MARK: - Measuring (the editor font, so boxes fit what's drawn)

const measureText = measurer();
const fonts = () => ({
  title: `600 13px ${cssVar('--vscode-font-family', 'sans-serif')}`,
  row: `11.5px ${cssVar('--vscode-editor-font-family', 'monospace')}`,
  rowBold: `bold 11.5px ${cssVar('--vscode-editor-font-family', 'monospace')}`,
  type: `10.5px ${cssVar('--vscode-font-family', 'sans-serif')}`,
});
let fontSet = fonts();
const measure: Measure = (text, font) => measureText(text, fontSet[font]);

function measureAll(m: SchemaModel): Record<string, Size> {
  fontSet = fonts();
  return Object.fromEntries(m.entities.map((e) => [e.id, entitySize(e, measure)]));
}

function rectOf(id: string): Rect | undefined {
  const p = positions[id];
  const s = sizes[id];
  return p && s ? { x: p.x, y: p.y, width: s.width, height: s.height } : undefined;
}

// MARK: - Rendering (only what's in view)

const renderedEntities = new Map<string, { g: SVGGElement; x: number; y: number }>();
const renderedRels = new Map<string, { g: SVGGElement; d: string }>();
let frame = 0;

function schedule() {
  if (!frame) frame = requestAnimationFrame(() => ((frame = 0), render()));
}

function clearRendered() {
  entityLayer.replaceChildren();
  edgeLayer.replaceChildren();
  renderedEntities.clear();
  renderedRels.clear();
}

function viewportSize(): Size {
  return { width: stage.clientWidth, height: stage.clientHeight };
}

function applyView() {
  world.setAttribute('transform', `translate(${view.x} ${view.y}) scale(${view.zoom})`);
  zoomLabel.textContent = `${Math.round(view.zoom * 100)}%`;
  const d: Detail = view.zoom < 0.18 ? 'outline' : view.zoom < Metrics.greekBelow ? 'greek' : 'full';
  if (d !== detail) {
    detail = d;
    entityLayer.replaceChildren();
    renderedEntities.clear();
  }
  schedule();
  saveView();
}

let saveTimer = 0;
function saveView() {
  if (!Object.keys(positions).length) return; // nothing drawn yet: let the first layout fit
  clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => vscode.save({ view, selected }), 200);
}

function render() {
  if (!model) return;
  const vp = viewportSize();
  const w = worldViewport(view, vp);
  const margin = 120 / view.zoom;
  const visible: Rect = { x: w.x - margin, y: w.y - margin, width: w.width + 2 * margin, height: w.height + 2 * margin };
  const focus = focusSet();
  const keep = new Set<string>();
  for (const e of model.entities) {
    const r = rectOf(e.id);
    if (!r || !intersects(r, visible)) continue;
    keep.add(e.id);
    let item = renderedEntities.get(e.id);
    if (!item) {
      item = { g: entityElement(e, r, detail), x: NaN, y: NaN };
      entityLayer.append(item.g);
      renderedEntities.set(e.id, item);
    }
    if (item.x !== r.x || item.y !== r.y) {
      item.g.setAttribute('transform', `translate(${r.x} ${r.y})`);
      [item.x, item.y] = [r.x, r.y];
    }
    entityClasses(item.g, e.id, focus);
  }
  for (const [id, item] of renderedEntities) if (!keep.has(id)) (item.g.remove(), renderedEntities.delete(id));
  const keepRels = new Set<string>();
  for (const rel of model.relationships) {
    const child = byId.get(rel.fromEntity);
    const parent = byId.get(rel.toEntity);
    const cr = rectOf(rel.fromEntity);
    const pr = rectOf(rel.toEntity);
    if (!child || !parent || !cr || !pr) continue;
    const c = curve(rel, child, parent, cr, pr);
    if (!intersects(hull(c), visible)) continue;
    keepRels.add(rel.id);
    const d = curvePath(c);
    let item = renderedRels.get(rel.id);
    if (!item || item.d !== d) {
      const g = relElement(rel, c);
      if (item) item.g.replaceWith(g);
      else edgeLayer.append(g);
      item = { g, d };
      renderedRels.set(rel.id, item);
    }
    relClasses(item.g, rel, focus);
  }
  for (const [id, item] of renderedRels) if (!keepRels.has(id)) (item.g.remove(), renderedRels.delete(id));
}

/** Entities to keep at full strength: the selection (or hovered box) and its neighbours, else search matches. */
function focusSet(): Set<string> | undefined {
  if (selected?.kind === 'entity') return neighbours(model!.relationships, selected.id);
  if (selected?.kind === 'rel') {
    const r = model!.relationships.find((x) => x.id === selected!.id);
    return r ? new Set([r.fromEntity, r.toEntity]) : undefined;
  }
  if (hovered) return neighbours(model!.relationships, hovered);
  return matches.length ? new Set(matches) : undefined;
}

function entityClasses(g: SVGGElement, id: string, focus: Set<string> | undefined) {
  g.classList.toggle('dim', !!focus && !focus.has(id));
  g.classList.toggle('sel', selected?.kind === 'entity' && selected.id === id);
  g.classList.toggle('match', matches.includes(id) && !selected);
}

function relClasses(g: SVGGElement, rel: Relationship, focus: Set<string> | undefined) {
  const anchor = selected?.kind === 'entity' ? selected.id : hovered;
  const active = selected?.kind === 'rel' ? selected.id === rel.id : !!anchor && (rel.fromEntity === anchor || rel.toEntity === anchor);
  const off = !active && selected?.kind === 'entity';
  g.classList.toggle('active', active);
  g.classList.toggle('off', off);
  g.classList.toggle('dim', !active && !off && !!focus && !(focus.has(rel.fromEntity) && focus.has(rel.toEntity)));
}

function refreshClasses() {
  if (!model) return;
  const focus = focusSet();
  for (const [id, item] of renderedEntities) entityClasses(item.g, id, focus);
  for (const rel of model.relationships) {
    const item = renderedRels.get(rel.id);
    if (item) relClasses(item.g, rel, focus);
  }
}

/** One table box in local coordinates (the group is translated to its position). */
function entityElement(e: Entity, r: Rect, level: Detail): SVGGElement {
  const { width: w, height: h } = r;
  const H = Metrics.header;
  const isView = e.kind === 'view' || e.kind === 'materializedView';
  const g = svg('g', { class: `entity${isView ? ' view' : ''}`, 'data-id': e.id });
  g.append(svg('rect', { class: 'box', width: w, height: h, rx: 7 }));
  const color = headerColor(e.schema);
  g.append(
    svg('path', {
      class: `head${color ? '' : ' accent'}`,
      d: `M0,${H} V7 A7,7 0 0 1 7,0 H${w - 7} A7,7 0 0 1 ${w},7 V${H} Z`,
      fill: color,
    }),
  );
  const count = e.rowEstimate && e.rowEstimate > 0 && w > 200 ? compactRows(e.rowEstimate) : '';
  const countW = count ? measureText(count, `9.5px ${cssVar('--vscode-font-family', 'sans-serif')}`) + 12 : 0;
  const title = ellipsize((isView ? '◇ ' : '') + e.name, w - Metrics.padding * 2 - countW, (t) => measure(t, 'title'));
  g.append(text('title', Metrics.padding, H / 2, title));
  if (count) g.append(text('count', w - 8, H / 2, count, 'end'));
  const attrs = visibleAttributes(e);
  if (level === 'outline') g.append(svg('rect', { class: 'rows', x: 4, y: H + 4, width: w - 8, height: Math.max(0, h - H - 8), rx: 3 }));
  else {
    attrs.forEach((a, i) => {
      const y = H + i * Metrics.row;
      const mid = y + Metrics.row / 2;
      if (i % 2 === 1) g.append(svg('rect', { class: 'stripe', x: 1, y, width: w - 2, height: Metrics.row }));
      const b = badge(a);
      if (level === 'greek') {
        if (b) g.append(svg('rect', { class: `bar ${b.toLowerCase()}`, x: 8, y: mid - 3, width: 12, height: 6 }));
        g.append(svg('rect', { class: 'bar name', x: 28, y: mid - 3, width: Math.max(0, Math.min(w - 90, a.name.length * 7)), height: 6 }));
        const tw = Math.min(60, a.type.length * 5.5);
        g.append(svg('rect', { class: 'bar type', x: w - Metrics.padding - tw, y: mid - 3, width: tw, height: 6 }));
        return;
      }
      if (b) g.append(text(`kbadge ${b.toLowerCase()}`, 8, mid, b));
      const typeW = measure(a.type, 'type');
      const nameW = Math.max(40, w - 28 - Metrics.padding - typeW - 8);
      const cls = `col${a.nullable ? '' : ' nn'}${(a.presence ?? 1) < 1 ? ' partial' : ''}`;
      g.append(text(cls, 28, mid, ellipsize(a.name, nameW, (t) => measureText(t, a.nullable ? fontSet.row : fontSet.rowBold))));
      g.append(text('type', w - Metrics.padding, mid, a.type, 'end'));
    });
    if (e.attributes.length > attrs.length && level === 'full') {
      const y = H + attrs.length * Metrics.row + Metrics.row / 2;
      g.append(text('more', 28, y, `… ${e.attributes.length - attrs.length} more`));
    }
  }
  g.append(svg('rect', { class: 'outline', width: w, height: h, rx: 7 }));
  return g;
}

function text(cls: string, x: number, y: number, content: string, anchor?: 'end'): SVGTextElement {
  const t = svg('text', { class: cls, x, y, 'text-anchor': anchor, 'dominant-baseline': 'central' });
  t.textContent = content;
  return t;
}

function relElement(rel: Relationship, c: ReturnType<typeof curve>): SVGGElement {
  const d = curvePath(c);
  const m = marks(rel, c);
  const g = svg(
    'g',
    { class: `rel${rel.inferred ? ' inferred' : ''}`, 'data-id': rel.id },
    svg('path', { class: 'hit', d }),
    svg('path', { class: 'line', d }),
    svg('path', { class: 'mark', d: m.lines }),
  );
  for (const p of m.circles) g.append(svg('circle', { class: 'mark', cx: p.x, cy: p.y, r: 4 }));
  const title = svg('title');
  title.textContent = `${rel.fromEntity}(${rel.fromColumns.join(', ')}) → ${rel.toEntity}(${rel.toColumns.join(', ')})`;
  g.append(title);
  return g;
}

// MARK: - Pan, zoom, drag, hover, select

function zoomBy(factor: number, at?: Point) {
  const vp = viewportSize();
  view = zoomAt(view, factor, at ?? { x: vp.width / 2, y: vp.height / 2 });
  hasView = true;
  applyView();
}

function fitAll() {
  const b = boundsOf(Object.keys(positions).map(rectOf).filter((r): r is Rect => !!r));
  if (!b) return;
  view = fit(b, viewportSize());
  hasView = true;
  applyView();
}

stage.addEventListener(
  'wheel',
  (e) => {
    e.preventDefault();
    const b = stage.getBoundingClientRect();
    if (e.ctrlKey || e.metaKey) {
      // Pinch on a trackpad arrives as ctrl+wheel; a mouse wheel with ⌘/Ctrl zooms too.
      zoomBy(Math.exp(-e.deltaY * (e.deltaMode === 1 ? 0.05 : 0.0025) * 4), { x: e.clientX - b.left, y: e.clientY - b.top });
    } else {
      const k = e.deltaMode === 1 ? 16 : 1;
      view = { ...view, x: view.x - (e.shiftKey ? e.deltaY : e.deltaX) * k, y: view.y - (e.shiftKey ? 0 : e.deltaY) * k };
      hasView = true;
      applyView();
    }
  },
  { passive: false },
);

type Gesture =
  | { kind: 'pan'; x0: number; y0: number; view: View; moved: boolean }
  | { kind: 'drag'; id: string; x0: number; y0: number; start: Point; moved: boolean }
  | { kind: 'rel'; id: string };
let gesture: Gesture | undefined;

stage.addEventListener('pointerdown', (e) => {
  if (e.button !== 0 || !model) return;
  stage.focus();
  const target = e.target as Element;
  const entity = target.closest('.entity')?.getAttribute('data-id');
  const rel = target.closest('.rel')?.getAttribute('data-id');
  stage.setPointerCapture(e.pointerId);
  if (entity && positions[entity]) gesture = { kind: 'drag', id: entity, x0: e.clientX, y0: e.clientY, start: { ...positions[entity]! }, moved: false };
  else if (rel) gesture = { kind: 'rel', id: rel };
  else gesture = { kind: 'pan', x0: e.clientX, y0: e.clientY, view, moved: false };
  stage.classList.toggle('panning', gesture.kind === 'pan');
});

stage.addEventListener('pointermove', (e) => {
  if (!gesture) {
    const id = (e.target as Element).closest('.entity')?.getAttribute('data-id') ?? undefined;
    if (id !== hovered) {
      hovered = id;
      refreshClasses();
    }
    return;
  }
  if (gesture.kind === 'pan') {
    const dx = e.clientX - gesture.x0, dy = e.clientY - gesture.y0;
    gesture.moved ||= Math.abs(dx) + Math.abs(dy) > 3;
    view = { ...gesture.view, x: gesture.view.x + dx, y: gesture.view.y + dy };
    hasView = true;
    applyView();
  } else if (gesture.kind === 'drag') {
    const dx = (e.clientX - gesture.x0) / view.zoom, dy = (e.clientY - gesture.y0) / view.zoom;
    if (!gesture.moved && Math.abs(dx) + Math.abs(dy) < 3 / view.zoom) return;
    gesture.moved = true;
    positions[gesture.id] = { x: Math.round(gesture.start.x + dx), y: Math.round(gesture.start.y + dy) };
    stage.classList.add('dragging');
    schedule();
  }
});

const endGesture = () => {
  const g = gesture;
  gesture = undefined;
  stage.classList.remove('panning', 'dragging');
  if (!g) return;
  if (g.kind === 'drag' && g.moved) {
    vscode.post({ type: 'moved', positions: { [g.id]: positions[g.id]! } });
  } else if (g.kind === 'drag') {
    select({ kind: 'entity', id: g.id });
  } else if (g.kind === 'rel') {
    select({ kind: 'rel', id: g.id });
  } else if (!g.moved) {
    select(undefined);
  }
};
stage.addEventListener('pointerup', endGesture);
stage.addEventListener('pointercancel', endGesture);
stage.addEventListener('pointerleave', () => {
  if (!gesture && hovered) {
    hovered = undefined;
    refreshClasses();
  }
});

function select(s: Selected, reveal = false) {
  selected = s;
  if (s && inspector.hidden && stage.clientWidth > 500) toggleInspector(true);
  refreshClasses();
  renderInspector();
  saveView();
  if (reveal && s?.kind === 'entity') {
    const r = rectOf(s.id);
    if (r) {
      view = centreOn(view, { x: r.x + r.width / 2, y: r.y + r.height / 2 }, viewportSize());
      applyView();
    }
  }
}

function toggleInspector(show = inspector.hidden) {
  inspector.hidden = !show;
  inspectorBtn.classList.toggle('on', show);
  vscode.save({ inspector: show });
  schedule();
}

new ResizeObserver(schedule).observe(stage);

stage.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') select(undefined);
  else if (e.key === '+' || e.key === '=') zoomBy(1.25);
  else if (e.key === '-') zoomBy(1 / 1.25);
  else if (e.key === '0') fitAll();
});
window.addEventListener('keydown', (e) => {
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'f') {
    e.preventDefault();
    searchInput.focus();
    searchInput.select();
  }
});

// MARK: - Search

function updateSearch() {
  matches = model ? search(model.entities, searchInput.value) : [];
  matchIndex = -1;
  matchLabel.textContent = searchInput.value.trim() ? `${matches.length} found` : '';
  matchLabel.classList.toggle('none', !!searchInput.value.trim() && !matches.length);
  if (matches.length && selected?.kind === 'entity' && !matches.includes(selected.id)) selected = undefined;
  refreshClasses();
}
searchInput.addEventListener('input', updateSearch);
searchInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && matches.length) {
    matchIndex = (matchIndex + (e.shiftKey ? matches.length - 1 : 1)) % matches.length;
    matchLabel.textContent = `${matchIndex + 1} of ${matches.length}`;
    select({ kind: 'entity', id: matches[matchIndex]! }, true);
  } else if (e.key === 'Escape') {
    searchInput.value = '';
    updateSearch();
    stage.focus();
  }
});

// MARK: - Inspector

function renderInspector() {
  inspector.replaceChildren();
  if (!model) return;
  if (selected?.kind === 'entity' && byId.has(selected.id)) inspectEntity(byId.get(selected.id)!);
  else if (selected?.kind === 'rel') {
    const r = model.relationships.find((x) => x.id === selected!.id);
    if (r) inspectRelationship(r);
  } else inspectDiagram(model);
}

function section(title: string, ...children: (Node | null)[]): HTMLElement {
  return el('section', {}, el('h3', { text: title }), ...children);
}

function link(label: string, run: () => void, title?: string): HTMLButtonElement {
  const b = el('button', { class: 'link', type: 'button', text: label, title });
  b.addEventListener('click', run);
  return b;
}

const goTo = (id: string) => select({ kind: 'entity', id }, true);

function inspectDiagram(m: SchemaModel) {
  inspector.append(
    el('h2', { text: m.database }),
    el('p', { class: 'muted', text: `${m.entities.length} tables · ${m.relationships.length} relationships · ${m.engine}` }),
  );
  if (m.warnings.length) inspector.append(section('Warnings', el('ul', { class: 'warnings' }, ...m.warnings.map((w) => el('li', { text: w })))));
  inspector.append(
    section(
      'Legend',
      el(
        'ul',
        { class: 'legend' },
        el('li', {}, el('b', { class: 'kbadge pk', text: 'PK' }), ' primary key  ', el('b', { class: 'kbadge fk', text: 'FK' }), ' foreign key  ', el('b', { class: 'kbadge uq', text: 'UQ' }), ' unique'),
        el('li', { text: 'Bold column: NOT NULL. Italic: missing from some sampled documents.' }),
        el('li', { text: 'Crow’s foot at the child; “||” at the parent, “o|” when the foreign key is nullable.' }),
        m.relationships.some((r) => r.inferred) ? el('li', { text: 'Dashed lines are inferred from sampled documents.' }) : null,
      ),
    ),
    el('p', { class: 'muted', text: 'Click a table or line to inspect it. Drag tables to rearrange; positions are saved for this workspace.' }),
  );
}

function inspectEntity(e: Entity) {
  inspector.append(
    el('h2', { text: e.name, title: e.id }),
    el('p', { class: 'muted', text: [e.schema, e.kind, e.rowEstimate ? compactRows(e.rowEstimate) : ''].filter(Boolean).join(' · ') }),
  );
  if (e.comment) inspector.append(el('p', { text: e.comment }));
  inspector.append(
    section(
      `Columns (${e.attributes.length})`,
      el(
        'table',
        { class: 'cols' },
        ...e.attributes.map((a) => {
          const b = badge(a);
          const extra = [
            a.nullable ? '' : 'not null',
            a.defaultValue !== undefined ? `default ${a.defaultValue}` : '',
            a.presence !== undefined && a.presence < 1 ? `in ${Math.round(a.presence * 100)}%` : '',
          ].filter(Boolean).join(' · ');
          return el(
            'tr',
            { title: a.comment },
            el('td', { class: `kbadge ${b?.toLowerCase() ?? ''}`, text: b ?? '' }),
            el('td', {}, el('div', { class: `cname${a.nullable ? '' : ' nn'}`, text: a.name }), extra ? el('div', { class: 'muted small', text: extra }) : null),
            el('td', { class: 'ctype', text: a.type }),
          );
        }),
      ),
    ),
  );
  if (e.indexes.length) {
    inspector.append(
      section(
        `Indexes (${e.indexes.length})`,
        el('ul', { class: 'plain' }, ...e.indexes.map((i) =>
          el('li', {}, el('div', { class: 'strong', text: i.name }), el('div', { class: 'muted small', text: (i.primary ? 'primary · ' : i.unique ? 'unique · ' : '') + i.columns.join(', ') })),
        )),
      ),
    );
  }
  const rels = relsOf.get(e.id) ?? [];
  if (rels.length) {
    inspector.append(
      section(
        `Relationships (${rels.length})`,
        el('ul', { class: 'plain' }, ...rels.map((r) => {
          const out = r.fromEntity === e.id;
          const other = out ? r.toEntity : r.fromEntity;
          return el(
            'li',
            {},
            link(`${out ? '→' : '←'} ${other}`, () => goTo(other), out ? 'References this table' : 'Referenced by this table'),
            el('div', { class: 'muted small', text: `${r.fromColumns.join(', ')} → ${r.toColumns.join(', ')} · ${relSummary(r)}` }),
          );
        })),
      ),
    );
  }
}

function relSummary(r: Relationship): string {
  return [
    r.cardinality === 'oneToOne' ? 'one-to-one' : 'many-to-one',
    r.optional ? 'optional' : 'required',
    r.onDelete ? `on delete ${r.onDelete}` : '',
    r.onUpdate ? `on update ${r.onUpdate}` : '',
    r.inferred ? 'inferred' : '',
  ].filter(Boolean).join(' · ');
}

function inspectRelationship(r: Relationship) {
  inspector.append(
    el('h2', { text: r.name }),
    el('p', { class: 'muted', text: relSummary(r) }),
    section('Child (references)', link(r.fromEntity, () => goTo(r.fromEntity)), el('div', { class: 'mono small', text: r.fromColumns.join(', ') })),
    section('Parent (referenced)', link(r.toEntity, () => goTo(r.toEntity)), el('div', { class: 'mono small', text: r.toColumns.join(', ') })),
  );
  if (r.inferred) inspector.append(el('p', { class: 'muted', text: 'Inferred from sampled documents: no constraint enforces it.' }));
}

// MARK: - Export (whole diagram, light colours, no selection, never greeked)

const EXPORT_STYLE = `
.box{fill:#fff}.outline{fill:none;stroke:#c8c8c8;stroke-width:1}.head.accent{fill:#0a84ff}
.title{font:600 13px FONT_UI;fill:#fff}.count{font:9.5px FONT_UI;fill:rgba(255,255,255,.85)}
.stripe{fill:rgba(0,0,0,.035)}.col{font:11.5px FONT_MONO;fill:#1f1f1f}.col.nn{font-weight:bold}.col.partial{font-style:italic}
.type{font:10.5px FONT_UI;fill:#6e6e6e}.more{font:10px FONT_UI;fill:#6e6e6e}
.kbadge{font:bold 8.5px FONT_UI}.kbadge.pk{fill:#e8890c}.kbadge.fk{fill:#0a6fdb}.kbadge.uq{fill:#8e44ad}
.rel .line,.rel .mark{fill:none;stroke:#7a7a7a;stroke-width:1.3}.rel .hit{display:none}.rel.inferred .line{stroke-dasharray:5 4}
.rel circle.mark{fill:#fff}`;

function buildExport(): { svg: string; width: number; height: number } | undefined {
  if (!model) return undefined;
  const b = boundsOf(model.entities.map((e) => rectOf(e.id)).filter((r): r is Rect => !!r));
  if (!b) return undefined;
  const pad = 24;
  const width = Math.ceil(b.width + pad * 2);
  const height = Math.ceil(b.height + pad * 2);
  const root = document.createElementNS(SVG_NS, 'svg');
  root.setAttribute('xmlns', SVG_NS);
  root.setAttribute('width', String(width));
  root.setAttribute('height', String(height));
  root.setAttribute('viewBox', `0 0 ${width} ${height}`);
  const style = document.createElementNS(SVG_NS, 'style');
  style.textContent = EXPORT_STYLE.replaceAll('FONT_UI', cssVar('--vscode-font-family', 'sans-serif')).replaceAll(
    'FONT_MONO',
    cssVar('--vscode-editor-font-family', 'monospace'),
  );
  root.append(style, svg('rect', { width, height, fill: '#ffffff' }));
  const g = svg('g', { transform: `translate(${pad - b.x} ${pad - b.y})` });
  for (const rel of model.relationships) {
    const child = byId.get(rel.fromEntity), parent = byId.get(rel.toEntity);
    const cr = rectOf(rel.fromEntity), pr = rectOf(rel.toEntity);
    if (child && parent && cr && pr) g.append(relElement(rel, curve(rel, child, parent, cr, pr)));
  }
  for (const e of model.entities) {
    const r = rectOf(e.id);
    if (!r) continue;
    const box = entityElement(e, r, 'full');
    box.setAttribute('transform', `translate(${r.x} ${r.y})`);
    g.append(box);
  }
  root.append(g);
  return { svg: new XMLSerializer().serializeToString(root), width, height };
}

async function exportAs(format: 'svg' | 'png') {
  const x = buildExport();
  if (!x) return;
  if (format === 'svg') {
    vscode.post({ type: 'export', format, data: `<?xml version="1.0" encoding="UTF-8"?>\n${x.svg}` });
    return;
  }
  showBanner('Rendering PNG…');
  try {
    const scale = exportScale(x);
    const url = URL.createObjectURL(new Blob([x.svg], { type: 'image/svg+xml' }));
    const img = new Image();
    await new Promise<void>((resolve, reject) => {
      img.onload = () => resolve();
      img.onerror = () => reject(new Error('The diagram could not be rendered.'));
      img.src = url;
    });
    const c = document.createElement('canvas');
    c.width = Math.round(x.width * scale);
    c.height = Math.round(x.height * scale);
    const ctx = c.getContext('2d')!;
    ctx.scale(scale, scale);
    ctx.drawImage(img, 0, 0);
    URL.revokeObjectURL(url);
    const data = c.toDataURL('image/png').split(',')[1] ?? '';
    banner.hidden = true;
    vscode.post({ type: 'export', format, data });
  } catch (e) {
    showBanner(e instanceof Error ? e.message : String(e), 'OK', () => (banner.hidden = true), true);
  }
}

applyView();
renderInspector();
