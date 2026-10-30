// The JSON shape of `ConnectionProfile` (engine/crates/goku-engine/src/profile.rs, same as the Swift app) and the few
// helpers the extension needs without the engine. The engine stays the authority: it decodes and validates.
import { redactUri } from './redact.ts';

export const KINDS = ['postgres', 'mysql', 'mongodb'] as const;
export type Kind = (typeof KINDS)[number];
export type SslMode = 'disable' | 'prefer' | 'require' | 'verifyCA' | 'verifyFull';

export const KIND_LABEL: Record<Kind, string> = { postgres: 'PostgreSQL', mysql: 'MySQL / MariaDB', mongodb: 'MongoDB' };
export const DEFAULT_PORT: Record<Kind, number> = { postgres: 5432, mysql: 3306, mongodb: 27017 };
export const DEFAULT_USER: Record<Kind, string> = { postgres: 'postgres', mysql: 'root', mongodb: '' };

export interface HostPort {
  host: string;
  port: number;
}

/** Only the fields the extension reads or writes; the rest passes through untouched. Never holds a password. */
export interface ProfileJson {
  id?: string;
  name: string;
  kind: Kind;
  mode?: 'single' | 'multiHost' | 'srv' | 'uri';
  hosts?: HostPort[];
  user?: string;
  database?: string;
  ssl?: { mode?: SslMode; caCertificatePath?: string; clientCertificatePath?: string; clientKeyPath?: string };
  /** SSH tunnel (`ssh.ts`); its secrets live in SecretStorage like the password. */
  ssh?: unknown;
  targetSession?: string;
  srvHost?: string;
  uri?: string;
  connectTimeoutSeconds?: number;
  readOnly?: boolean;
  [key: string]: unknown;
}

/** What `connection/open` takes next to the profile (the engine's `Credentials`). Only ever sent over stdin. */
export interface Credentials {
  password?: string;
  sshPassword?: string;
  sshKeyPassphrase?: string;
}

export function isKind(v: unknown): v is Kind {
  return typeof v === 'string' && (KINDS as readonly string[]).includes(v);
}

function hostsOf(p: ProfileJson): HostPort[] {
  return Array.isArray(p.hosts) && p.hosts.length ? p.hosts : [{ host: 'localhost', port: DEFAULT_PORT[p.kind] }];
}

function hostPort(h: HostPort): string {
  return String(h.host).includes(':') && !String(h.host).startsWith('[') ? `[${h.host}]:${h.port}` : `${h.host}:${h.port}`;
}

/** Same text as the engine's `endpoint_summary`, never a secret. */
export function endpointSummary(p: ProfileJson): string {
  if (p.mode === 'srv') return `srv://${p.srvHost ?? ''}`;
  if (p.mode === 'uri') return redactUri(p.uri ?? '');
  return hostsOf(p).map(hostPort).join(', ');
}

/** The pieces of `scheme://user:password@hosts/path?query`, still percent-encoded (`query` is decoded). */
export interface UrlParts {
  scheme: string;
  user?: string;
  password?: string;
  hosts: string;
  path: string;
  query: [string, string][];
  /** Everything after the userinfo, as written. */
  afterUserinfo: string;
}

export function splitUrl(url: string): UrlParts | undefined {
  const m = /^([A-Za-z][A-Za-z0-9+.-]*):\/\/([\s\S]*)$/.exec(url.trim());
  if (!m) return undefined;
  let rest = m[2]!;
  let userinfo: string | undefined;
  // Up to the last `@` like the redaction rule, so an unencoded `@` or `/` in a password still parses; a `user:` prefix
  // or a userinfo without `/?#` tells it from an `@` that only appears later in the path or query.
  const at = rest.lastIndexOf('@');
  if (at >= 0) {
    const candidate = rest.slice(0, at);
    if (/^[^:/?#@]+:/.test(candidate) || !/[/?#]/.test(candidate)) {
      userinfo = candidate;
      rest = rest.slice(at + 1);
    }
  }
  const afterUserinfo = rest;
  const hash = rest.indexOf('#');
  if (hash >= 0) rest = rest.slice(0, hash);
  const q = rest.indexOf('?');
  const query = q >= 0 ? rest.slice(q + 1) : '';
  if (q >= 0) rest = rest.slice(0, q);
  const slash = rest.indexOf('/');
  const colon = userinfo?.indexOf(':') ?? -1;
  return {
    scheme: m[1]!.toLowerCase(),
    user: userinfo === undefined ? undefined : colon >= 0 ? userinfo.slice(0, colon) : userinfo,
    password: colon >= 0 ? userinfo!.slice(colon + 1) : undefined,
    hosts: slash >= 0 ? rest.slice(0, slash) : rest,
    path: slash >= 0 ? rest.slice(slash + 1) : '',
    query: query
      .split('&')
      .filter(Boolean)
      .map((kv) => {
        const eq = kv.indexOf('=');
        return [decode(eq >= 0 ? kv.slice(0, eq) : kv), decode(eq >= 0 ? kv.slice(eq + 1) : '')];
      }),
    afterUserinfo,
  };
}

export function decode(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

/** True when connecting will need a password: SQL engines always, MongoDB only with a user. */
export function needsPassword(p: ProfileJson): boolean {
  if (p.kind !== 'mongodb') return true;
  if (p.mode === 'uri') return !!splitUrl(p.uri ?? '')?.user;
  return !!p.user;
}

/**
 * What a saved password belongs to: kind, servers and user. A saved password is only handed out when this still
 * matches, so a settings entry that reuses another one's `id` (e.g. from a cloned repo's workspace settings) can't send
 * that password to a different server.
 */
export function secretFingerprint(p: ProfileJson): string {
  let where: string;
  let user = p.user ?? DEFAULT_USER[p.kind];
  if (p.mode === 'uri') {
    const u = splitUrl(p.uri ?? '');
    where = u ? `${u.scheme}://${u.hosts}` : '';
    user = decode(u?.user ?? '');
  } else if (p.mode === 'srv') where = `srv://${p.srvHost ?? ''}`;
  else where = hostsOf(p).map(hostPort).join(',');
  return JSON.stringify([p.kind, where.toLowerCase(), user]);
}
