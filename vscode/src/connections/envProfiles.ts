// Turns `.env` variables (and pasted URLs) into profiles + credentials. Passwords only ever land in `credentials`, which
// the store hands to `connection/open` and drops; the profile is safe to keep, show and validate. Error messages name the
// variable and file, never the value.
import { createHash } from 'node:crypto';
import {
  DEFAULT_PORT,
  KIND_LABEL,
  decode,
  endpointSummary,
  isKind,
  splitUrl,
  type Credentials,
  type HostPort,
  type Kind,
  type ProfileJson,
  type SslMode,
} from './profile.ts';

export const URL_VARS = ['DATABASE_URL', 'POSTGRES_URL', 'POSTGRESQL_URL', 'MYSQL_URL', 'MONGODB_URI', 'MONGO_URL', 'MONGODB_URL'];

/** One `goqulient.envConnections` entry: a custom variable that holds a connection URL. */
export interface EnvMapping {
  name?: string;
  /** Only in this env file (as written in `goqulient.envFiles`); every file when missing. */
  file?: string;
  url?: string;
  /** For URLs whose scheme doesn't say (e.g. `tcp://`); must agree with the scheme when it does. */
  kind?: Kind;
  readOnly?: boolean;
}

export interface EnvFile {
  /** Identifies the file across restarts (workspace folder URI + path); hashed into connection ids. */
  key: string;
  /** Shown in names, e.g. `.env` or `api/.env` in a multi-root workspace. */
  label: string;
  /** The path as written in `goqulient.envFiles`, matched against `EnvMapping.file`. */
  path: string;
}

export interface EnvConnection {
  id: string;
  name: string;
  variable: string;
  file: string;
  profile?: ProfileJson;
  credentials?: Credentials;
  /** Why this variable couldn't become a connection (safe to show: no value in it). */
  error?: string;
}

export class UrlError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'UrlError';
  }
}

export function envId(fileKey: string, variable: string): string {
  return `env-${createHash('sha256').update(`${fileKey}\n${variable}`).digest('hex').slice(0, 16)}`;
}

export function kindOfScheme(scheme: string): Kind | undefined {
  const s = scheme.toLowerCase();
  if (s === 'mongodb' || s === 'mongodb+srv') return 'mongodb';
  // SQLAlchemy-style `postgresql+psycopg://`, `mysql+pymysql://`.
  const base = s.split('+')[0];
  if (base === 'postgres' || base === 'postgresql') return 'postgres';
  if (base === 'mysql' || base === 'mysql2' || base === 'mariadb') return 'mysql';
  return undefined;
}

/** libpq `sslmode`, MySQL `ssl-mode` and `ssl=true/false`. An unknown value becomes the strictest, as in the engine. */
export function sslModeOf(value: string): SslMode {
  switch (value.trim().toLowerCase().replace(/_/g, '-')) {
    case 'disable':
    case 'disabled':
    case 'false':
    case '0':
      return 'disable';
    case 'allow':
    case 'prefer':
    case 'preferred':
      return 'prefer';
    case 'require':
    case 'required':
    case 'true':
    case '1':
      return 'require';
    case 'verify-ca':
      return 'verifyCA';
    default:
      return 'verifyFull';
  }
}

const TARGET_SESSION: Record<string, string> = {
  any: 'any',
  'read-write': 'readWrite',
  'read-only': 'readOnly',
  primary: 'primary',
  standby: 'standby',
  'prefer-standby': 'preferStandby',
};

function parseHosts(list: string, defaultPort: number): HostPort[] {
  return list
    .split(',')
    .map((h) => h.trim())
    .filter(Boolean)
    .map((h) => {
      const m = /^\[([^\]]*)\](?::(.*))?$/.exec(h) ?? /^([^:]*)(?::([^:]*))?$/.exec(h);
      const host = m ? decode(m[1]!) : h; // a bare IPv6 address without brackets keeps the default port
      const portText = m?.[2];
      const port = portText === undefined || portText === '' ? defaultPort : Number(portText);
      if (!Number.isInteger(port) || port < 1 || port > 65535) throw new UrlError('The port must be a number between 1 and 65535.');
      if (host.startsWith('/')) throw new UrlError('Unix socket paths aren’t supported; use a host name.');
      return { host, port };
    });
}

