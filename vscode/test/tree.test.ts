import assert from 'node:assert/strict';
import { describe, test } from 'node:test';
import type { CancellationToken } from 'vscode-jsonrpc/node.js';
import type { Connection } from '../src/connections/model.ts';
import { EngineError } from '../src/engine/client.ts';
import {
  columnDescription,
  connectionDescription,
  connectionTooltip,
  erLayoutKey,
  groupObjects,
  isAuthFailure,
  sourceGroups,
} from '../src/tree/model.ts';
import { Sessions, type ConnectHooks, type Target } from '../src/tree/sessions.ts';
import type { ObjectRef } from '../src/views/protocol.ts';

const ref = (name: string, kind: ObjectRef['kind']): ObjectRef => ({ database: 'shop', schema: 'public', name, kind });
const conn = (over: Partial<Connection>): Connection => ({ id: 'a', name: 'A', source: 'user', endpoint: 'h:5432', readOnly: false, ...over });

describe('tree model', () => {
  test('kind groups follow the app order and skip empty kinds', () => {
    const groups = groupObjects([ref('v', 'view'), ref('t2', 'table'), ref('f', 'function'), ref('t1', 'table'), ref('m', 'materializedView')]);
    assert.deepEqual(
      groups.map((g) => [g.label, g.objects.map((o) => o.name)]),
      [['Tables', ['t2', 't1']], ['Views', ['v']], ['Materialized Views', ['m']], ['Functions', ['f']]],
    );
    assert.deepEqual(groupObjects([]), []);
  });

  test('ER layout key per connection, database and schemas', () => {
    assert.equal(erLayoutKey('c1', 'shop', []), 'goqulient.er:c1:shop:');
    assert.equal(erLayoutKey('c1', 'shop', ['public', 'audit']), 'goqulient.er:c1:shop:public,audit');
  });

  test('wrong-password messages of the three drivers, and not other access errors', () => {
    for (const m of [
      'Connection failed: db error: FATAL: password authentication failed for user "dbc"',
      "Connection failed: Access denied for user 'dbc'@'172.18.0.5' (using password: YES)",
      'Connection failed: Command failed (AuthenticationFailed): Authentication failed.',
      'Connection failed: SCRAM failure: bad auth',
    ]) {
      assert.ok(isAuthFailure(m), m);
    }
    for (const m of ["Access denied for user 'dbc'@'%' to database 'secret'", 'Timed out: connect', 'Connection refused']) {
      assert.ok(!isAuthFailure(m), m);
    }
  });

  test('source groups: user, workspace, one per env file, in list order', () => {
    const list = [
      conn({ id: '1', source: 'user' }),
      conn({ id: '2', source: 'env', file: '.env' }),
      conn({ id: '3', source: 'workspace' }),
      conn({ id: '4', source: 'env', file: 'api/.env' }),
      conn({ id: '5', source: 'env', file: '.env' }),
    ];
    assert.deepEqual(
      sourceGroups(list).map((g) => [g.label, g.connections.map((c) => c.id)]),
      [['User Settings', ['1']], ['.env', ['2', '5']], ['Workspace Settings', ['3']], ['api/.env', ['4']]],
    );
  });

  test('descriptions and tooltips', () => {
    assert.equal(connectionDescription(conn({ readOnly: true })), 'read-only · h:5432');
    assert.equal(connectionDescription(conn({}), 'connecting…'), 'connecting… · h:5432');
    assert.equal(connectionDescription(conn({ invalid: '“kind” is missing.' })), '“kind” is missing.');
    const tip = connectionTooltip(conn({ source: 'env', file: '.env', variable: 'DATABASE_URL', readOnly: true }));
    assert.match(tip, /From: DATABASE_URL in \.env/);
    assert.match(tip, /Read-only/);
    const col = { name: 'id', type: 'integer', nullable: false, isPrimaryKey: true, isForeignKey: false, isUnique: true };
    assert.equal(columnDescription(col), 'integer · PK · NOT NULL');
    assert.equal(columnDescription({ ...col, isPrimaryKey: false, nullable: true, presence: 0.5 }), 'integer · unique · 50%');
  });
});

/** A scripted engine: `connection/open` answers from `opens` in order (an Error = a failure). */
class FakeEngine {
  opens: (string | Error | 'hang')[] = [];
  calls: { method: string; params: Record<string, unknown> }[] = [];
  stopListeners = new Set<() => void>();
  private next = 1;

  onDidStop(l: () => void) {
    this.stopListeners.add(l);
    return () => this.stopListeners.delete(l);
  }

  stop() {
    for (const l of this.stopListeners) l();
  }

  async request<R>(method: string, params: Record<string, unknown>, token?: CancellationToken): Promise<R> {
    this.calls.push({ method, params });
    if (method === 'connection/open') {
      const step = this.opens.shift() ?? 'ok';
      if (step instanceof Error) throw step;
      if (step === 'hang') {
        await new Promise<void>((_, reject) => token?.onCancellationRequested(() => reject(new EngineError(-32800, 'Cancelled'))));
      }
      return { sessionId: `s${this.next++}`, serverInfo: { product: 'PostgreSQL', version: '17', connectedHost: 'h', currentUser: 'u', isReadOnlyNode: false } } as R;
    }
    if (method === 'catalog/databases' && params.sessionId === 'gone') throw new EngineError(-32001, 'Not connected', 'notConnected');
    return {} as R;
  }
}

