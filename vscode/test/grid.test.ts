import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  CLIP_MARKER,
  cellText,
  columnAt,
  columnRange,
  columnWidth,
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
  tsvField,
  visibleRange,
} from '../webview/lib/grid.ts';

test('visible rows with overscan, clamped to the data', () => {
  assert.deepEqual(visibleRange(0, 220, 22, 1000), { start: 0, end: 10 });
  assert.deepEqual(visibleRange(2200, 220, 22, 1000, 5), { start: 95, end: 115 });
  assert.deepEqual(visibleRange(2211, 220, 22, 1000), { start: 100, end: 111 }, 'a half row at each edge counts');
  assert.deepEqual(visibleRange(1e9, 220, 22, 1000, 5), { start: 1000, end: 1000 }, 'scrolled past the end');
  assert.deepEqual(visibleRange(-50, 220, 22, 3), { start: 0, end: 3 });
  assert.deepEqual(visibleRange(0, 220, 22, 0), { start: 0, end: 0 });
});

test('column offsets and binary search', () => {
  const o = prefixOffsets([100, 50, 200]);
  assert.deepEqual(o, [0, 100, 150, 350]);
  assert.equal(columnAt(o, 0), 0);
  assert.equal(columnAt(o, 99.5), 0);
  assert.equal(columnAt(o, 100), 1);
  assert.equal(columnAt(o, 349), 2);
  assert.equal(columnAt(o, 5000), 2, 'clamped to the last column');
  assert.equal(columnAt([0], 10), -1);
  assert.deepEqual(columnRange(o, 120, 40), { start: 1, end: 3 });
  assert.deepEqual(columnRange(o, 0, 100), { start: 0, end: 1 });
  assert.deepEqual(columnRange(o, 0, 100, 2), { start: 0, end: 3 });
});

test('1,000 × 50 grid renders a small window of cells', () => {
  const o = prefixOffsets(Array.from({ length: 50 }, (_, i) => 80 + (i % 7) * 20));
  const rows = visibleRange(11_000, 600, 22, 1000, 6);
  const cols = columnRange(o, 2_000, 1200, 2);
  const cells = (rows.end - rows.start) * (cols.end - cols.start);
  assert.ok(cells < 1000, `${cells} cells`);
  assert.ok(o[cols.start]! <= 2_000 && o[cols.end]! >= 3_200);
});

test('clipped and NULL cells', () => {
  assert.equal(isClipped(`abc${CLIP_MARKER}`), true);
  assert.equal(isClipped('abc'), false);
  assert.equal(isClipped(null), false);
  assert.equal(cellText(null), 'NULL');
  assert.equal(cellText(`abc${CLIP_MARKER}`), 'abc');
  assert.equal(cellText('a\nb\r\nc\td'), 'a ↵ b ↵ c d');
  assert.equal(cellText('x'.repeat(600)).length, 500);
});

test('column width from header and samples, clamped', () => {
  const m = (t: string) => t.length * 7;
  assert.equal(columnWidth('id', ['1', '22'], m), 64);
  assert.equal(columnWidth('name', ['x'.repeat(30)], m), 30 * 7 + 24);
  assert.equal(columnWidth('long', ['x'.repeat(500)], m), 360);
  assert.equal(columnWidth('n', [null], m), 64);
  assert.deepEqual(sampleIndexes(3, 10), [0, 1, 2]);
  assert.deepEqual(sampleIndexes(1000, 4), [0, 250, 500, 750]);
});

test('TSV follows RFC 4180 quoting, NULL is empty', () => {
  assert.equal(tsvField(null), '');
  assert.equal(tsvField('plain'), 'plain');
  assert.equal(tsvField('a\tb'), '"a\tb"');
  assert.equal(tsvField('line\nbreak'), '"line\nbreak"');
  assert.equal(tsvField('say "hi"'), '"say ""hi"""');
  assert.equal(toTsv([['1', null], ['2', 'x\ty']], ['id', 'v']), 'id\tv\n1\t\n2\t"x\ty"');
});

test('selection rectangles and movement', () => {
  const rect = selectionRect({ anchor: { row: 5, col: 3 }, focus: { row: 2, col: 1 } });
  assert.deepEqual(rect, { r0: 2, r1: 5, c0: 1, c1: 3 });
  const rows = [['a', 'b', 'c'], ['d', null, 'f'], ['g', 'h', 'i']];
  assert.deepEqual(sliceRect(rows, { r0: 1, r1: 2, c0: 1, c1: 2 }), [[null, 'f'], ['h', 'i']]);
  assert.deepEqual(sliceRect(rows, { r0: 2, r1: 9, c0: 0, c1: 0 }), [['g']], 'rows past the end are ignored');
  assert.deepEqual(moveCell({ row: 0, col: 0 }, -1, 5, 10, 3), { row: 0, col: 2 });
  assert.deepEqual(moveCell({ row: 4, col: 1 }, 1e9, -1e9, 10, 3), { row: 9, col: 0 });
});

test('header click cycles ascending, descending, unsorted', () => {
  const a = nextSort(undefined, 'id');
  assert.deepEqual(a, { column: 'id', descending: false });
  const d = nextSort(a, 'id');
  assert.deepEqual(d, { column: 'id', descending: true });
  assert.equal(nextSort(d, 'id'), undefined);
  assert.deepEqual(nextSort(d, 'name'), { column: 'name', descending: false });
});

test('paging offsets and labels', () => {
  assert.deepEqual(pageOffsets(0, 100, 100, true), { prev: undefined, next: 100 });
  assert.deepEqual(pageOffsets(100, 40, 100, false), { prev: 0, next: undefined });
  assert.deepEqual(pageOffsets(50, 100, 100, true), { prev: 0, next: 150 });
  assert.deepEqual(pageOffsets(0, 0, 100, true), { prev: undefined, next: undefined });
  assert.equal(pageLabel(0, 1000, true), 'Rows 1–1,000 · more rows');
  assert.equal(pageLabel(1000, 5, false), 'Rows 1,001–1,005');
  assert.equal(pageLabel(0, 0, false), 'No rows');
  assert.equal(pageLabel(200, 0, false), 'No more rows');
});
