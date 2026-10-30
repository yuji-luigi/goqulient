import assert from 'node:assert/strict';
import { test } from 'node:test';
import { decodeSettingsConnections } from '../src/connections/model.ts';
import type { ProfileJson } from '../src/connections/profile.ts';
import { hostKeyInfo, needsSshPassword, pinHostKey, sshEndpoint, sshSecretFingerprint, trustPrompt, usesSsh, withHostKey } from '../src/connections/ssh.ts';

const KEY = 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPyoYifZI50KA7L3DSP9dH30pfHeBa0Jtd3jSiCO6cAK';
const tunnelled = (ssh: Record<string, unknown>): ProfileJson => ({ name: 'pg', kind: 'postgres', hosts: [{ host: 'db', port: 5432 }], ssh: { enabled: true, host: 'bastion', user: 'me', ...ssh } });

test('which SSH secret a profile needs', () => {
  assert.equal(needsSshPassword(tunnelled({})), true, 'password is the default auth');
  assert.equal(needsSshPassword(tunnelled({ auth: 'kerberos' })), true, 'unknown auth falls back to password, like the engine');
  assert.equal(needsSshPassword(tunnelled({ auth: 'privateKey' })), false, 'a key asks for its passphrase only when the engine says so');
  assert.equal(needsSshPassword(tunnelled({ enabled: false })), false);
  assert.equal(usesSsh({ name: 'x', kind: 'mysql', ssh: 'yes' }), false);
  assert.equal(sshEndpoint(tunnelled({ port: 2222 })), 'me@bastion:2222');
  assert.equal(sshEndpoint(tunnelled({})), 'me@bastion:22');
});

test('a saved SSH secret belongs to the bastion and user, a passphrase to the key file', () => {
  const p = tunnelled({});
  assert.equal(sshSecretFingerprint(p, 'sshPassword'), sshSecretFingerprint(tunnelled({ host: 'BASTION', port: 22 }), 'sshPassword'));
  assert.notEqual(sshSecretFingerprint(p, 'sshPassword'), sshSecretFingerprint(tunnelled({ host: 'other' }), 'sshPassword'));
  assert.notEqual(sshSecretFingerprint(p, 'sshPassword'), sshSecretFingerprint(tunnelled({ user: 'root' }), 'sshPassword'));
  const key = (path: string) => sshSecretFingerprint(tunnelled({ auth: 'privateKey', privateKeyPath: path }), 'sshKeyPassphrase');
  assert.equal(key(''), key('~/.ssh/id_ed25519'), 'empty = the default key, as in the engine');
  assert.notEqual(key('~/.ssh/id_rsa'), key('~/.ssh/id_ed25519'));
});

test('the unknownHostKey detail is checked before it is shown', () => {
  const detail = { host: 'bastion', port: 22, keyType: 'ssh-ed25519', hostKey: KEY, fingerprint: 'SHA256:abc' };
  assert.deepEqual(hostKeyInfo(detail), detail);
  assert.equal(hostKeyInfo('text'), undefined);
  assert.equal(hostKeyInfo({ ...detail, port: '22' }), undefined);
  assert.equal(hostKeyInfo({ ...detail, hostKey: '' }), undefined);
  const { message, detail: text } = trustPrompt('Prod', detail);
  assert.equal(message, 'Trust the SSH host bastion:22?');
  assert.match(text, /“Prod”/);
  assert.match(text, /ssh-ed25519 key fingerprint:\nSHA256:abc/);
});

test('trusting a host key rewrites only that settings entry', () => {
  const other = { name: 'other', kind: 'mysql', futureField: [1] };
  const mine = { id: 'X', name: 'pg', kind: 'postgres', ssh: { enabled: true, host: 'bastion', user: 'me', port: 22 }, readOnly: true };
  const entries = [other, mine, 'broken'];
  const next = pinHostKey(entries, 1, { name: 'pg', kind: 'postgres' }, KEY)!;
  assert.deepEqual(next[0], other);
  assert.equal(next[0], other, 'other entries are the same objects');
  assert.equal(next[2], 'broken', 'unreadable entries stay as they are');
  assert.deepEqual(next[1], { ...mine, ssh: { ...mine.ssh, trustedHostKey: KEY } });
  assert.equal(entries[1], mine);
  assert.equal((mine.ssh as Record<string, unknown>).trustedHostKey, undefined, 'the input is not modified');
  assert.equal(pinHostKey(entries, 0, { name: 'pg', kind: 'postgres' }, KEY), undefined, 'the entry moved');
  assert.equal(pinHostKey(entries, 5, { name: 'pg', kind: 'postgres' }, KEY), undefined);
  assert.equal(pinHostKey({}, 0, { name: 'pg', kind: 'postgres' }, KEY), undefined);
  assert.equal(pinHostKey([{ name: 'pg', kind: 'postgres' }], 0, { name: 'pg', kind: 'postgres' }, KEY), undefined, 'no SSH');
});

test('the retried profile is the one the settings will hold, so the session survives the refresh', () => {
  const raw = [{ name: 'pg', kind: 'postgres', ssh: { enabled: true, host: 'b', user: 'u', trustedHostKey: '' }, user: 'dbc' }];
  const [before] = decodeSettingsConnections(raw, 'user');
  assert.equal(before!.index, 0);
  const [after] = decodeSettingsConnections(pinHostKey(raw, 0, { name: 'pg', kind: 'postgres' }, KEY), 'user');
  assert.equal(JSON.stringify(after!.profile), JSON.stringify(withHostKey(before!.profile!, KEY)));
  assert.equal(after!.id, before!.id, 'pinning keeps the connection id (and its saved secrets)');
});
