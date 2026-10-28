// Dev harness: stands in for VS Code. Defines `acquireVsCodeApi` and answers the webview's messages from fixture data,
// so dist/webview/*.js runs in a plain browser (docs/vscode-extension.md, "Webviews"). Query string: `theme=dark|light|hc`, and for
// er.html `fixture=postgres|mysql|mongodb|synthetic300`, for table.html `fixture=rows|mongo|error|empty`, `delay=<ms>`.
const params = new URLSearchParams(location.search);
document.documentElement.dataset.theme = params.get('theme') ?? 'dark';
const view = document.documentElement.dataset.view;
const fixture = params.get('fixture') ?? (view === 'er' ? 'postgres' : 'rows');
const delay = Number(params.get('delay') ?? 120);

const send = (data) => setTimeout(() => window.dispatchEvent(new MessageEvent('message', { data })), 0);
let state;
window.__goku = { sent: [], received: [], copied: [], exports: [] };

window.acquireVsCodeApi = () => ({
  postMessage(m) {
    window.__goku.received.push(m);
    (view === 'er' ? er : table)(m);
  },
  getState: () => state,
  setState: (s) => (state = s),
});

const reply = (m) => {
  window.__goku.sent.push(m);
  send(m);
};

// MARK: - Table: a synthetic table of 2,500 rows × 50 columns, served in pages

const TOTAL = 2500;
const COLUMNS = Array.from({ length: 50 }, (_, i) =>
  i === 0
    ? { name: 'id', typeName: 'bigint' }
    : [
        { name: `payload_${i}`, typeName: 'jsonb' },
        { name: `name_${i}`, typeName: 'text' },
        { name: `amount_${i}`, typeName: 'numeric(12,2)' },
        { name: `created_at_${i}`, typeName: 'timestamp with time zone' },
        { name: `flag_${i}`, typeName: 'boolean' },
      ][i % 5],
);
const WORDS = ['alpha', 'bravo', 'charlie', 'delta', 'echo', 'foxtrot', 'golf', 'hotel', 'india', 'juliet'];
function cell(r, c) {
  if (c === 0) return String(r + 1);
  if ((r * 7 + c * 3) % 23 === 0) return null;
  switch (c % 5) {
    case 1: return r % 97 === 3 && c === 1 ? `${'Long description '.repeat(40)}… [truncated]` : `${WORDS[(r + c) % 10]} ${WORDS[(r * c) % 10]}`;
    case 2: return ((r * 37 + c * 11) % 100000 / 100).toFixed(2);
    case 3: return `2026-0${1 + (r % 9)}-1${r % 10} 0${c % 10}:${10 + (r % 50)}:00+00`;
    case 4: return (r + c) % 2 ? 'true' : 'false';
    default: return r % 41 === 5 ? '{"note": "multi\nline", "tags": ["a", "b"]}' : `{"n": ${r}, "k": "${WORDS[c % 10]}"}`;
  }
}
const ALL = Array.from({ length: TOTAL }, (_, r) => COLUMNS.map((_, c) => cell(r, c)));

const MONGO_COLUMNS = [
  { name: '_id', typeName: 'objectId' },
  { name: 'name', typeName: 'string' },
  { name: 'email', typeName: 'string' },
  { name: 'address.city', typeName: 'string' },
  { name: 'tags', typeName: 'array' },
  { name: 'createdAt', typeName: 'date' },
];
const MONGO = Array.from({ length: 120 }, (_, i) => {
  const id = (0x65f0a0000000000000000000n + BigInt(i)).toString(16);
  const city = i % 4 === 0 ? null : ['Milan', 'Rome', 'Turin'][i % 3];
  const row = [id, `Customer ${i + 1}`, `user${i + 1}@example.com`, city, `["vip", "q${i % 4}"]`, `2026-10-${10 + (i % 18)}T08:00:00Z`];
  const doc = `{\n  _id: ObjectId("${id}"),\n  name: "Customer ${i + 1}",\n  email: "user${i + 1}@example.com",\n  address: ${city ? `{ city: "${city}", zip: "2010${i % 10}" }` : 'null'},\n  tags: ["vip", "q${i % 4}"],\n  createdAt: ISODate("${row[5]}")\n}`;
  return { row, doc };
});

