// Engine sessions by connection id, for the sidebar. Connecting is lazy and cancellable (`$/cancelRequest`
// cancels it in the engine too); a rejected password is asked for once more; an engine stop or an unknown session id
// makes the connection idle again without an error, and the next expand reconnects. No `vscode` import (Node tests).
import { CancellationTokenSource } from 'vscode-jsonrpc/node.js';
import type { Credentials, ProfileJson } from '../connections/profile.ts';
import type { PasswordSource } from '../connections/store.ts';
import type { EngineClient } from '../engine/client.ts';
import { EngineError } from '../engine/client.ts';
import { isAuthFailure } from './model.ts';

export interface ServerInfo {
  product: string;
  version: string;
  connectedHost: string;
  currentUser: string;
  isReadOnlyNode: boolean;
}

export type SessionState =
  | { status: 'idle' }
  | { status: 'connecting' }
  | { status: 'connected'; sessionId: string; serverInfo: ServerInfo }
  /** `message` is the engine's redacted display message (or ours, which never holds a secret). */
  | { status: 'failed'; message: string };

/** What a connection needs to be opened. `key` identifies the profile; a different key later closes the session. */
export interface Target {
  id: string;
  name: string;
  key: string;
}

export interface OpenParams {
  profile: ProfileJson;
  credentials: Credentials;
  source: PasswordSource;
}

export interface ConnectHooks {
  /** Profile + credentials (may prompt). Undefined = the user cancelled. */
  params(target: Target): Promise<OpenParams | undefined>;
  /** The server rejected the password: new credentials to try once more, or undefined to give up. */
  rejected(target: Target, message: string, source: PasswordSource): Promise<Credentials | undefined>;
}

export type Engine = Pick<EngineClient, 'request' | 'onDidStop'>;

interface Entry {
  state: SessionState;
  key?: string;
  /** Bumped whenever the connection goes back to idle, so the tree can collapse it (its node id changes). */
  epoch: number;
  cts?: CancellationTokenSource;
  connecting?: Promise<SessionState>;
}

export class Sessions {
  private readonly engine: Engine;
  private readonly hooks: ConnectHooks;
  private readonly entries = new Map<string, Entry>();
  private readonly listeners = new Set<(id: string, collapsed: boolean) => void>();
  private readonly unsubscribe: () => void;

  constructor(engine: Engine, hooks: ConnectHooks) {
    this.engine = engine;
    this.hooks = hooks;
    // The engine's sessions died with it: idle, no error; the next expand reconnects (to the restarted engine).
    this.unsubscribe = engine.onDidStop(() => {
      for (const [id, e] of this.entries) if (e.state.status !== 'idle') this.reset(id, e);
    });
  }

