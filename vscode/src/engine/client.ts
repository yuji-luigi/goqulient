// The extension's side of the engine protocol (docs/vscode-extension.md): spawns goku-engine-server, speaks JSON-RPC
// over its stdio, restarts it after a crash (within a budget) and forwards its stderr to the log. No `vscode` import,
// so the Node tests can run it against the real binary.
import { spawn, type ChildProcess } from 'node:child_process';
import {
  CancellationToken,
  ResponseError,
  StreamMessageReader,
  StreamMessageWriter,
  createMessageConnection,
  type MessageConnection,
} from 'vscode-jsonrpc/node.js';

export const PROTOCOL_VERSION = 1;

/** Error codes the engine answers with (engine-server/src/server.rs `code`). */
export const ErrorCode = {
  methodNotFound: -32601,
  invalidParams: -32602,
  notInitialized: -32002,
  engine: -32001,
  protocolMismatch: -32003,
  cancelled: -32800,
} as const;

export interface Log {
  info(message: string): void;
  error(message: string): void;
}

export interface InitializeResult {
  protocolVersion: number;
  engineVersion: string;
  capabilities: { methods: string[] };
}

/** An error from the engine. `message` is already safe to show (secrets removed). */
export class EngineError extends Error {
  readonly code: number;
  /** `DbError` kind for engine errors (`timeout`, `readOnly`, `invalidConfiguration`, …). */
  readonly kind: string | undefined;
  constructor(code: number, message: string, kind?: string) {
    super(message);
    this.name = 'EngineError';
    this.code = code;
    this.kind = kind;
  }
  get cancelled(): boolean {
    return this.code === ErrorCode.cancelled;
  }
}

/** At most `max` restarts within `windowMs`; after that the engine stays down until the user restarts it. */
export class CrashBudget {
  private readonly crashes: number[] = [];
  private readonly max: number;
  private readonly windowMs: number;
  private readonly now: () => number;
  constructor(max = 3, windowMs = 3 * 60_000, now: () => number = Date.now) {
    this.max = max;
    this.windowMs = windowMs;
    this.now = now;
  }
  /** Records a crash; true when another restart is allowed. */
  record(): boolean {
    const t = this.now();
    while (this.crashes.length && t - this.crashes[0]! > this.windowMs) this.crashes.shift();
    this.crashes.push(t);
    return this.crashes.length <= this.max;
  }
  reset(): void {
    this.crashes.length = 0;
  }
}

export interface EngineClientOptions {
  /** Resolved on every start, so a changed `goqulient.enginePath` applies on restart. */
  binary: () => string;
  clientName: string;
  log: Log;
  budget?: CrashBudget;
  /** Called when the engine died and won't be restarted automatically. */
  onGaveUp?: (reason: string) => void;
}

interface Running {
  process: ChildProcess;
  connection: MessageConnection;
  info: InitializeResult;
  stopping: boolean;
}

export class EngineClient {
  private readonly options: EngineClientOptions;
  private readonly budget: CrashBudget;
  private running: Promise<Running> | undefined;
  private disposed = false;
  /** The initialized process, set once `initialize` answered. */
  private current: Running | undefined;
  private readonly startListeners = new Set<(info: InitializeResult) => void>();
  private readonly stopListeners = new Set<() => void>();

  constructor(options: EngineClientOptions) {
    this.options = options;
    this.budget = options.budget ?? new CrashBudget();
  }

  /** Starts the engine if needed and returns what `initialize` reported. */
  async info(): Promise<InitializeResult> {
    return (await this.ensure()).info;
  }

  /** True while a started engine process is up (doesn't start one). */
  get isRunning(): boolean {
    return this.current !== undefined;
  }

  /** Called after every successful start, including automatic restarts. Returns an unsubscribe function. */
  onDidStart(listener: (info: InitializeResult) => void): () => void {
    this.startListeners.add(listener);
    return () => this.startListeners.delete(listener);
  }

  /** Called when a started engine stops for any reason (crash, `stop`, `restart`): its sessions are gone. */
  onDidStop(listener: () => void): () => void {
    this.stopListeners.add(listener);
    return () => this.stopListeners.delete(listener);
  }

  /** The engine's process id while it runs (for tests and the log). */
  async pid(): Promise<number | undefined> {
    return (await this.running)?.process.pid;
  }

  async request<R>(method: string, params: unknown, token?: CancellationToken): Promise<R> {
    const { connection } = await this.ensure();
    try {
      return await connection.sendRequest<R>(method, params, token ?? CancellationToken.None);
    } catch (e) {
      throw toEngineError(e);
    }
  }

