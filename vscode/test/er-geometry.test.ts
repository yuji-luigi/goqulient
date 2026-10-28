import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import type { Attribute, Entity, Relationship, SchemaModel } from '../src/views/protocol.ts';
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
  rowY,
  schemaHue,
  search,
  worldViewport,
  zoomAt,
  type Measure,
} from '../webview/lib/er.ts';

const attr = (name: string, extra: Partial<Attribute> = {}): Attribute => ({
  name,
  type: 'integer',
  nullable: false,
  isPrimaryKey: false,
  isForeignKey: false,
  isUnique: false,
  ...extra,
});

const entity = (id: string, attributes: Attribute[]): Entity => ({ id, schema: '', name: id, kind: 'table', attributes, indexes: [] });

const rel = (from: string, fromCol: string, to: string, extra: Partial<Relationship> = {}): Relationship => ({
  id: `${from}->${to}`,
  name: `${from}_fk`,
  fromEntity: from,
  fromColumns: [fromCol],
  toEntity: to,
  toColumns: ['id'],
  cardinality: 'manyToOne',
  optional: false,
  inferred: false,
  ...extra,
});

const measure: Measure = (t, font) => t.length * (font === 'title' ? 8 : font === 'row' ? 7 : 6);

test('box size follows the app formula, clamped to 150...420', () => {
  const e = entity('orders', [attr('id'), attr('customer_id')]);
  // max(6×8 + 20 + 30, 11×7 + 7×6 + 20 + 44) = 183
  assert.deepEqual(entitySize(e, measure), { width: 183, height: 30 + 2 * 18 + 6 });
  assert.equal(entitySize(entity('t', []), measure).width, Metrics.minWidth);
  assert.equal(entitySize(entity('t', [attr('x'.repeat(200))]), measure).width, Metrics.maxWidth);
  const many = entity('wide', Array.from({ length: 75 }, (_, i) => attr(`c${i}`)));
  assert.equal(entitySize(many, measure).height, 30 + 60 * 18 + 6 + 18, 'at most 60 rows plus the "more" row');
});

test('curves anchor at the FK row and leave towards the parent', () => {
  const child = entity('child', [attr('id'), attr('parent_id')]);
  const parent = entity('parent', [attr('id')]);
  const r = rel('child', 'parent_id', 'parent');
  const cr = { x: 300, y: 0, width: 150, height: 72 };
  const pr = { x: 0, y: 100, width: 150, height: 54 };
  assert.equal(rowY(child, cr, 'parent_id'), 30 + 1.5 * 18);
  assert.equal(rowY(child, cr, 'missing'), 15, 'unknown column → header');
  const c = curve(r, child, parent, cr, pr);
  assert.deepEqual([c.start, c.end, c.d1, c.d2], [{ x: 300, y: 57 }, { x: 150, y: 139 }, -1, 1]);
  assert.deepEqual(c.c1, { x: 225, y: 57 });
  assert.equal(curvePath(c), 'M300,57 C225,57 225,139 150,139');
  const right = curve(r, child, parent, { ...cr, x: 0 }, { ...pr, x: 300 });
  assert.deepEqual([right.d1, right.d2], [1, -1]);
  const stacked = curve(r, child, parent, cr, { ...pr, x: 310 });
  assert.deepEqual([stacked.start.x, stacked.end.x, stacked.d1, stacked.d2], [450, 460, 1, 1], 'overlapping columns loop on the right');
  const self = curve(rel('child', 'id', 'child'), child, child, cr, cr);
  assert.equal(self.start.y + 8, self.end.y, 'self-reference on the same row is nudged');
});

test("crow's foot for many, bar for one, circle when optional", () => {
  const c = { start: { x: 100, y: 50 }, end: { x: 0, y: 10 }, c1: { x: 0, y: 0 }, c2: { x: 0, y: 0 }, d1: -1, d2: 1 };
  const many = marks({ cardinality: 'manyToOne', optional: false }, c);
  assert.ok(many.lines.startsWith('M88,50 L100,44'), many.lines);
  assert.ok(many.lines.includes('M13,4 L13,16'), 'parent "||" second bar');
  assert.deepEqual(many.circles, [{ x: 82, y: 50 }]);
  const one = marks({ cardinality: 'oneToOne', optional: true }, c);
  assert.ok(one.lines.startsWith('M91,44 L91,56'), one.lines);
  assert.deepEqual(one.circles, [{ x: 82, y: 50 }, { x: 16, y: 10 }]);
  assert.ok(!one.lines.includes('M13,'), 'optional parent end has "o|", not "||"');
});

