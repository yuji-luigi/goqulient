// VSX-12 webview: virtualised read-only grid (rows and columns), sticky header + row numbers, sort, paging, copy as
// TSV, and a documents view for MongoDB. The extension owns the data; this only renders the page it was sent.
import './table.css';
import type { QueryResult, Sort, TableFromWebview, TableToWebview } from '../src/views/protocol.ts';
import { button, contextMenu, cssVar, el, host, measurer } from './host.ts';
import {
  cellText,
  columnAt,
  columnRange,
  columnWidth,
  inRect,
  isClipped,
  moveCell,
  nextSort,
  pageLabel,
  pageOffsets,
  prefixOffsets,
  sampleIndexes,
  selectionRect,
  sliceRect,
  toTsv,
  visibleRange,
  type Cell,
  type Selection,
} from './lib/grid.ts';

interface UiState {
  view: 'grid' | 'documents';
  scroll: { requestId: number; top: number; left: number };
}

const vscode = host<TableToWebview, TableFromWebview, UiState>();
const ROW_H = 22;
const OVERSCAN = 6;
const NUMERIC = /int|serial|numeric|decimal|float|double|real|money|number|long/i;

let pageSize = 1000;
let readOnly = true;
let result: QueryResult | undefined;
let offset = 0;
let sort: Sort | undefined;
let shownRequest = 0;
let loading: { requestId: number; offset: number } | undefined;
let error: string | undefined;
let view: UiState['view'] = vscode.state().view ?? 'grid';
let widths: number[] = [];
let offsets: number[] = [0];
let numeric: boolean[] = [];
let selection: Selection | undefined;

// MARK: - DOM skeleton

const titleEl = el('span', { class: 'title' });
const roBadge = el('span', { class: 'badge', text: 'read-only', title: 'This connection is read-only' });
const gridBtn = button('Grid', 'Show the rows as a grid', () => setView('grid'), 'seg');
const docsBtn = button('Documents', 'Show the documents as text', () => setView('documents'), 'seg');
const viewToggle = el('span', { class: 'segmented', role: 'group' }, gridBtn, docsBtn);
const status = el('span', { class: 'status' });
const prevBtn = button('‹ Prev', 'Previous page', () => page(-1));
const nextBtn = button('Next ›', 'Next page', () => page(1));
const reloadBtn = button('Reload', 'Load this page again', () => load(offset, sort));
const toolbar = el('div', { class: 'toolbar' }, titleEl, roBadge, viewToggle, el('span', { class: 'spacer' }), status, prevBtn, nextBtn, reloadBtn);
const progress = el('div', { class: 'progress', hidden: '' });
const bannerText = el('span');
const bannerBtn = el('button', { class: 'btn', type: 'button' });
const banner = el('div', { class: 'banner', hidden: '' }, bannerText, bannerBtn);

const corner = el('div', { class: 'corner', text: '#', title: 'Select all' });
const headerInner = el('div', { class: 'header-inner' });
const header = el('div', { class: 'header' }, headerInner);
const gutterInner = el('div', { class: 'gutter-inner' });
const gutter = el('div', { class: 'gutter' }, gutterInner);
const spacer = el('div', { class: 'spacer-box' });
const cellsLayer = el('div', { class: 'cells' });
const body = el('div', { class: 'body', tabindex: '0', role: 'grid' }, spacer, cellsLayer);
const grid = el('div', { class: 'grid' }, corner, header, gutter, body);
const docs = el('div', { class: 'docs', hidden: '' });
const empty = el('div', { class: 'empty', hidden: '' });
const footer = el('div', { class: 'footer' });
document.getElementById('app')!.append(toolbar, progress, banner, grid, docs, empty, footer);

// MARK: - Messages

vscode.listen((m) => {
  switch (m.type) {
    case 'init':
      pageSize = m.pageSize;
      readOnly = m.readOnly;
      titleEl.textContent = m.title;
      titleEl.title = [m.object.database, m.object.schema, m.object.name].filter(Boolean).join('.');
      break;
    case 'loading':
      loading = { requestId: m.requestId, offset: m.offset };
      error = undefined;
      break;
    case 'page':
      loading = undefined;
      error = undefined;
      result = m.result;
      offset = m.offset;
      sort = m.sort;
      shownRequest = m.requestId;
      setData();
      break;
    case 'error':
      loading = undefined;
      error = m.message;
      break;
    case 'cancelled':
      loading = undefined;
      break;
  }
  chrome();
});
vscode.post({ type: 'ready' });

