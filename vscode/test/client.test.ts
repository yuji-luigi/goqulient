// CrashBudget is pure; the rest runs the real goku-engine-server (a debug build, for `debug/wait`) when
// GOKU_ENGINE_BIN points to one: `scripts/dev.sh vscode test` builds it in the engine container and sets it.
import assert from 'node:assert/strict';
import { describe, test } from 'node:test';
import { CancellationTokenSource } from 'vscode-jsonrpc/node.js';
import { CrashBudget, EngineClient, EngineError, ErrorCode, type Log } from '../src/engine/client.ts';

test('crash budget allows max restarts per window', () => {
  let now = 0;
  const b = new CrashBudget(2, 1000, () => now);
  assert.equal(b.record(), true);
  assert.equal(b.record(), true);
  assert.equal(b.record(), false);
  now = 5000;
  assert.equal(b.record(), true, 'old crashes leave the window');
  b.reset();
  assert.equal(b.record(), true);
});

const bin = process.env.GOKU_ENGINE_BIN;

function collect(): Log & { lines: string[] } {
  const lines: string[] = [];
  return { lines, info: (m) => lines.push(m), error: (m) => lines.push(`ERROR ${m}`) };
}

function waitFor(cond: () => boolean, ms = 5000): Promise<void> {
  const until = Date.now() + ms;
  return new Promise((resolve, reject) => {
    const tick = () => (cond() ? resolve() : Date.now() > until ? reject(new Error('timed out waiting')) : setTimeout(tick, 20));
    tick();
  });
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

describe('with the real engine', { skip: bin ? false : 'GOKU_ENGINE_BIN not set' }, () => {
  const client = (extra: Partial<ConstructorParameters<typeof EngineClient>[0]> = {}) =>
    new EngineClient({ binary: () => bin!, clientName: 'node-test', log: collect(), ...extra });

  test('initialize, requests and engine errors', async () => {
    const log = collect();
    const c = client({ log });
    try {
      const info = await c.info();
      assert.equal(info.protocolVersion, 1);
      assert.ok(info.capabilities.methods.includes('profile/validate'));
      const r = await c.request<{ endpoint: string }>('profile/validate', {
        profile: { name: 'm', kind: 'mongodb', mode: 'uri', uri: 'mongodb://u:hunter2@h/db' },
      });
      assert.equal(r.endpoint, 'mongodb://u:****@h/db');
      await assert.rejects(c.request('profile/validate', { profile: { name: 'x', kind: 'mysql', hosts: [] } }), (e: EngineError) => {
        assert.equal(e.code, ErrorCode.engine);
        assert.equal(e.kind, 'invalidConfiguration');
        assert.match(e.message, /^Invalid configuration: Every host needs/);
        return true;
      });
      assert.ok(log.lines.some((l) => l.includes('[engine] goku-engine') && l.includes('initialized by node-test')));
    } finally {
      await c.stop();
    }
  });

  test('a VS Code cancellation token cancels on the engine', async () => {
    const c = client();
    try {
      const source = new CancellationTokenSource();
      const pending = c.request('debug/wait', { seconds: 30, deadline: 60 }, source.token);
      setTimeout(() => source.cancel(), 100);
      await assert.rejects(pending, (e: EngineError) => e.cancelled);
      await assert.rejects(c.request('debug/wait', { seconds: 30, deadline: 0.05 }), (e: EngineError) => e.kind === 'timeout');
    } finally {
      await c.stop();
    }
  });

  test('stop ends the process cleanly', async () => {
    const c = client();
    await c.info();
    const pid = (await c.pid())!;
    await c.stop();
    assert.equal(alive(pid), false);
  });

  test('a crashed engine restarts, then gives up when the budget is spent', async () => {
    let gaveUp = '';
    const c = client({ budget: new CrashBudget(1, 60_000), onGaveUp: (r) => (gaveUp = r) });
    try {
      await c.info();
      const first = (await c.pid())!;
      process.kill(first, 'SIGKILL');
      let second: number | undefined;
      await waitFor(() => (void c.pid().then((p) => (second = p), () => {}), second !== undefined && second !== first));
      assert.ok(alive(second!), 'restarted with a new process');
      assert.equal((await c.info()).protocolVersion, 1);
      process.kill(second!, 'SIGKILL');
      await waitFor(() => gaveUp !== '');
      assert.match(gaveUp, /crashed repeatedly/);
      const third = (await c.restart()).engineVersion;
      assert.ok(third, 'restart() works after giving up');
    } finally {
      await c.stop();
    }
  });

  test('a missing binary is a readable error', async () => {
    const c = new EngineClient({ binary: () => '/nonexistent/goku-engine-server', clientName: 't', log: collect() });
    await assert.rejects(c.info(), /Couldn't start the Goku engine/);
  });
});