/**
 * A connection URL as a profile (no `id`, `name` = the kind) and the credentials it carries. Throws `UrlError` with a
 * message that never contains the URL.
 */
export function profileFromUrl(url: string, kindHint?: Kind): { profile: ProfileJson; credentials: Credentials } {
  const u = splitUrl(url);
  if (!u) throw new UrlError('Not a connection URL (expected scheme://…).');
  const schemeKind = kindOfScheme(u.scheme);
  if (schemeKind && kindHint && schemeKind !== kindHint) {
    throw new UrlError(`The URL is for ${KIND_LABEL[schemeKind]}, not ${KIND_LABEL[kindHint]}.`);
  }
  const kind = schemeKind ?? kindHint;
  if (!kind) throw new UrlError(`Unsupported URL scheme “${u.scheme}”.`);
  const credentials: Credentials = {};
  const password = u.password === undefined ? undefined : decode(u.password);
  if (kind === 'mongodb') {
    if (!schemeKind) throw new UrlError('A MongoDB URL must start with mongodb:// or mongodb+srv://.');
    // URI mode keeps every option as written. The password moves to the credentials; the engine puts it back
    // into `scheme://user@host` when connecting (as the app's MongoSession.connectionString does).
    const scheme = url.trim().slice(0, u.scheme.length);
    const uri = u.user === undefined ? url.trim() : `${scheme}://${u.user}@${u.afterUserinfo}`;
    if (password) credentials.password = password;
    const profile: ProfileJson = { name: KIND_LABEL.mongodb, kind, mode: 'uri', uri };
    if (u.user) profile.user = decode(u.user);
    const db = decode(u.path);
    if (db) profile.database = db;
    return { profile, credentials };
  }
  const q = new Map(u.query.map(([k, v]) => [k.toLowerCase(), v]));
  const defaultPort = Number(q.get('port')) || DEFAULT_PORT[kind];
  const hosts = parseHosts(u.hosts || q.get('host') || 'localhost', defaultPort);
  if (!hosts.length) hosts.push({ host: 'localhost', port: defaultPort });
  const profile: ProfileJson = { name: KIND_LABEL[kind], kind, mode: hosts.length > 1 ? 'multiHost' : 'single', hosts };
  const user = u.user !== undefined ? decode(u.user) : q.get('user');
  if (user) profile.user = user;
  const queryPassword = q.get('password');
  if (password !== undefined || queryPassword !== undefined) credentials.password = password ?? queryPassword;
  const database = decode(u.path.split('/')[0]!) || q.get('dbname') || q.get('database') || '';
  if (database) profile.database = database;
  const ssl: NonNullable<ProfileJson['ssl']> = {};
  const mode = q.get('sslmode') ?? q.get('ssl-mode') ?? q.get('ssl');
  if (mode !== undefined) ssl.mode = sslModeOf(mode);
  const ca = q.get('sslrootcert') ?? q.get('ssl-ca');
  const cert = q.get('sslcert') ?? q.get('ssl-cert');
  const key = q.get('sslkey') ?? q.get('ssl-key');
  if (ca) ssl.caCertificatePath = ca;
  if (cert) ssl.clientCertificatePath = cert;
  if (key) ssl.clientKeyPath = key;
  if (Object.keys(ssl).length) profile.ssl = ssl;
  const target = TARGET_SESSION[q.get('target_session_attrs') ?? ''];
  if (kind === 'postgres' && target) profile.targetSession = target;
  const timeout = Number(q.get('connect_timeout'));
  if (Number.isInteger(timeout) && timeout > 0) profile.connectTimeoutSeconds = Math.min(timeout, 300);
  return { profile, credentials };
}