function load(at: number, s: Sort | undefined) {
  vscode.post({ type: 'load', offset: Math.max(0, at), sort: s });
}

function page(direction: -1 | 1) {
  if (!result) return;
  const { prev, next } = pageOffsets(offset, result.rows.length, pageSize, result.truncated);
  const to = direction < 0 ? prev : next;
  if (to !== undefined) load(to, sort);
}

function setView(v: UiState['view']) {
  view = v;
  vscode.save({ view });
  chrome();
  if (v === 'grid') render();
}

/** Everything around the grid: toolbar state, banner, progress, empty/documents panes. */
function chrome() {
  roBadge.hidden = !readOnly;
  const hasDocs = !!result?.documents;
  viewToggle.hidden = !hasDocs;
  const showDocs = hasDocs && view === 'documents';
  gridBtn.classList.toggle('on', !showDocs);
  docsBtn.classList.toggle('on', showDocs);
  const n = result?.rows.length ?? 0;
  const { prev, next } = result ? pageOffsets(offset, n, pageSize, result.truncated) : {};
  prevBtn.disabled = !!loading || prev === undefined;
  nextBtn.disabled = !!loading || next === undefined;
  reloadBtn.disabled = !!loading;
  status.textContent = result ? `${pageLabel(offset, n, result.truncated)} · ${result.elapsedMs} ms` : '';
  status.classList.toggle('more', !!result?.truncated);
  status.title = result?.truncated ? 'There are more rows: use Next' : '';
  progress.hidden = !loading;
  grid.classList.toggle('stale', !!loading);
  banner.hidden = !loading && !error;
  banner.classList.toggle('error', !!error);
  if (loading) {
    bannerText.textContent = `Loading rows from ${(loading.offset + 1).toLocaleString('en-US')}…`;
    bannerBtn.textContent = 'Cancel';
    bannerBtn.onclick = () => vscode.post({ type: 'cancel' });
  } else if (error) {
    bannerText.textContent = error;
    bannerBtn.textContent = 'Retry';
    bannerBtn.onclick = () => load(offset, sort);
  }
  grid.hidden = !result || result.columns.length === 0 || showDocs;
  grid.classList.toggle('no-rows', !!result && result.rows.length === 0);
  docs.hidden = !showDocs;
  empty.hidden = !result || result.rows.length > 0;
  empty.textContent = offset > 0 ? 'No more rows.' : 'No rows.';
  footerText();
}

// MARK: - Data

const measure = measurer();

function setData() {
  const r = result!;
  const font = `${cssVar('--vscode-editor-font-size', '12px')} ${cssVar('--vscode-editor-font-family', 'monospace')}`;
  const headerFont = `600 ${cssVar('--vscode-font-size', '13px')} ${cssVar('--vscode-font-family', 'sans-serif')}`;
  const samples = sampleIndexes(r.rows.length, 200);
  widths = r.columns.map((c, i) =>
    Math.max(
      columnWidth(c.name, samples.map((s) => r.rows[s]![i] ?? null), (t) => measure(t, font)),
      Math.min(360, measure(c.name, headerFont) + 38),
    ),
  );
  numeric = r.columns.map((c) => NUMERIC.test(c.typeName));
  offsets = prefixOffsets(widths);
  selection = undefined;
  buildHeader();
  const digits = String(offset + r.rows.length).length;
  grid.style.setProperty('--gutter', `${Math.max(36, digits * 8 + 18)}px`);
  spacer.style.width = `${offsets[offsets.length - 1]}px`;
  spacer.style.height = `${r.rows.length * ROW_H}px`;
  gutterInner.style.height = `${r.rows.length * ROW_H}px`;
  cellsLayer.replaceChildren();
  gutterInner.replaceChildren();
  rendered.clear();
  renderedRows.clear();
  // A webview recreated after being hidden gets the same page again: put the scroll position back.
  const saved = vscode.state().scroll;
  if (saved && saved.requestId === shownRequest) [body.scrollTop, body.scrollLeft] = [saved.top, saved.left];
  else [body.scrollTop, body.scrollLeft] = [0, 0];
  buildDocuments();
  chrome();
  render();
}

