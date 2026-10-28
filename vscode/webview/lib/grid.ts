// Pure grid helpers for the table webview: virtualisation math, clipped/NULL cell display, selection and TSV copy.
// No DOM, so the Node tests run them directly.
import type { Sort } from '../../src/views/protocol.ts';

/** What the engine appends to a cell it cut (`CLIP_MARKER` in engine/crates/goku-engine/src/limits.rs). */
export const CLIP_MARKER = '… [truncated]';

export function isClipped(v: string | null | undefined): boolean {
  return typeof v === 'string' && v.endsWith(CLIP_MARKER);
}

/** One line of text for a cell (the grid shows a single line, like the app); the clip marker becomes a badge. */
export function cellText(v: string | null): string {
  if (v === null) return 'NULL';
  const s = isClipped(v) ? v.slice(0, -CLIP_MARKER.length) : v;
  return s.length > 500 ? s.slice(0, 500) : s.replace(/\r\n|\r|\n/g, ' ↵ ').replace(/\t/g, ' ');
}

export interface Range {
  /** First index, inclusive. */
  start: number;
  /** Last index, exclusive. */
  end: number;
}

/** Rows (fixed `size`) intersecting `[scroll, scroll + viewport)`, widened by `overscan` on both sides. */
export function visibleRange(scroll: number, viewport: number, size: number, count: number, overscan = 0): Range {
  if (count <= 0 || size <= 0) return { start: 0, end: 0 };
  const first = Math.floor(Math.max(0, scroll) / size);
  const last = Math.ceil((Math.max(0, scroll) + Math.max(0, viewport)) / size);
  return { start: Math.max(0, Math.min(count, first - overscan)), end: Math.min(count, Math.max(first, last) + overscan) };
}

/** `offsets[i]` = left edge of column `i`; `offsets[n]` = total width. */
export function prefixOffsets(widths: readonly number[]): number[] {
  const out = [0];
  for (const w of widths) out.push(out[out.length - 1]! + w);
  return out;
}

/** Index of the column containing `x` (clamped), by binary search over `prefixOffsets`. */
export function columnAt(offsets: readonly number[], x: number): number {
  const n = offsets.length - 1;
  if (n <= 0) return -1;
  let lo = 0;
  let hi = n - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (offsets[mid]! <= x) lo = mid;
    else hi = mid - 1;
  }
  return lo;
}

/** Columns (variable widths) intersecting `[scroll, scroll + viewport)`, widened by `overscan`. */
export function columnRange(offsets: readonly number[], scroll: number, viewport: number, overscan = 0): Range {
  const n = offsets.length - 1;
  if (n <= 0) return { start: 0, end: 0 };
  const first = columnAt(offsets, Math.max(0, scroll));
  const last = columnAt(offsets, Math.max(0, scroll + viewport - 1));
  return { start: Math.max(0, first - overscan), end: Math.min(n, last + 1 + overscan) };
}

/** Width for a column from its header and a sample of cells, measured with `measure` (px), clamped to `[min, max]`. */
export function columnWidth(
  header: string,
  samples: readonly (string | null)[],
  measure: (text: string) => number,
  min = 64,
  max = 360,
  padding = 24,
): number {
  let w = measure(header) + padding + 14; // room for the sort arrow
  for (const s of samples) w = Math.max(w, measure(cellText(s)) + padding);
  return Math.round(Math.min(max, Math.max(min, w)));
}

/** Evenly spread sample of at most `n` row indexes out of `count`, so wide values deep in the page count too. */
export function sampleIndexes(count: number, n: number): number[] {
  if (count <= n) return Array.from({ length: count }, (_, i) => i);
  const step = count / n;
  return Array.from({ length: n }, (_, i) => Math.floor(i * step));
}

export interface Cell {
  row: number;
  col: number;
}

export interface Selection {
  anchor: Cell;
  focus: Cell;
}

export interface Rect {
  r0: number;
  r1: number;
  c0: number;
  c1: number;
}

/** Inclusive bounds of a selection. */
export function selectionRect(s: Selection): Rect {
  return {
    r0: Math.min(s.anchor.row, s.focus.row),
    r1: Math.max(s.anchor.row, s.focus.row),
    c0: Math.min(s.anchor.col, s.focus.col),
    c1: Math.max(s.anchor.col, s.focus.col),
  };
}

export function inRect(r: Rect, row: number, col: number): boolean {
  return row >= r.r0 && row <= r.r1 && col >= r.c0 && col <= r.c1;
}

/** Moves a cell by `(dr, dc)` inside a `rows × cols` grid. */
export function moveCell(c: Cell, dr: number, dc: number, rows: number, cols: number): Cell {
  return { row: Math.max(0, Math.min(rows - 1, c.row + dr)), col: Math.max(0, Math.min(cols - 1, c.col + dc)) };
}

/** One TSV field, RFC 4180 style (as the app's `RowCopy`): NULL is empty; tab, newline or quote → quoted. */
export function tsvField(v: string | null): string {
  if (v === null) return '';
  return /[\t\n\r"]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v;
}

export function toTsv(rows: readonly (readonly (string | null)[])[], header?: readonly string[]): string {
  const lines = rows.map((r) => r.map(tsvField).join('\t'));
  if (header) lines.unshift(header.map(tsvField).join('\t'));
  return lines.join('\n');
}

/** The cells of `rect` as rows, for copying. */
export function sliceRect(rows: readonly (readonly (string | null)[])[], rect: Rect): (string | null)[][] {
  const out: (string | null)[][] = [];
  for (let r = rect.r0; r <= rect.r1 && r < rows.length; r++) out.push(rows[r]!.slice(rect.c0, rect.c1 + 1));
  return out;
}

/** Header click: ascending → descending → unsorted; another column starts ascending. */
export function nextSort(current: Sort | undefined, column: string): Sort | undefined {
  if (!current || current.column !== column) return { column, descending: false };
  return current.descending ? undefined : { column, descending: true };
}

/** "Rows 1–1,000", "No rows", with "+" when more rows exist. */
export function pageLabel(offset: number, count: number, truncated: boolean): string {
  if (count === 0) return offset > 0 ? 'No more rows' : 'No rows';
  const f = (n: number) => n.toLocaleString('en-US');
  return `Rows ${f(offset + 1)}–${f(offset + count)}${truncated ? ' · more rows' : ''}`;
}

/** Offset of the previous / next page; `undefined` when there is none. */
export function pageOffsets(offset: number, count: number, pageSize: number, truncated: boolean): { prev?: number; next?: number } {
  return {
    prev: offset > 0 ? Math.max(0, offset - pageSize) : undefined,
    next: truncated && count > 0 ? offset + count : undefined,
  };
}
