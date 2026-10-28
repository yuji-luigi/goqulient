// Pure ER geometry for the diagram webview, ported from the app's `ERMetrics` / `ERPainter` (Sources/GokuClient/Views/
// ERDiagram.swift): box sizes, column row positions, Bézier relationship curves, crow's-foot marks, culling, pan/zoom.
// No DOM, so the Node tests run them directly.
import type { Attribute, Entity, Point, Relationship, Size } from '../../src/views/protocol.ts';

export const Metrics = {
  header: 30,
  row: 18,
  padding: 10,
  bottom: 6,
  minWidth: 150,
  maxWidth: 420,
  maxAttributes: 60,
  /** Below this zoom column text is unreadable, so rows are drawn as bars (much cheaper than text). */
  greekBelow: 0.4,
  minZoom: 0.1,
  maxZoom: 3,
} as const;

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type Measure = (text: string, font: 'title' | 'row' | 'type') => number;

export function visibleAttributes(e: Entity): Attribute[] {
  return e.attributes.length > Metrics.maxAttributes ? e.attributes.slice(0, Metrics.maxAttributes) : e.attributes;
}

/** Same formula as `ERTabModel.recomputeSizes`, with the webview's fonts. */
export function entitySize(e: Entity, measure: Measure): Size {
  const attrs = visibleAttributes(e);
  let w = measure(e.name, 'title') + Metrics.padding * 2 + 30;
  for (const a of attrs) w = Math.max(w, measure(a.name, 'row') + measure(a.type, 'type') + Metrics.padding * 2 + 44);
  const extra = e.attributes.length > attrs.length ? Metrics.row : 0;
  return {
    width: Math.ceil(Math.min(Math.max(w, Metrics.minWidth), Metrics.maxWidth)),
    height: Metrics.header + attrs.length * Metrics.row + Metrics.bottom + extra,
  };
}

/** Vertical middle of `column`'s row, or of the header when it isn't shown. */
export function rowY(e: Entity, r: Rect, column: string | undefined): number {
  const i = column === undefined ? -1 : visibleAttributes(e).findIndex((a) => a.name === column);
  return i >= 0 ? r.y + Metrics.header + (i + 0.5) * Metrics.row : r.y + Metrics.header / 2;
}

export interface Curve {
  start: Point;
  end: Point;
  c1: Point;
  c2: Point;
  /** Direction the line leaves the child (+1 right, -1 left) and enters the parent. */
  d1: number;
  d2: number;
}

/** Child → parent Bézier anchored at the first FK column's row (same rules as `ERPainter.curve`). */
export function curve(rel: Relationship, child: Entity, parent: Entity, cr: Rect, pr: Rect): Curve {
  const y1 = rowY(child, cr, rel.fromColumns[0]);
  const y2 = rowY(parent, pr, rel.toColumns[0]);
  if (child.id === parent.id) {
    const start = { x: cr.x + cr.width, y: y1 };
    const end = { x: cr.x + cr.width, y: y2 === y1 ? y1 + 8 : y2 };
    return { start, end, c1: { x: start.x + 50, y: start.y }, c2: { x: end.x + 50, y: end.y }, d1: 1, d2: 1 };
  }
  let start: Point, end: Point, d1: number, d2: number;
  if (pr.x >= cr.x + cr.width - 20) {
    [start, end, d1, d2] = [{ x: cr.x + cr.width, y: y1 }, { x: pr.x, y: y2 }, 1, -1];
  } else if (pr.x + pr.width <= cr.x + 20) {
    [start, end, d1, d2] = [{ x: cr.x, y: y1 }, { x: pr.x + pr.width, y: y2 }, -1, 1];
  } else {
    [start, end, d1, d2] = [{ x: cr.x + cr.width, y: y1 }, { x: pr.x + pr.width, y: y2 }, 1, 1];
  }
  const k = Math.max(40, Math.abs(end.x - start.x) / 2);
  return { start, end, c1: { x: start.x + d1 * k, y: start.y }, c2: { x: end.x + d2 * k, y: end.y }, d1, d2 };
}