  /** `collapsed` = the epoch changed (the node should show as a fresh, collapsed one). */
  onDidChange(listener: (id: string, collapsed: boolean) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  state(id: string): SessionState {
    return this.entries.get(id)?.state ?? { status: 'idle' };
  }

  epoch(id: string): number {
    return this.entries.get(id)?.epoch ?? 0;
  }

  /** The open session id, or undefined. */
  sessionId(id: string): string | undefined {
    const s = this.state(id);
    return s.status === 'connected' ? s.sessionId : undefined;
  }

  /** Connects if needed; concurrent calls share one attempt. Resolves to the state it ended in. */
  connect(target: Target): Promise<SessionState> {
    const e = this.entry(target.id);
    if (e.state.status === 'connected') return Promise.resolve(e.state);
    if (e.connecting) return e.connecting;
    const cts = new CancellationTokenSource();
    e.cts = cts;
    e.key = target.key;
    this.set(target.id, { status: 'connecting' });
    const attempt = this.open(target, cts).then(
      (state) => {
        if (e.cts !== cts) return this.state(target.id); // cancelled or reset meanwhile
        e.cts = undefined;
        e.connecting = undefined;
        cts.dispose();
        if (state.status === 'idle') this.reset(target.id, e);
        else this.set(target.id, state);
        return state;
      },
    );
    e.connecting = attempt;
    return attempt;
  }

  /** Stops a connect in progress (also in the engine). The connection goes back to idle. */
  cancel(id: string): void {
    const e = this.entries.get(id);
    if (!e?.cts) return;
    e.cts.cancel();
    this.reset(id, e);
  }

  /** Closes the session (if any) and goes back to idle. */
  async disconnect(id: string): Promise<void> {
    const e = this.entries.get(id);
    if (!e) return;
    const sessionId = e.state.status === 'connected' ? e.state.sessionId : undefined;
    if (e.cts) e.cts.cancel();
    this.reset(id, e);
    if (sessionId) await this.close(sessionId);
  }

  /** Clears a failure so the next expand tries again (the node stays where it is). */
  retry(id: string): void {
    const e = this.entries.get(id);
    if (e?.state.status === 'failed') this.set(id, { status: 'idle' });
  }

  /** Keeps sessions whose target still exists with the same profile; closes the others. */
  async sync(targets: readonly Target[]): Promise<void> {
    const keys = new Map(targets.map((t) => [t.id, t.key]));
    const closing: Promise<void>[] = [];
    for (const [id, e] of this.entries) {
      if (keys.get(id) === e.key) continue;
      if (keys.has(id) || e.state.status !== 'idle') closing.push(this.disconnect(id));
      if (!keys.has(id)) this.entries.delete(id);
    }
    await Promise.all(closing);
  }

  /**
   * A request on the connection's session. A session the engine doesn't know any more (engine restarted) makes the
   * connection idle and rejects with `EngineError` kind `notConnected`.
   */
  async request<R>(id: string, method: string, params: Record<string, unknown>): Promise<R> {
    const sessionId = this.sessionId(id);
    if (!sessionId) throw new EngineError(-32001, 'Not connected', 'notConnected');
    try {
      return await this.engine.request<R>(method, { ...params, sessionId });
    } catch (e) {
      const entry = this.entries.get(id);
      if (e instanceof EngineError && e.kind === 'notConnected' && entry && this.sessionId(id) === sessionId) {
        this.reset(id, entry);
      }
      throw e;
    }
  }

  dispose(): void {
    this.unsubscribe();
    for (const e of this.entries.values()) e.cts?.cancel();
    this.listeners.clear();
  }

  private async open(target: Target, cts: CancellationTokenSource): Promise<SessionState> {
    const cancelled = () => cts.token.isCancellationRequested;
    try {
      const params = await this.hooks.params(target);
      if (!params || cancelled()) return { status: 'idle' };
      let credentials = params.credentials;
      for (let attempt = 0; ; attempt++) {
        try {
          const r = await this.engine.request<{ sessionId: string; serverInfo: ServerInfo }>(
            'connection/open',
            { profile: params.profile, credentials },
            cts.token,
          );
          if (cancelled()) {
            void this.close(r.sessionId); // the open won the race against Cancel
            return { status: 'idle' };
          }
          return { status: 'connected', sessionId: r.sessionId, serverInfo: r.serverInfo };
        } catch (e) {
          if (cancelled() || (e instanceof EngineError && e.cancelled)) return { status: 'idle' };
          const message = e instanceof Error ? e.message : String(e);
          if (attempt > 0 || params.source === 'none' || !isAuthFailure(message)) throw e;
          const again = await this.hooks.rejected(target, message, params.source);
          if (!again || cancelled()) throw e;
          credentials = again;
        }
      }
    } catch (e) {
      if (cancelled()) return { status: 'idle' };
      return { status: 'failed', message: e instanceof Error ? e.message : String(e) };
    }
  }

  private async close(sessionId: string): Promise<void> {
    try {
      await this.engine.request('connection/close', { sessionId });
    } catch {
      // The engine is gone or the session already closed: nothing left to release.
    }
  }

  private entry(id: string): Entry {
    let e = this.entries.get(id);
    if (!e) this.entries.set(id, (e = { state: { status: 'idle' }, epoch: 0 }));
    return e;
  }

  private set(id: string, state: SessionState): void {
    this.entry(id).state = state;
    for (const l of this.listeners) l(id, false);
  }

  private reset(id: string, e: Entry): void {
    e.cts?.dispose();
    e.cts = undefined;
    e.connecting = undefined;
    e.state = { status: 'idle' };
    e.epoch++;
    for (const l of this.listeners) l(id, true);
  }
}