function buildHeader() {
  headerInner.replaceChildren();
  const r = result!;
  r.columns.forEach((c, i) => {
    const arrow = sort?.column === c.name ? (sort.descending ? ' ▼' : ' ▲') : '';
    const cell = el(
      'div',
      { class: `hcell${numeric[i] ? ' num' : ''}${arrow ? ' sorted' : ''}`, title: `${c.name}\n${c.typeName}\nClick to sort` },
      el('span', { class: 'hname', text: c.name }),
      el('span', { class: 'harrow', text: arrow }),
      el('span', { class: 'htype', text: c.typeName }),
    );
    cell.style.left = `${offsets[i]}px`;
    cell.style.width = `${widths[i]}px`;
    cell.addEventListener('click', (e) => {
      if ((e.target as HTMLElement).classList.contains('resize')) return;
      load(0, nextSort(sort, c.name)); // a new order starts at the first page
    });
    const handle = el('div', { class: 'resize', title: 'Drag to resize' });
    handle.addEventListener('pointerdown', (e) => resizeColumn(e, i, handle));
    cell.append(handle);
    headerInner.append(cell);
  });
  headerInner.style.width = `${offsets[offsets.length - 1]! + 40}px`;
}

function resizeColumn(e: PointerEvent, i: number, handle: HTMLElement) {
  e.preventDefault();
  e.stopPropagation();
  handle.setPointerCapture(e.pointerId);
  const x0 = e.clientX;
  const w0 = widths[i]!;
  const move = (ev: PointerEvent) => {
    widths[i] = Math.max(40, Math.round(w0 + ev.clientX - x0));
    offsets = prefixOffsets(widths);
    buildHeader();
    spacer.style.width = `${offsets[offsets.length - 1]}px`;
    cellsLayer.replaceChildren();
    rendered.clear();
    render();
  };
  const up = () => {
    handle.removeEventListener('pointermove', move);
    handle.removeEventListener('pointerup', up);
  };
  handle.addEventListener('pointermove', move);
  handle.addEventListener('pointerup', up);
}

function buildDocuments() {
  docs.replaceChildren();
  const d = result?.documents;
  if (!d) return;
  d.forEach((text, i) => {
    const clipped = isClipped(text);
    docs.append(
      el(
        'div',
        { class: 'doc' },
        el('div', { class: 'doc-n', text: String(offset + i + 1) }),
        el('pre', { class: 'doc-text', text: clipped ? cellText(text) : text }),
        clipped ? el('div', { class: 'clip-note', text: 'Truncated: the document is longer than the cell cap.' }) : null,
      ),
    );
  });
}

// MARK: - Virtualised rendering

/** Rendered cells by "row:col" and row numbers by row, so scrolling only adds and removes what changed. */
const rendered = new Map<string, HTMLDivElement>();
const renderedRows = new Map<number, HTMLDivElement>();
let frame = 0;

function schedule() {
  if (!frame) frame = requestAnimationFrame(() => ((frame = 0), render()));
}