const n = (v: number) => Math.round(v * 100) / 100;

export function curvePath(c: Curve): string {
  return `M${n(c.start.x)},${n(c.start.y)} C${n(c.c1.x)},${n(c.c1.y)} ${n(c.c2.x)},${n(c.c2.y)} ${n(c.end.x)},${n(c.end.y)}`;
}

export interface Marks {
  /** Path data for the straight strokes (foot, bars). */
  lines: string;
  circles: Point[];
}

/**
 * Crow's foot ends, as in the app: the child end shows an "o" plus a foot (many) or a bar (one); the parent end shows
 * "||", or "o|" when the FK is nullable.
 */
export function marks(rel: Pick<Relationship, 'cardinality' | 'optional'>, c: Curve): Marks {
  const { start: s, end: e, d1, d2 } = c;
  const seg = (a: Point, b: Point) => `M${n(a.x)},${n(a.y)} L${n(b.x)},${n(b.y)}`;
  const parts: string[] = [];
  if (rel.cardinality === 'manyToOne') {
    const tip = { x: s.x + d1 * 12, y: s.y };
    parts.push(seg(tip, { x: s.x, y: s.y - 6 }), seg(tip, { x: s.x, y: s.y + 6 }), seg(tip, s));
  } else {
    parts.push(seg({ x: s.x + d1 * 9, y: s.y - 6 }, { x: s.x + d1 * 9, y: s.y + 6 }));
  }
  const circles = [{ x: s.x + d1 * 18, y: s.y }];
  if (rel.optional) circles.push({ x: e.x + d2 * 16, y: e.y });
  else parts.push(seg({ x: e.x + d2 * 13, y: e.y - 6 }, { x: e.x + d2 * 13, y: e.y + 6 }));
  parts.push(seg({ x: e.x + d2 * 8, y: e.y - 6 }, { x: e.x + d2 * 8, y: e.y + 6 }));
  return { lines: parts.join(' '), circles };
}

/** Contains the Bézier (it lies inside its control points' hull) and the end marks. */
export function hull(c: Curve): Rect {
  const xs = [c.start.x, c.end.x, c.c1.x, c.c2.x];
  const ys = [c.start.y, c.end.y, c.c1.y, c.c2.y];
  const x0 = Math.min(...xs) - 24;
  const y0 = Math.min(...ys) - 10;
  return { x: x0, y: y0, width: Math.max(...xs) + 24 - x0, height: Math.max(...ys) + 10 - y0 };
}

export function intersects(a: Rect, b: Rect): boolean {
  return a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
}

export function inset(r: Rect, d: number): Rect {
  return { x: r.x + d, y: r.y + d, width: r.width - 2 * d, height: r.height - 2 * d };
}

export function boundsOf(rects: Iterable<Rect>): Rect | undefined {
  let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
  for (const r of rects) {
    x0 = Math.min(x0, r.x);
    y0 = Math.min(y0, r.y);
    x1 = Math.max(x1, r.x + r.width);
    y1 = Math.max(y1, r.y + r.height);
  }
  return x0 === Infinity ? undefined : { x: x0, y: y0, width: x1 - x0, height: y1 - y0 };
}

/** Screen = world × zoom + (x, y). */
export interface View {
  x: number;
  y: number;
  zoom: number;
}

export function clampZoom(z: number): number {
  return Math.min(Metrics.maxZoom, Math.max(Metrics.minZoom, z));
}

/** Zooms by `factor` keeping the world point under `at` (screen coordinates) in place. */
export function zoomAt(v: View, factor: number, at: Point): View {
  const zoom = clampZoom(v.zoom * factor);
  const k = zoom / v.zoom;
  return { zoom, x: at.x - (at.x - v.x) * k, y: at.y - (at.y - v.y) * k };
}