function partsProfile(vars: Map<string, string>, kind: Kind): { profile: ProfileJson; credentials: Credentials } | undefined {
  const v = (k: string) => vars.get(k)?.trim() || undefined;
  const p = kind === 'postgres' ? 'PG' : 'MYSQL_';
  const host = v(`${p}HOST`);
  const database = v(`${p}DATABASE`);
  // The mysql image's MYSQL_ROOT_PASSWORD alone also describes a server (as root, see below).
  const rootPassword = kind === 'mysql' && !v('MYSQL_USER') ? vars.get('MYSQL_ROOT_PASSWORD') : undefined;
  if (!host && !database && rootPassword === undefined) return undefined;
  // libpq accepts a host list with one port for all or one port per host.
  const ports = (v(`${p}PORT`) ?? '').split(',');
  const list = (host ?? 'localhost').split(',').map((raw, i) => {
    const h = raw.trim();
    const port = (ports[i] ?? ports[0])!.trim();
    return port ? `${h.includes(':') ? `[${h}]` : h}:${port}` : h;
  });
  const hosts = parseHosts(list.join(','), DEFAULT_PORT[kind]);
  const profile: ProfileJson = { name: KIND_LABEL[kind], kind, mode: hosts.length > 1 ? 'multiHost' : 'single', hosts };
  const user = v(`${p}USER`) ?? (rootPassword !== undefined ? 'root' : undefined);
  if (user) profile.user = user;
  if (database) profile.database = database;
  const sslmode = kind === 'postgres' ? v('PGSSLMODE') : undefined;
  if (sslmode) profile.ssl = { mode: sslModeOf(sslmode) };
  const password = vars.get(`${p}PASSWORD`) ?? rootPassword;
  return { profile, credentials: password === undefined ? {} : { password } };
}

/**
 * The postgres image's `POSTGRES_USER` / `POSTGRES_PASSWORD` / `POSTGRES_DB` (+ `POSTGRES_HOST` / `POSTGRES_PORT`, else
 * localhost and 5432). Like the image, the user defaults to `postgres` and the database to the user.
 */
function dockerPostgresProfile(vars: Map<string, string>): { profile: ProfileJson; credentials: Credentials } | undefined {
  const v = (k: string) => vars.get(k)?.trim() || undefined;
  const password = vars.get('POSTGRES_PASSWORD');
  if (!v('POSTGRES_USER') && !v('POSTGRES_DB') && !v('POSTGRES_HOST') && password === undefined) return undefined;
  const port = v('POSTGRES_PORT');
  const host = v('POSTGRES_HOST') ?? 'localhost';
  const hosts = parseHosts(port ? `${host.includes(':') ? `[${host}]` : host}:${port}` : host, DEFAULT_PORT.postgres);
  const user = v('POSTGRES_USER') ?? 'postgres';
  const profile: ProfileJson = { name: KIND_LABEL.postgres, kind: 'postgres', mode: hosts.length > 1 ? 'multiHost' : 'single', hosts, user, database: v('POSTGRES_DB') ?? user };
  return { profile, credentials: password === undefined ? {} : { password } };
}

/**
 * Text typed or pasted into the first Add Connection picker, if it looks like `scheme://…`: what the picker shows for it
 * (never the password) and whether it parses. Without this, a pasted URL filters every item away and Enter does nothing.
 */