function render() {
  if (!result || grid.hidden) return;
  const rows = visibleRange(body.scrollTop, body.clientHeight, ROW_H, result.rows.length, OVERSCAN);
  const cols = columnRange(offsets, body.scrollLeft, body.clientWidth, 2);
  headerInner.style.transform = `translateX(${-body.scrollLeft}px)`;
  gutterInner.style.transform = `translateY(${-body.scrollTop}px)`;
  const keep = new Set<string>();
  const rect = selection && selectionRect(selection);
  for (let r = rows.start; r < rows.end; r++) {
    const values = result.rows[r]!;
    for (let c = cols.start; c < cols.end; c++) {
      const key = `${r}:${c}`;
      keep.add(key);
      let cell = rendered.get(key);
      if (!cell) {
        const v = values[c] ?? null;
        cell = el('div', { class: `cell${v === null ? ' null' : ''}${numeric[c] ? ' num' : ''}`, text: cellText(v) });
        if (isClipped(v)) {
          cell.classList.add('clipped');
          cell.title = 'Truncated: the value is longer than the cell cap';
        }
        cell.dataset.r = String(r);
        cell.dataset.c = String(c);
        cell.style.transform = `translate(${offsets[c]}px, ${r * ROW_H}px)`;
        cell.style.width = `${widths[c]}px`;
        cellsLayer.append(cell);
        rendered.set(key, cell);
      }
      cell.classList.toggle('sel', !!rect && inRect(rect, r, c));
      cell.classList.toggle('focus', !!selection && selection.focus.row === r && selection.focus.col === c);
    }
  }
  for (const [key, cell] of rendered) if (!keep.has(key)) (cell.remove(), rendered.delete(key));
  for (let r = rows.start; r < rows.end; r++) {
    let n = renderedRows.get(r);
    if (!n) {
      n = el('div', { class: 'rownum', text: String(offset + r + 1) });
      n.dataset.r = String(r);
      n.style.transform = `translateY(${r * ROW_H}px)`;
      gutterInner.append(n);
      renderedRows.set(r, n);
    }
    n.classList.toggle('sel', !!rect && r >= rect.r0 && r <= rect.r1);
  }
  for (const [r, n] of renderedRows) if (r < rows.start || r >= rows.end) (n.remove(), renderedRows.delete(r));
}

body.addEventListener('scroll', () => {
  schedule();
  vscode.save({ scroll: { requestId: shownRequest, top: body.scrollTop, left: body.scrollLeft } });
}, { passive: true });
new ResizeObserver(schedule).observe(body);

// MARK: - Selection, keyboard, copy

function cellFromEvent(e: MouseEvent): Cell | undefined {
  if (!result) return undefined;
  const b = body.getBoundingClientRect();
  const x = e.clientX - b.left + body.scrollLeft;
  const y = e.clientY - b.top + body.scrollTop;
  const row = Math.floor(y / ROW_H);
  if (row < 0 || row >= result.rows.length || x >= offsets[offsets.length - 1]!) return undefined;
  return { row, col: columnAt(offsets, x) };
}

function select(s: Selection | undefined) {
  selection = s;
  render();
  footerText();
}

function allColumns(row0: number, row1: number): Selection {
  const last = (result?.columns.length ?? 1) - 1;
  return { anchor: { row: row0, col: 0 }, focus: { row: row1, col: last } };
}

let dragging = false;
body.addEventListener('mousedown', (e) => {
  if (e.button !== 0) return;
  const c = cellFromEvent(e);
  if (!c) return;
  body.focus();
  dragging = true;
  select(e.shiftKey && selection ? { anchor: selection.anchor, focus: c } : { anchor: c, focus: c });
});
body.addEventListener('mousemove', (e) => {
  if (!dragging || !selection) return;
  const c = cellFromEvent(e);
  if (c && (c.row !== selection.focus.row || c.col !== selection.focus.col)) select({ anchor: selection.anchor, focus: c });
});
window.addEventListener('mouseup', () => (dragging = false));

gutter.addEventListener('mousedown', (e) => {
  const target = (e.target as HTMLElement).closest<HTMLElement>('.rownum');
  if (!target || !result) return;
  const r = Number(target.dataset.r);
  body.focus();
  select(e.shiftKey && selection ? allColumns(selection.anchor.row, r) : allColumns(r, r));
});
corner.addEventListener('click', () => result?.rows.length && select(allColumns(0, result.rows.length - 1)));

body.addEventListener('keydown', (e) => {
  if (!result || !result.rows.length) return;
  const mod = e.metaKey || e.ctrlKey;
  if (mod && e.key.toLowerCase() === 'c') {
    e.preventDefault();
    copy('selection');
    return;
  }
  if (mod && e.key.toLowerCase() === 'a') {
    e.preventDefault();
    select(allColumns(0, result.rows.length - 1));
    return;
  }
  const step = Math.max(1, Math.floor(body.clientHeight / ROW_H) - 1);
  const moves: Record<string, [number, number]> = {
    ArrowUp: [-1, 0], ArrowDown: [1, 0], ArrowLeft: [0, -1], ArrowRight: [0, 1],
    PageUp: [-step, 0], PageDown: [step, 0], Home: [0, -1e9], End: [0, 1e9],
  };
  const d = moves[e.key];
  if (!d) return;
  e.preventDefault();
  const from = selection?.focus ?? { row: 0, col: 0 };
  const to = selection ? moveCell(from, d[0], d[1], result.rows.length, result.columns.length) : from;
  select({ anchor: e.shiftKey && selection ? selection.anchor : to, focus: to });
  reveal(to);
});