test('hull, intersection and bounds', () => {
  const c = { start: { x: 0, y: 0 }, end: { x: 100, y: 50 }, c1: { x: 50, y: 0 }, c2: { x: 50, y: 50 }, d1: 1, d2: -1 };
  assert.deepEqual(hull(c), { x: -24, y: -10, width: 148, height: 70 });
  assert.equal(intersects({ x: 0, y: 0, width: 10, height: 10 }, { x: 10, y: 0, width: 5, height: 5 }), false, 'touching edges');
  assert.equal(intersects({ x: 0, y: 0, width: 10, height: 10 }, { x: 9, y: 9, width: 5, height: 5 }), true);
  assert.deepEqual(boundsOf([{ x: 10, y: 5, width: 10, height: 10 }, { x: -5, y: 20, width: 5, height: 5 }]), { x: -5, y: 5, width: 25, height: 20 });
  assert.equal(boundsOf([]), undefined);
});

test('pan/zoom math keeps the point under the pointer', () => {
  const v = { x: 40, y: 20, zoom: 1 };
  const z = zoomAt(v, 2, { x: 140, y: 120 });
  assert.equal(z.zoom, 2);
  // world point under the pointer before: (100, 100); after: still at screen (140, 120)
  assert.deepEqual([100 * z.zoom + z.x, 100 * z.zoom + z.y], [140, 120]);
  assert.equal(zoomAt(v, 100, { x: 0, y: 0 }).zoom, Metrics.maxZoom);
  assert.equal(zoomAt(v, 0.0001, { x: 0, y: 0 }).zoom, Metrics.minZoom);
  const f = fit({ x: 100, y: 100, width: 1000, height: 500 }, { width: 548, height: 548 });
  assert.equal(f.zoom, 0.5);
  assert.deepEqual(worldViewport(f, { width: 548, height: 548 }).x, 100 - 24 / 0.5);
  assert.equal(fit({ x: 0, y: 0, width: 10, height: 10 }, { width: 1000, height: 1000 }).zoom, 1.5, 'never above 150%');
  const c = centreOn({ x: 0, y: 0, zoom: 2 }, { x: 50, y: 50 }, { width: 400, height: 200 });
  assert.deepEqual(c, { x: 100, y: 0, zoom: 2 });
});

test('header colour matches the app hash; search, neighbours, ellipsis', () => {
  // Swift: var h: UInt32 = 5381; for b in "public".utf8 { h = (h &* 33) ^ UInt32(b) }; h % 360
  let h = 5381n;
  for (const b of Buffer.from('public')) h = ((h * 33n) & 0xffffffffn) ^ BigInt(b);
  assert.equal(schemaHue('public'), Number(h % 360n));
  assert.match(headerColor('public')!, /^hsl\(\d+ 34\.43% 54\.25%\)$/);
  assert.equal(headerColor(''), undefined);
  const es = [entity('orders', []), entity('order_items', []), { ...entity('users', []), id: 'auth.users' }];
  assert.deepEqual(search(es, ' ORDER'), ['orders', 'order_items']);
  assert.deepEqual(search(es, 'auth.'), ['auth.users']);
  assert.deepEqual(search(es, '  '), []);
  const rels = [rel('order_items', 'order_id', 'orders'), rel('orders', 'user_id', 'auth.users')];
  assert.deepEqual([...neighbours(rels, 'orders')].sort(), ['auth.users', 'order_items', 'orders']);
  const m = (t: string) => t.length * 10;
  assert.equal(ellipsize('short', 100, m), 'short');
  assert.equal(ellipsize('abcdefghijkl', 60, m), 'abcde…');
  assert.equal(compactRows(950), '~950 rows');
  assert.equal(compactRows(12_345), '~12.3k rows');
  assert.equal(compactRows(2_500_000), '~2.5M rows');
  assert.equal(badge(attr('id', { isPrimaryKey: true, isForeignKey: true })), 'PK');
  assert.equal(badge(attr('x', { isUnique: true })), 'UQ');
  assert.equal(exportScale({ width: 1000, height: 400 }), 2);
  assert.equal(exportScale({ width: 32_000, height: 400 }), 0.5);
});

test('every parity schema measures and routes without NaN', () => {
  for (const name of ['postgres', 'mysql', 'mongodb']) {
    const model = (JSON.parse(readFileSync(new URL(`../../Tests/Parity/schema/${name}.json`, import.meta.url), 'utf8')) as { output: SchemaModel }).output;
    const byId = new Map(model.entities.map((e) => [e.id, e]));
    const rects = new Map(model.entities.map((e, i) => [e.id, { x: (i % 4) * 500, y: Math.floor(i / 4) * 400, ...entitySize(e, measure) }]));
    for (const r of model.relationships) {
      const c = curve(r, byId.get(r.fromEntity)!, byId.get(r.toEntity)!, rects.get(r.fromEntity)!, rects.get(r.toEntity)!);
      assert.doesNotMatch(curvePath(c) + marks(r, c).lines, /NaN|undefined/, `${name}: ${r.id}`);
    }
  }
});