export function pastedUrl(value: string): { url: string; detail: string; ok: boolean } | undefined {
  const url = value.trim();
  if (!/^[a-z][a-z0-9+.-]*:\/\//i.test(url)) return undefined;
  try {
    const { profile } = profileFromUrl(url);
    const db = profile.mode !== 'uri' && profile.database ? ` · ${profile.database}` : ''; // a kept URI shows its own
    return { url, detail: `${KIND_LABEL[profile.kind]} · ${endpointSummary(profile)}${db}`, ok: true };
  } catch (e) {
    return { url, detail: e instanceof UrlError ? e.message : 'Not a connection URL.', ok: false };
  }
}

/** `goqulient.envConnections` as written by hand: entries without a variable name and fields of the wrong type are dropped. */
export function sanitizeMappings(raw: unknown): EnvMapping[] {
  if (!Array.isArray(raw)) return [];
  const str = (v: unknown) => (typeof v === 'string' && v.trim() ? v.trim() : undefined);
  return raw.flatMap((m: Record<string, unknown> | null) => {
    const url = str(m?.url);
    if (!m || !url) return [];
    const out: EnvMapping = { url };
    const name = str(m.name);
    const file = str(m.file);
    if (name) out.name = name;
    if (file) out.file = file;
    if (isKind(m.kind)) out.kind = m.kind;
    if (typeof m.readOnly === 'boolean') out.readOnly = m.readOnly;
    return [out];
  });
}

function sameFile(a: string, b: string): boolean {
  const norm = (s: string) => s.trim().replace(/\\/g, '/').replace(/^(\.\/)+/, '');
  return norm(a) === norm(b);
}

/**
 * The connections one env file describes: the well-known URL variables, `PG*` / `MYSQL_*` parts and the
 * `goqulient.envConnections` mappings (a mapping for a well-known variable replaces its default handling). Read-only unless a
 * mapping says otherwise (D8).
 */
/**
 * `named`: variables marked by a `# DATABASE_URL: Name` comment (`parseEnv`). They become connections whatever their
 * name, and the marker's name is used unless a `goqulient.envConnections` entry names the variable too.
 */
export function envConnections(
  vars: Map<string, string>,
  file: EnvFile,
  mappings: readonly EnvMapping[] = [],
  named: ReadonlyMap<string, string> = new Map(),
): EnvConnection[] {
  const out: EnvConnection[] = [];
  const mine = mappings.filter((m) => typeof m?.url === 'string' && m.url.trim() && (!m.file || sameFile(m.file, file.path)));
  const mapped = new Set(mine.map((m) => m.url!.trim()));
  const add = (variable: string, build: () => { profile: ProfileJson; credentials: Credentials } | undefined, m?: EnvMapping) => {
    const id = envId(file.key, variable);
    const name = m?.name?.trim() || named.get(variable) || `${variable} (${file.label})`;
    try {
      const built = build();
      if (!built) return;
      const readOnly = typeof m?.readOnly === 'boolean' ? m.readOnly : true;
      out.push({ id, name, variable, file: file.label, profile: { ...built.profile, id, name, readOnly }, credentials: built.credentials });
    } catch (e) {
      const reason = e instanceof UrlError ? e.message : 'Unreadable value.';
      out.push({ id, name, variable, file: file.label, error: `${variable} in ${file.label}: ${reason}` });
    }
  };
  for (const variable of URL_VARS) {
    const value = vars.get(variable)?.trim();
    if (!value || mapped.has(variable)) continue;
    // DATABASE_URL is generic (sqlite:, redis://…): skip what isn't a database GoQulient knows instead of flagging it,
    // unless a marker comment says it's meant as a connection.
    if (variable === 'DATABASE_URL' && !named.has(variable) && !kindOfScheme(splitUrl(value)?.scheme ?? '')) continue;
    add(variable, () => profileFromUrl(value));
  }
  add('PG*', () => partsProfile(vars, 'postgres'));
  add('POSTGRES_*', () => dockerPostgresProfile(vars));
  add('MYSQL_*', () => partsProfile(vars, 'mysql'));
  for (const m of mine) {
    const variable = m.url!.trim();
    const value = vars.get(variable)?.trim();
    const kind = isKind(m.kind) ? m.kind : undefined;
    if (value) add(variable, () => profileFromUrl(value, kind), m);
    else if (m.file) out.push({ id: envId(file.key, variable), name: m.name?.trim() || `${variable} (${file.label})`, variable, file: file.label, error: `${variable} isn’t set in ${file.label}.` });
  }
  for (const [variable, label] of named) {
    if (URL_VARS.includes(variable) || mapped.has(variable)) continue; // already listed above, with this name
    const value = vars.get(variable)?.trim();
    if (value) add(variable, () => profileFromUrl(value));
    else out.push({ id: envId(file.key, variable), name: label || `${variable} (${file.label})`, variable, file: file.label, error: `${variable} in ${file.label} is empty.` });
  }
  return out;
}
