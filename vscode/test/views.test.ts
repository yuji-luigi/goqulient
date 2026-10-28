// Extension-side pure helpers of the webviews: saved ER positions and the HTML shell.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { csp, escapeHtml, nonce, shell } from '../src/views/html.ts';
import { capEntities, decodeSaved, encodeSaved, layoutEdges, mergeLayout, partition } from '../src/views/layout.ts';
import type { SchemaModel } from '../src/views/protocol.ts';

test('saved positions decode tolerantly', () => {
  assert.deepEqual(decodeSaved(undefined), {});
  assert.deepEqual(decodeSaved('junk'), {});
  assert.deepEqual(decodeSaved([1, 2]), {});
  const v = { version: 1, positions: { a: { x: 1, y: 2 }, b: { x: 'no', y: 1 }, c: null, d: { x: Infinity, y: 0 }, e: { x: 0, y: 0, z: 9 } } };
  assert.deepEqual(decodeSaved(v), { a: { x: 1, y: 2 }, e: { x: 0, y: 0 } }, "extra fields are dropped");
  assert.deepEqual(decodeSaved({ a: { x: 3, y: 4 } }), { a: { x: 3, y: 4 } }, 'a bare map (no version) is read too');
  assert.deepEqual(decodeSaved(encodeSaved({ q: { x: 5, y: 6 } })), { q: { x: 5, y: 6 } });
  assert.deepEqual(decodeSaved(JSON.parse(JSON.stringify(encodeSaved({ q: { x: 5, y: 6 } })))), { q: { x: 5, y: 6 } });
});

const size = { width: 100, height: 50 };
const rect = (x: number, y: number) => ({ origin: { x, y }, size });

test('merge keeps saved boxes and puts new ones below them as a block', () => {
  const saved = { a: { x: 10, y: 20 }, b: { x: 300, y: 400 }, gone: { x: 0, y: 0 } };
  const sizes = { a: size, b: size, c: size, d: size };
  const fresh = { c: rect(0, 0), d: rect(200, 36) };
  const out = mergeLayout(saved, sizes, fresh);
  assert.deepEqual(out.a, { x: 10, y: 20 });
  assert.deepEqual(out.b, { x: 300, y: 400 });
  assert.equal(out.gone, undefined, 'positions of tables that no longer exist are dropped');
  assert.deepEqual(out.c, { x: 10, y: 530 }, 'below the lowest saved box (450) + gap, left-aligned with them');
  assert.deepEqual(out.d, { x: 210, y: 566 }, 'relative placement inside the new block is kept');
  assert.deepEqual(mergeLayout({}, sizes, fresh), { c: { x: 0, y: 0 }, d: { x: 200, y: 36 } }, 'nothing saved → fresh layout as is');
  assert.deepEqual(mergeLayout(saved, sizes, {}), { a: saved.a, b: saved.b });
  assert.deepEqual(partition(['a', 'c', 'b'], saved), { known: ['a', 'b'], fresh: ['c'] });
});

const model = (n: number): SchemaModel => ({
  database: 'db',
  engine: 'postgres',
  entities: Array.from({ length: n }, (_, i) => ({ id: `t${i}`, schema: '', name: `t${i}`, kind: 'table', attributes: [], indexes: [] })),
  relationships: [
    { id: 'r1', name: 'r1', fromEntity: 't1', fromColumns: ['a'], toEntity: 't0', toColumns: ['id'], cardinality: 'manyToOne', optional: false, inferred: false },
    { id: 'r2', name: 'r2', fromEntity: 't5', fromColumns: ['a'], toEntity: 't1', toColumns: ['id'], cardinality: 'manyToOne', optional: false, inferred: false },
  ],
  warnings: [],
});

test('entity cap and layout edges', () => {
  const m = model(6);
  assert.equal(capEntities(m), m, 'under the cap: untouched');
  const capped = capEntities(m, 3);
  assert.deepEqual(capped.entities.map((e) => e.id), ['t0', 't1', 't2']);
  assert.deepEqual(capped.relationships.map((r) => r.id), ['r1']);
  assert.deepEqual(capped.warnings, ['Showing the first 3 of 6 tables.']);
  assert.deepEqual(layoutEdges(m), [{ child: 't1', parent: 't0' }, { child: 't5', parent: 't1' }]);
  assert.deepEqual(layoutEdges(m, new Set(['t1', 't5'])), [{ child: 't5', parent: 't1' }]);
});

test('HTML shell: nonce CSP, no inline anything else, escaped title', () => {
  const n = nonce();
  assert.match(n, /^[A-Za-z0-9+/]{24}$/);
  assert.notEqual(n, nonce());
  const policy = csp({ cspSource: 'vscode-webview://x', nonce: n });
  assert.match(policy, /^default-src 'none'/);
  assert.match(policy, new RegExp(`script-src 'nonce-${n.replace(/[+/]/g, '\\$&')}'`));
  assert.doesNotMatch(policy, /unsafe-inline|unsafe-eval|https?:/);
  const html = shell({ cspSource: 'vscode-webview://x', nonce: n, scriptUri: 'https://file+.vscode-resource/er.js', styleUri: 's.css', title: '<orders> — "shop"' });
  assert.ok(html.includes('&#60;orders&#62; — &#34;shop&#34;'));
  assert.equal((html.match(/<script /g) ?? []).length, 1);
  assert.ok(html.includes(`<script nonce="${n}" src=`));
  assert.equal(escapeHtml(`a&'b`), 'a&#38;&#39;b');
});