let seq = 0;
function table(m) {
  if (m.type === 'copy') return void window.__goku.copied.push(m.text);
  if (m.type === 'cancel') return reply({ type: 'cancelled', requestId: seq });
  if (m.type === 'ready') {
    const mongo = fixture === 'mongo';
    reply({
      type: 'init',
      title: mongo ? 'customers — shop' : 'orders — shop',
      object: { database: 'shop', schema: mongo ? '' : 'public', name: mongo ? 'customers' : 'orders', kind: mongo ? 'collection' : 'table' },
      readOnly: true,
      pageSize: 1000,
    });
  }
  if (m.type === 'ready' || m.type === 'load') {
    const offset = m.offset ?? 0;
    const sort = m.sort;
    const id = ++seq;
    reply({ type: 'loading', requestId: id, offset, sort });
    if (fixture === 'loading') return;
    setTimeout(() => {
      if (id !== seq) return;
      if (fixture === 'error')
        return reply({ type: 'error', requestId: id, message: 'permission denied for table orders (SQLSTATE 42501)' });
      if (fixture === 'empty')
        return reply({ type: 'page', requestId: id, offset, sort, result: { columns: COLUMNS.slice(0, 6), rows: [], truncated: false, elapsedMs: 3 } });
      if (fixture === 'mongo') {
        const page = MONGO.slice(offset, offset + 100);
        return reply({
          type: 'page', requestId: id, offset, sort,
          result: { columns: MONGO_COLUMNS, rows: page.map((d) => d.row), documents: page.map((d) => d.doc), truncated: offset + 100 < MONGO.length, elapsedMs: 9 },
        });
      }
      let rows = ALL;
      if (sort) {
        const c = COLUMNS.findIndex((x) => x.name === sort.column);
        rows = [...ALL].sort((a, b) => {
          const [x, y] = [a[c], b[c]];
          const k = x === null ? -1 : y === null ? 1 : c % 5 === 2 || c === 0 ? Number(x) - Number(y) : x.localeCompare(y);
          return sort.descending ? -k : k;
        });
      }
      const page = rows.slice(offset, offset + 1000);
      reply({ type: 'page', requestId: id, offset, sort, result: { columns: COLUMNS, rows: page, truncated: offset + 1000 < TOTAL, elapsedMs: 42 } });
    }, delay);
  }
}

// MARK: - ER: parity fixtures (or 300 synthetic tables) with a stub layout; the real layout comes from the engine

let model;
async function loadModel() {
  if (fixture === 'synthetic300') return synthetic(300);
  const res = await fetch(`/Tests/Parity/schema/${fixture}.json`);
  return (await res.json()).output;
}

function synthetic(n) {
  const entities = [];
  const relationships = [];
  for (let i = 0; i < n; i++) {
    const schema = ['public', 'billing', 'auth', 'audit'][i % 4];
    const name = `${WORDS[i % 10]}_${Math.floor(i / 10)}`;
    const attributes = [{ name: 'id', type: 'bigint', nullable: false, isPrimaryKey: true, isForeignKey: false, isUnique: false }];
    const parent = i > 3 ? Math.floor((((i * 2654435761) % 4294967296) / 4294967296) * i) : -1;
    if (parent >= 0) attributes.push({ name: `${entities[parent].name}_id`, type: 'bigint', nullable: i % 3 === 0, isPrimaryKey: false, isForeignKey: true, isUnique: false });
    for (let k = 0; k < 3 + (i % 9); k++) attributes.push({ name: `col_${k}`, type: k % 2 ? 'text' : 'integer', nullable: k % 3 !== 0, isPrimaryKey: false, isForeignKey: false, isUnique: k === 4 });
    entities.push({ id: `${schema}.${name}`, schema, name, kind: 'table', attributes, indexes: [], rowEstimate: i * 1234 });
    if (parent >= 0) {
      const p = entities[parent];
      relationships.push({ id: `${schema}.${name}->${p.id}`, name: `${name}_fk`, fromEntity: `${schema}.${name}`, fromColumns: [`${p.name}_id`], toEntity: p.id, toColumns: ['id'], cardinality: 'manyToOne', optional: i % 3 === 0, inferred: false });
    }
  }
  return { database: 'synthetic', engine: 'postgres', entities, relationships, warnings: [] };
}

/** Layers by longest path to a parent-less table (parents left), stacked per layer: close enough for screenshots. */
function stubLayout(sizes) {
  const parents = new Map(model.entities.map((e) => [e.id, []]));
  for (const r of model.relationships) if (r.fromEntity !== r.toEntity) parents.get(r.fromEntity)?.push(r.toEntity);
  const depth = new Map();
  const visit = (id, seen) => {
    if (depth.has(id)) return depth.get(id);
    if (seen.has(id)) return 0;
    seen.add(id);
    const d = Math.max(-1, ...parents.get(id).map((p) => visit(p, seen))) + 1;
    depth.set(id, d);
    return d;
  };
  const layers = [];
  for (const e of model.entities) if (sizes[e.id]) (layers[visit(e.id, new Set())] ??= []).push(e.id);
  const out = {};
  let x = 0;
  for (const layer of layers.filter(Boolean)) {
    const colWidth = Math.max(...layer.map((id) => sizes[id].width));
    let y = 0;
    let colX = x;
    for (const id of layer) {
      if (y > 2400) [y, colX] = [0, colX + colWidth + 50];
      out[id] = { x: colX, y };
      y += sizes[id].height + 36;
    }
    x = colX + colWidth + 90;
  }
  return out;
}

async function er(m) {
  switch (m.type) {
    case 'ready':
      reply({ type: 'init', title: `${fixture === 'synthetic300' ? 'synthetic' : 'shop'} — ER diagram`, database: 'shop', schemas: [] });
      reply({ type: 'loading', message: 'Reading the schema of shop…' });
      if (fixture === 'loading') return;
      model = await loadModel();
      setTimeout(() => reply({ type: 'model', model }), delay);
      break;
    case 'sizes':
    case 'autoLayout':
      if (m.sizes) window.__goku.sizes = m.sizes;
      setTimeout(() => reply({ type: 'layout', positions: stubLayout(window.__goku.sizes) }), 30);
      break;
    case 'export':
      window.__goku.exports.push(m);
      reply({ type: 'exported', path: `/tmp/shop-er.${m.format}` });
      break;
    case 'reload':
      er({ type: 'ready' });
      break;
  }
}