/** Whole diagram in the viewport with a margin, at most 150% (like the app's Fit). */
export function fit(bounds: Rect, viewport: Size, margin = 24): View {
  const w = Math.max(1, viewport.width - margin * 2);
  const h = Math.max(1, viewport.height - margin * 2);
  const zoom = clampZoom(Math.min(1.5, w / Math.max(1, bounds.width), h / Math.max(1, bounds.height)));
  return {
    zoom,
    x: (viewport.width - bounds.width * zoom) / 2 - bounds.x * zoom,
    y: (viewport.height - bounds.height * zoom) / 2 - bounds.y * zoom,
  };
}

/** Centres world point `p` in the viewport at the current zoom. */
export function centreOn(v: View, p: Point, viewport: Size): View {
  return { zoom: v.zoom, x: viewport.width / 2 - p.x * v.zoom, y: viewport.height / 2 - p.y * v.zoom };
}

/** The world rectangle the viewport shows. */
export function worldViewport(v: View, viewport: Size): Rect {
  return { x: -v.x / v.zoom, y: -v.y / v.zoom, width: viewport.width / v.zoom, height: viewport.height / v.zoom };
}

/** `id` and everything one relationship away from it. */
export function neighbours(rels: readonly Relationship[], id: string): Set<string> {
  const s = new Set([id]);
  for (const r of rels) {
    if (r.fromEntity === id) s.add(r.toEntity);
    if (r.toEntity === id) s.add(r.fromEntity);
  }
  return s;
}

/** Entities whose name (or qualified id) contains `query`, case-insensitively, in model order. */
export function search(entities: readonly Entity[], query: string): string[] {
  const q = query.trim().toLowerCase();
  if (!q) return [];
  return entities.filter((e) => e.name.toLowerCase().includes(q) || e.id.toLowerCase().includes(q)).map((e) => e.id);
}

/** Header hue from the schema name: the app's djb2-xor hash (`ERPainter.headerColor`), UInt32 arithmetic. */
export function schemaHue(schema: string): number {
  let h = 5381;
  for (const b of new TextEncoder().encode(schema)) h = (Math.imul(h, 33) ^ b) >>> 0;
  return h % 360;
}

/** The app's `NSColor(hue:, saturation: 0.45, brightness: 0.7)` as CSS; `undefined` for no schema (accent colour). */
export function headerColor(schema: string): string | undefined {
  if (!schema) return undefined;
  const v = 0.7, s = 0.45;
  const l = v * (1 - s / 2);
  const sl = (v - l) / Math.min(l, 1 - l);
  return `hsl(${schemaHue(schema)} ${n(sl * 100)}% ${n(l * 100)}%)`;
}

/** Cuts `text` with "…" so it fits `max` px. */
export function ellipsize(text: string, max: number, measure: (t: string) => number): string {
  if (measure(text) <= max) return text;
  let lo = 0, hi = text.length;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (measure(text.slice(0, mid) + '…') <= max) lo = mid;
    else hi = mid - 1;
  }
  return text.slice(0, lo) + '…';
}

export function compactRows(n: number): string {
  if (n < 1000) return `~${n} rows`;
  if (n < 1_000_000) return `~${(n / 1000).toFixed(1)}k rows`;
  return `~${(n / 1_000_000).toFixed(1)}M rows`;
}

/** Badge for a column, in the app's precedence. */
export function badge(a: Attribute): 'PK' | 'FK' | 'UQ' | undefined {
  return a.isPrimaryKey ? 'PK' : a.isForeignKey ? 'FK' : a.isUnique ? 'UQ' : undefined;
}

/** Export scale: up to 2×, but no side over 16,000 px (as the app). */
export function exportScale(bounds: Size): number {
  return Math.min(2, 16_000 / Math.max(bounds.width, bounds.height, 1));
}