const T: Target = { id: 'a', name: 'A', key: '{"kind":"postgres"}' };
const authError = () => new EngineError(-32001, 'Connection failed: password authentication failed for user "u"', 'connectionFailed');

function setup(source: 'saved' | 'none' = 'saved', again: string | null = 'right') {
  const engine = new FakeEngine();
  const rejected: string[] = [];
  const hooks: ConnectHooks = {
    params: async () => ({ profile: { name: 'A', kind: 'postgres' }, credentials: { password: 'wrong' }, source }),
    rejected: async (_t, message) => {
      rejected.push(message);
      return again === null ? undefined : { password: again };
    },
  };
  const sessions = new Sessions(engine, hooks);
  const events: [string, boolean][] = [];
  sessions.onDidChange((id, collapsed) => events.push([id, collapsed]));
  return { engine, sessions, rejected, events };
}

describe('Sessions', () => {
  test('connect once, share concurrent attempts, disconnect closes the session', async () => {
    const { engine, sessions, events } = setup();
    const [a, b] = await Promise.all([sessions.connect(T), sessions.connect(T)]);
    assert.equal(a, b);
    assert.equal(sessions.sessionId('a'), 's1');
    assert.equal(engine.calls.filter((c) => c.method === 'connection/open').length, 1);
    await sessions.disconnect('a');
    assert.equal(sessions.state('a').status, 'idle');
    assert.deepEqual(engine.calls.at(-1), { method: 'connection/close', params: { sessionId: 's1' } });
    assert.equal(sessions.epoch('a'), 1);
    assert.deepEqual(events, [['a', false], ['a', false], ['a', true]]);
  });

  test('a rejected password is asked for once more', async () => {
    const { engine, sessions, rejected } = setup();
    engine.opens = [authError(), 'ok'];
    const state = await sessions.connect(T);
    assert.equal(state.status, 'connected');
    assert.equal(rejected.length, 1);
    assert.deepEqual(engine.calls.filter((c) => c.method === 'connection/open').map((c) => c.params.credentials), [{ password: 'wrong' }, { password: 'right' }]);
  });

  test('a second rejection, a dismissed prompt or an env password fails with the engine message', async () => {
    for (const [source, again, opens, prompts] of [
      ['saved', 'still-wrong', [authError(), authError()], 1],
      ['saved', null, [authError()], 1],
      ['none', 'x', [authError()], 0],
    ] as const) {
      const { engine, sessions, rejected } = setup(source, again);
      engine.opens = [...opens];
      const state = await sessions.connect(T);
      assert.equal(state.status, 'failed');
      assert.match((state as { message: string }).message, /password authentication failed/);
      assert.equal(rejected.length, prompts);
    }
  });

  test('a failure stays until Retry; other errors are not retried', async () => {
    const { engine, sessions, rejected } = setup();
    engine.opens = [new EngineError(-32001, 'Connection failed: refused', 'connectionFailed')];
    assert.equal((await sessions.connect(T)).status, 'failed');
    assert.equal(rejected.length, 0);
    assert.equal(sessions.epoch('a'), 0, 'stays expanded to show the error');
    sessions.retry('a');
    assert.equal(sessions.state('a').status, 'idle');
    assert.equal((await sessions.connect(T)).status, 'connected');
  });

  test('Cancel Connecting cancels the engine request and goes back to idle', async () => {
    const { engine, sessions } = setup();
    engine.opens = ['hang'];
    const done = sessions.connect(T);
    await new Promise((r) => setImmediate(r));
    assert.equal(sessions.state('a').status, 'connecting');
    sessions.cancel('a');
    assert.equal(sessions.state('a').status, 'idle');
    await done;
    assert.equal(sessions.state('a').status, 'idle', 'the cancelled attempt does not come back as failed');
    assert.equal(sessions.epoch('a'), 1);
  });

  test('an engine stop or an unknown session makes connections idle without an error', async () => {
    const { engine, sessions } = setup();
    await sessions.connect(T);
    engine.stop();
    assert.equal(sessions.state('a').status, 'idle');
    assert.equal(sessions.epoch('a'), 1);
    engine.opens = ['ok'];
    await sessions.connect(T);
    (sessions as unknown as { entries: Map<string, { state: { sessionId: string } }> }).entries.get('a')!.state.sessionId = 'gone';
    await assert.rejects(sessions.request('a', 'catalog/databases', {}), (e: EngineError) => e.kind === 'notConnected');
    assert.equal(sessions.state('a').status, 'idle');
    assert.equal(sessions.epoch('a'), 2);
  });

  test('sync closes sessions of removed or changed connections', async () => {
    const { engine, sessions } = setup();
    await sessions.connect(T);
    await sessions.connect({ ...T, id: 'b' });
    await sessions.sync([{ ...T, key: 'changed' }]);
    const closed = engine.calls.filter((c) => c.method === 'connection/close').map((c) => c.params.sessionId).sort();
    assert.deepEqual(closed, ['s1', 's2']);
    assert.equal(sessions.state('a').status, 'idle');
    await sessions.connect({ ...T, key: 'changed' });
    await sessions.sync([{ ...T, key: 'changed' }]);
    assert.equal(sessions.state('a').status, 'connected', 'same profile: kept');
  });
});
