// SSH tunnel helpers that don't need VS Code (Node tests import them): which SSH secret a profile needs, what a saved
// one belongs to, the engine's `unknownHostKey` detail, the trust prompt and the settings rewrite that pins a host key.
import type { ProfileJson } from './profile.ts';

/** The profile's `ssh` object (the engine's `SshSettings`), with the engine's defaults for missing fields. */
export interface SshJson {
  enabled?: boolean;
  host?: string;
  port?: number;
  user?: string;
  auth?: 'password' | 'privateKey';
  privateKeyPath?: string;
  trustedHostKey?: string;
  [key: string]: unknown;
}

/** The SSH secrets `connection/open` may carry next to the database password, as named in `Credentials`. */
export type SshSecret = 'sshPassword' | 'sshKeyPassphrase';

export const SSH_SECRET_LABEL: Record<SshSecret, string> = { sshPassword: 'SSH password', sshKeyPassphrase: 'SSH key passphrase' };

function ssh(p: ProfileJson): SshJson | undefined {
  const s = p.ssh;
  return s && typeof s === 'object' && !Array.isArray(s) && (s as SshJson).enabled === true ? (s as SshJson) : undefined;
}

export function usesSsh(p: ProfileJson): boolean {
  return ssh(p) !== undefined;
}

/** Password auth always needs the SSH password; an unknown `auth` falls back to password, as in the engine. */
export function needsSshPassword(p: ProfileJson): boolean {
  const s = ssh(p);
  return !!s && s.auth !== 'privateKey';
}

/** `user@host:port` of the bastion, never a secret. */
export function sshEndpoint(p: ProfileJson): string {
  const s = ssh(p);
  if (!s) return '';
  return `${s.user ?? ''}@${s.host ?? ''}:${typeof s.port === 'number' ? s.port : 22}`;
}

/**
 * What a saved SSH secret belongs to (like `secretFingerprint` for the database password): the bastion and user for a
 * password, the key file for a passphrase. A changed bastion or key never receives a saved secret.
 */
export function sshSecretFingerprint(p: ProfileJson, secret: SshSecret): string {
  const s = ssh(p) ?? {};
  if (secret === 'sshKeyPassphrase') return JSON.stringify(['key', String(s.privateKeyPath ?? '').trim() || '~/.ssh/id_ed25519']);
  return JSON.stringify(['ssh', String(s.host ?? '').toLowerCase(), typeof s.port === 'number' ? s.port : 22, s.user ?? '']);
}

/** The engine's `unknownHostKey` detail (docs/ARCHITECTURE.md, Protocol). */
export interface HostKeyInfo {
  host: string;
  port: number;
  keyType: string;
  hostKey: string;
  fingerprint: string;
}

export function hostKeyInfo(detail: unknown): HostKeyInfo | undefined {
  if (!detail || typeof detail !== 'object') return undefined;
  const d = detail as Record<string, unknown>;
  const ok = ['host', 'keyType', 'hostKey', 'fingerprint'].every((k) => typeof d[k] === 'string' && d[k]) && typeof d.port === 'number';
  return ok ? (d as unknown as HostKeyInfo) : undefined;
}

/** The modal's text: the bastion, the key type and its fingerprint, as `ssh` asks on first contact. */
export function trustPrompt(connection: string, info: HostKeyInfo): { message: string; detail: string } {
  return {
    message: `Trust the SSH host ${info.host}:${info.port}?`,
    detail:
      `“${connection}” connects through this SSH server, which GoQulient hasn’t seen before.\n\n` +
      `${info.keyType} key fingerprint:\n${info.fingerprint}\n\n` +
      'Compare it with the fingerprint your server administrator gives you (ssh-keygen -lf on the server’s host key). ' +
      'If you trust it, it is saved as ssh.trustedHostKey in this connection’s settings, and a different key later stops the connection.',
  };
}

/** The connection's profile with `hostKey` pinned, key order kept (the tree compares profiles as JSON). */
export function withHostKey(profile: ProfileJson, hostKey: string): ProfileJson {
  return { ...profile, ssh: { ...(profile.ssh as SshJson), trustedHostKey: hostKey } };
}

/**
 * `goqulient.connections` of one scope with entry `index` pinned to `hostKey`. Every other entry stays exactly as it was.
 * Undefined when the entry moved or changed meanwhile (`expect` = its name and kind), so nothing else gets the key.
 */
export function pinHostKey(entries: unknown, index: number, expect: { name: string; kind: string }, hostKey: string): unknown[] | undefined {
  if (!Array.isArray(entries)) return undefined;
  const entry: unknown = entries[index];
  if (!entry || typeof entry !== 'object' || Array.isArray(entry)) return undefined;
  const e = entry as ProfileJson;
  if (e.name !== expect.name || e.kind !== expect.kind || !usesSsh(e)) return undefined;
  return entries.map((x, i) => (i === index ? withHostKey(e, hostKey) : x));
}
