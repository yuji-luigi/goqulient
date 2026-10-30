// The connection list the tree shows, as a pure function of settings, env discovery and Workspace Trust (store.ts feeds
// it from VS Code). Decoding is as tolerant as the engine's (AGENTS.md, "Profiles"): an entry that can't be read
// stays in the list as invalid, with the reason, and is never dropped or rewritten.
import { createHash } from 'node:crypto';
import type { EnvConnection } from './envProfiles.ts';
import { endpointSummary, isKind, type Kind, type ProfileJson } from './profile.ts';

export type Source = 'user' | 'workspace' | 'env';

export interface Connection {
  /** Stable across restarts: the profile's `id`, else derived from scope + name (settings) or file + variable (env). */
  id: string;
  name: string;
  source: Source;
  kind?: Kind;
  /** Missing when `invalid` is set. Never holds a password. */
  profile?: ProfileJson;
  /** Why this entry can't be used (safe to show). */
  invalid?: string;
  endpoint: string;
  readOnly: boolean;
  /** Env connections: the file label and variable they came from. */
  file?: string;
  variable?: string;
  /** Settings connections: the entry's position in that scope's `goqulient.connections` (to write it back). */
  index?: number;
}

function hashId(prefix: string, ...parts: string[]): string {
  return `${prefix}-${createHash('sha256').update(parts.join('\n')).digest('hex').slice(0, 16)}`;
}

/** Decodes `goqulient.connections` from one scope. Only `name` and `kind` are checked here; the engine checks the rest. */
export function decodeSettingsConnections(raw: unknown, source: 'user' | 'workspace'): Connection[] {
  if (raw === undefined || raw === null) return [];
  const where = source === 'user' ? 'user settings' : 'workspace settings';
  if (!Array.isArray(raw)) {
    return [{ id: `${source}-not-a-list`, name: `goqulient.connections (${where})`, source, endpoint: '', readOnly: true, invalid: 'goqulient.connections must be a list.' }];
  }
  return raw.map((entry: unknown, i): Connection => {
    const fallbackName = `Connection ${i + 1} (${where})`;
    const invalid = (reason: string, name = fallbackName): Connection => ({
      id: hashId(source, 'invalid', String(i), name),
      name,
      source,
      endpoint: '',
      readOnly: true,
      invalid: reason,
    });
    if (typeof entry !== 'object' || entry === null || Array.isArray(entry)) return invalid('Not an object.');
    const e = entry as Record<string, unknown>;
    if (typeof e.name !== 'string' || !e.name.trim()) return invalid('“name” is missing.');
    if (e.kind === undefined || e.kind === null) return invalid('“kind” is missing.', e.name);
    if (!isKind(e.kind)) return invalid(`Unknown database kind “${String(e.kind)}”.`, e.name);
    const profile = { ...e, name: e.name, kind: e.kind } as ProfileJson;
    const id = typeof e.id === 'string' && e.id.trim() ? e.id.trim() : hashId(source, e.name, e.kind);
    return { id, name: e.name, source, kind: e.kind, profile, endpoint: endpointSummary(profile), readOnly: profile.readOnly === true, index: i };
  });
}

export function fromEnv(c: EnvConnection): Connection {
  const base = { id: c.id, name: c.name, source: 'env' as const, file: c.file, variable: c.variable, readOnly: true };
  if (!c.profile) return { ...base, endpoint: '', invalid: c.error ?? 'Unreadable.' };
  return { ...base, kind: c.profile.kind, profile: c.profile, endpoint: endpointSummary(c.profile), readOnly: c.profile.readOnly !== false };
}

export interface MergeInput {
  /** `goqulient.connections` as `inspect()` reports it per scope. */
  user: unknown;
  workspace: unknown;
  env: readonly EnvConnection[];
  /** In an untrusted workspace only the user's own settings count (D8). */
  trusted: boolean;
}

/** User settings, then workspace settings, then `.env`. A repeated id keeps the first entry and marks the others. */
export function mergeConnections(input: MergeInput): Connection[] {
  const all = [
    ...decodeSettingsConnections(input.user, 'user'),
    ...(input.trusted ? decodeSettingsConnections(input.workspace, 'workspace') : []),
    ...(input.trusted ? input.env.map(fromEnv) : []),
  ];
  const seen = new Map<string, string>();
  return all.map((c) => {
    const first = seen.get(c.id);
    if (first === undefined) {
      seen.set(c.id, c.name);
      return c;
    }
    return { ...c, profile: undefined, id: hashId('dup', c.source, c.id, c.name), invalid: `Same id “${c.id}” as “${first}”; give it its own id.` };
  });
}
