// End-to-end check inside a real VS Code (run.mjs): the .env connections appear read-only, expanding them connects and
// lists `shop`, the table viewer gets a page, the ER diagram gets a model and a layout, an engine restart only
// collapses the connections, and no password shows up in any tree item. Reads only (the databases are shared).
const assert = require('node:assert/strict');
const { execFile } = require('node:child_process');
const { join } = require('node:path');
const vscode = require('vscode');

const SECRET = 'dbcpass';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function until(what, fn, ms = 30_000) {
  const end = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() > end) throw new Error(`Timed out waiting for ${what}`);
    await sleep(100);
  }
}

function screenshot(name) {
  const dir = process.env.GOKU_E2E_SCREENSHOTS;
  if (!dir) return Promise.resolve();
  return new Promise((resolve) =>
    execFile('import', ['-window', 'root', join(dir, `${name}.png`)], (e) => {
      if (e) console.log(`  (screenshot ${name} failed: ${e.message})`);
      resolve();
    }),
  );
}

exports.run = async function run() {
  const ext = vscode.extensions.all.find((e) => e.packageJSON.name === 'goqulient' && e.extensionPath);
  assert.ok(ext, 'extension found');
  const api = await ext.activate();
  assert.ok(api?.tree, 'test API (extension runs in test mode)');
  const { tree, view, sessions, store, engine, answered } = api;
  assert.equal(engine.isRunning, false, 'listing connections does not start the engine');

  await vscode.commands.executeCommand('workbench.view.extension.goqulient');
  await until('3 connections', () => store.list().length >= 3);
  const items = [];
  const item = (node) => {
    const i = tree.getTreeItem(node);
    items.push(i);
    return i;
  };
  // Children once nothing is "Connecting…" any more.
  const settled = (node) =>
    until(`children of ${item(node).label}`, async () => {
      const kids = await tree.getChildren(node);
      return kids.some((k) => k.type === 'message' && k.tone === 'loading') ? undefined : kids;
    });
  const named = (kids, label) => {
    const k = kids.find((n) => item(n).label === label);
    assert.ok(k, `“${label}” in [${kids.map((n) => item(n).label).join(', ')}]`);
    return k;
  };

  const roots = await tree.getChildren();
  const labels = roots.map((n) => item(n).label);
  assert.deepEqual(labels, ['DATABASE_URL (.env)', 'MYSQL_URL (.env)', 'MONGODB_URI (.env)']);
  for (const n of roots) {
    assert.equal(n.conn.readOnly, true, `${n.conn.name} read-only`);
    assert.match(String(item(n).description), /read-only/);
  }
  console.log(`  connections: ${labels.join(', ')} (all read-only)`);
  await until('the engine to start when the tree is shown', () => engine.isRunning, 15_000);

  const [pg, my, mongo] = roots;
  const results = {};
  const firstTable = async (conn, path) => {
    let node = conn;
    for (const label of path) node = named(await settled(node), label);
    const objects = await settled(node);
    assert.ok(objects.length > 0, `${conn.conn.name}: objects`);
    const obj = objects[0];
    const columns = await settled(obj);
    assert.ok(columns.length > 0 && columns.every((c) => c.type === 'column'), `${conn.conn.name}: columns of ${obj.ref.name}`);
    results[conn.conn.name] = `${path.join(' › ')} › ${obj.ref.name} (${objects.length} objects, ${columns.length} columns)`;
    return obj;
  };
  const pgTable = await firstTable(pg, ['shop', 'public', 'Tables']);
  const myTable = await firstTable(my, ['shop', 'Tables']);
  const mongoColl = await firstTable(mongo, ['shop', 'Collections']);
  for (const [name, r] of Object.entries(results)) console.log(`  ${name}: ${r}`);
  for (const n of roots) assert.equal(sessions.state(n.conn.id).status, 'connected');

  // Table viewer: the webview loads, says `ready`, and the panel asks the engine for the first page.
  for (const obj of [pgTable, myTable, mongoColl]) {
    const before = answered.filter((m) => m === 'table/page').length;
    const panel = await vscode.commands.executeCommand('goqulient.openTable', obj);
    assert.ok(panel, `panel for ${obj.ref.name}`);
    await until(`a page of ${obj.ref.name}`, () => answered.filter((m) => m === 'table/page').length > before);
  }
  console.log('  table viewer: a page arrived for each engine');
  if (process.env.GOKU_E2E_SCREENSHOTS) {
    await vscode.commands.executeCommand('workbench.action.closeAuxiliaryBar');
    await vscode.commands.executeCommand('notifications.clearAll');
    await vscode.commands.executeCommand('notifications.hideToasts');
    await view.reveal(pgTable, { expand: true, select: true });
  }
  await vscode.commands.executeCommand('goqulient.openTable', pgTable);
  await sleep(1500);
  await screenshot('table');

  // ER diagram: introspection, then the webview measures the boxes and the engine lays them out.
  const pgDb = pgTable.parent.parent.parent;
  assert.equal(pgDb.type, 'database');
  await vscode.commands.executeCommand('goqulient.openErDiagram', pgDb);
  await until('schema/introspect', () => answered.includes('schema/introspect'));
  await until('er/layout', () => answered.includes('er/layout'));
  console.log('  ER diagram: model and layout arrived');
  await vscode.commands.executeCommand('notifications.clearAll');
    await vscode.commands.executeCommand('notifications.hideToasts');
  await sleep(1500);
  await screenshot('er');

  // Disconnect, then an engine restart: connections go idle (no error) and reconnect on the next expand.
  await vscode.commands.executeCommand('goqulient.disconnect', mongo);
  assert.equal(sessions.state(mongo.conn.id).status, 'idle');
  await vscode.commands.executeCommand('goqulient.restartEngine');
  for (const n of roots) assert.equal(sessions.state(n.conn.id).status, 'idle', `${n.conn.name} idle after restart`);
  const again = await settled(pg);
  named(again, 'shop');
  console.log('  disconnect + engine restart: idle, reconnected on expand');

  for (const i of items) {
    const text = JSON.stringify([i.label, i.description, i.tooltip]);
    assert.ok(!text.includes(SECRET), `no password in tree items: ${text}`);
  }
  console.log(`  ${items.length} tree items checked: no password in labels, descriptions or tooltips`);
};