function reveal(c: Cell) {
  const top = c.row * ROW_H;
  if (top < body.scrollTop) body.scrollTop = top;
  else if (top + ROW_H > body.scrollTop + body.clientHeight) body.scrollTop = top + ROW_H - body.clientHeight;
  const left = offsets[c.col]!;
  const right = offsets[c.col + 1]!;
  if (left < body.scrollLeft) body.scrollLeft = left;
  else if (right > body.scrollLeft + body.clientWidth) body.scrollLeft = right - body.clientWidth;
}

type CopyWhat = 'value' | 'rows' | 'selection' | 'selectionWithHeaders' | 'column';

function copy(what: CopyWhat) {
  if (!result || !selection) return;
  const rect = selectionRect(selection);
  const single = rect.r0 === rect.r1 && rect.c0 === rect.c1;
  let text: string;
  if (what === 'value' || (what === 'selection' && single)) {
    text = result.rows[selection.focus.row]![selection.focus.col] ?? ''; // raw value, NULL → empty, like the app
  } else if (what === 'rows') {
    text = toTsv(sliceRect(result.rows, { ...rect, c0: 0, c1: result.columns.length - 1 }));
  } else if (what === 'column') {
    text = result.columns.slice(rect.c0, rect.c1 + 1).map((c) => c.name).join('\t');
  } else {
    const header = what === 'selectionWithHeaders' ? result.columns.slice(rect.c0, rect.c1 + 1).map((c) => c.name) : undefined;
    text = toTsv(sliceRect(result.rows, rect), header);
  }
  vscode.post({ type: 'copy', text });
}

body.addEventListener('contextmenu', (e) => {
  e.preventDefault();
  const c = cellFromEvent(e);
  if (!c || !result) return;
  if (!selection || !inRect(selectionRect(selection), c.row, c.col)) select({ anchor: c, focus: c });
  const rect = selectionRect(selection!);
  const nRows = rect.r1 - rect.r0 + 1;
  const clipped = sliceRect(result.rows, rect).some((r) => r.some(isClipped));
  contextMenu(e.clientX, e.clientY, [
    { label: 'Copy Value', run: () => copy('value') },
    { label: nRows > 1 ? `Copy ${nRows} Rows (TSV)` : 'Copy Row (TSV)', run: () => copy('rows') },
    { label: 'Copy Selection (TSV)', run: () => copy('selection') },
    { label: 'Copy Selection with Headers', run: () => copy('selectionWithHeaders') },
    '-',
    { label: rect.c0 === rect.c1 ? 'Copy Column Name' : 'Copy Column Names', run: () => copy('column') },
    ...(clipped ? ['-' as const, { label: 'Some values are truncated and copy as shown', run: () => {}, disabled: true }] : []),
  ]);
});

function footerText() {
  if (!result || !selection || grid.hidden) {
    footer.textContent = result ? `${result.columns.length} columns` : '';
    return;
  }
  const rect = selectionRect(selection);
  const { row, col } = selection.focus;
  const c = result.columns[col]!;
  const v = result.rows[row]![col] ?? null;
  const size = rect.r0 === rect.r1 && rect.c0 === rect.c1 ? '' : ` · ${rect.r1 - rect.r0 + 1} × ${rect.c1 - rect.c0 + 1} selected`;
  const info = v === null ? 'NULL' : isClipped(v) ? 'truncated' : `${v.length.toLocaleString('en-US')} chars`;
  footer.textContent = `Row ${(offset + row + 1).toLocaleString('en-US')} · ${c.name} (${c.typeName}) · ${info}${size}`;
}

chrome();