  /** Stops the engine (if running) and starts a fresh one; also forgets earlier crashes. */
  async restart(): Promise<InitializeResult> {
    this.budget.reset();
    await this.stop();
    this.disposed = false;
    return this.info();
  }

  /** `shutdown` + `exit`, then kills the process if it hasn't gone after `graceMs`. */
  async stop(graceMs = 2000): Promise<void> {
    const pending = this.running;
    this.running = undefined;
    this.disposed = true;
    if (!pending) return;
    let r: Running;
    try {
      r = await pending;
    } catch {
      return;
    }
    r.stopping = true;
    const exited = new Promise<void>((resolve) => {
      if (r.process.exitCode !== null || r.process.signalCode !== null) resolve();
      else r.process.once('exit', () => resolve());
    });
    try {
      await withTimeout(r.connection.sendRequest('shutdown'), graceMs);
      await r.connection.sendNotification('exit');
    } catch {
      // The engine is stuck or already gone: the kill below handles both.
    }
    const timer = setTimeout(() => r.process.kill(), graceMs);
    await exited;
    clearTimeout(timer);
    r.connection.dispose();
  }

  private ensure(): Promise<Running> {
    if (!this.running) {
      this.disposed = false;
      this.running = this.start();
      this.running.catch(() => (this.running = undefined));
    }
    return this.running;
  }

  private async start(): Promise<Running> {
    const { log } = this.options;
    const binary = this.options.binary();
    log.info(`Starting ${binary}`);
    // No shell and no secrets in argv or env (AGENTS.md rule 5): connection secrets only travel over stdin.
    const child = spawn(binary, [], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true, shell: false });
    const spawned = new Promise<void>((resolve, reject) => {
      child.once('spawn', resolve);
      child.once('error', (e) => reject(new EngineError(ErrorCode.engine, `Couldn't start the GoQulient engine: ${e.message}`)));
    });
    child.stderr!.setEncoding('utf8');
    child.stderr!.on('data', (chunk: string) => {
      for (const line of chunk.split(/\r?\n/)) if (line.trim()) log.info(`[engine] ${line}`);
    });
    await spawned;
    const connection = createMessageConnection(new StreamMessageReader(child.stdout!), new StreamMessageWriter(child.stdin!));
    connection.listen();
    const running: Running = { process: child, connection, info: undefined as unknown as InitializeResult, stopping: false };
    child.once('exit', (code, signal) => this.exited(running, code, signal));
    try {
      running.info = await withTimeout(
        connection.sendRequest<InitializeResult>('initialize', { protocolVersion: PROTOCOL_VERSION, clientName: this.options.clientName }),
        10_000,
      );
    } catch (e) {
      running.stopping = true;
      child.kill();
      connection.dispose();
      throw toEngineError(e);
    }
    log.info(`Engine ${running.info.engineVersion} ready (protocol ${running.info.protocolVersion})`);
    this.current = running;
    for (const l of this.startListeners) l(running.info);
    return running;
  }

  private exited(r: Running, code: number | null, signal: NodeJS.Signals | null): void {
    r.connection.dispose();
    if (this.current === r) {
      this.current = undefined;
      for (const l of this.stopListeners) l();
    }
    if (r.stopping) return;
    const how = signal ? `signal ${signal}` : `exit code ${code}`;
    this.options.log.error(`The GoQulient engine stopped unexpectedly (${how}).`);
    this.running = undefined;
    if (this.disposed) return;
    if (this.budget.record()) {
      this.ensure().catch((e) => this.options.log.error(`Restart failed: ${(e as Error).message}`));
    } else {
      this.options.onGaveUp?.(`The GoQulient engine crashed repeatedly (last: ${how}). Run “GoQulient: Restart Engine” to try again.`);
    }
  }
}

function toEngineError(e: unknown): EngineError {
  if (e instanceof EngineError) return e;
  if (e instanceof ResponseError) {
    const data = e.data as { kind?: string } | undefined;
    return new EngineError(e.code, e.message, data?.kind);
  }
  return new EngineError(ErrorCode.engine, e instanceof Error ? e.message : String(e));
}

function withTimeout<T>(p: Promise<T>, ms: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const t = setTimeout(() => reject(new EngineError(ErrorCode.engine, `The GoQulient engine didn't answer within ${ms / 1000}s.`)), ms);
    p.then(
      (v) => (clearTimeout(t), resolve(v)),
      (e) => (clearTimeout(t), reject(e)),
    );
  });
}
