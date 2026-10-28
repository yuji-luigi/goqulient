import assert from 'node:assert/strict';
import { test } from 'node:test';
import { envConnections, type EnvFile } from '../src/connections/envProfiles.ts';
import { decodeSettingsConnections, mergeConnections } from '../src/connections/model.ts';

const FILE: EnvFile = { key: 'file:///ws|.env', label: '.env', path: '.env' };
const ENV = envConnections(new Map([['DATABASE_URL', 'postgres://u:pw@prod/app'], ['MYSQL_URL', 'nope://x']]), FILE);

test('settings entries keep their fields; ids come from the profile or are derived stably', () => {
  const raw = [
    { id: 'ABC', name: 'pg', kind: 'postgres', hosts: [{ host: 'db', port: 6543 }], readOnly: true, futureField: 1 },
    { name: 'mongo', kind: 'mongodb', mode: 'uri', uri: 'mongodb://u:inline@h/db' },
  ];
  const [pg, mongo] = decodeSettingsConnections(raw, 'user');
  assert.deepEqual([pg!.id, pg!.source, pg!.endpoint, pg!.readOnly, pg!.profile?.futureField], ['ABC', 'user', 'db:6543', true, 1]);
  assert.equal(mongo!.readOnly, false);
  assert.equal(mongo!.endpoint, 'mongodb://u:****@h/db', 'the endpoint shown is redacted');
  assert.match(mongo!.id, /^user-[0-9a-f]{16}$/);
  assert.equal(decodeSettingsConnections(raw, 'user')[1]!.id, mongo!.id, 'stable across reads');
  assert.notEqual(decodeSettingsConnections(raw, 'workspace')[1]!.id, mongo!.id, 'scoped');
});

test('entries that can’t be read stay in the list with the reason', () => {
  const list = decodeSettingsConnections(
    [{ name: 'ok', kind: 'mysql' }, 'text', { kind: 'postgres' }, { name: 'n' }, { name: 'c', kind: 'cassandra' }, null, []],
    'workspace',
  );
  assert.deepEqual(
    list.map((c) => [c.name, c.invalid]),
    [
      ['ok', undefined],
      ['Connection 2 (workspace settings)', 'Not an object.'],
      ['Connection 3 (workspace settings)', '“name” is missing.'],
      ['n', '“kind” is missing.'],
      ['c', 'Unknown database kind “cassandra”.'],
      ['Connection 6 (workspace settings)', 'Not an object.'],
      ['Connection 7 (workspace settings)', 'Not an object.'],
    ],
  );
  assert.ok(list.slice(1).every((c) => c.profile === undefined));
  assert.equal(new Set(list.map((c) => c.id)).size, list.length, 'invalid entries get distinct ids');
  assert.deepEqual(decodeSettingsConnections({ name: 'x' }, 'user').map((c) => c.invalid), ['goqulient.connections must be a list.']);
  assert.deepEqual(decodeSettingsConnections(undefined, 'user'), []);
});

test('merge: user, then workspace, then env; env read-only and invalid env entries kept', () => {
  const list = mergeConnections({ user: [{ name: 'u', kind: 'postgres' }], workspace: [{ name: 'w', kind: 'mysql' }], env: ENV, trusted: true });
  assert.deepEqual(
    list.map((c) => [c.name, c.source, c.readOnly, !!c.invalid]),
    [
      ['u', 'user', false, false],
      ['w', 'workspace', false, false],
      ['DATABASE_URL (.env)', 'env', true, false],
      ['MYSQL_URL (.env)', 'env', true, true],
    ],
  );
  const env = list[2]!;
  assert.deepEqual([env.file, env.variable, env.endpoint], ['.env', 'DATABASE_URL', 'prod:5432']);
  assert.ok(!JSON.stringify(list).includes('pw'), 'no password in the merged list');
});

test('untrusted workspace: only user settings count', () => {
  const list = mergeConnections({ user: [{ name: 'u', kind: 'postgres' }], workspace: [{ name: 'w', kind: 'mysql' }], env: ENV, trusted: false });
  assert.deepEqual(list.map((c) => c.name), ['u']);
});

test('a repeated id keeps the first entry and marks the others', () => {
  const list = mergeConnections({
    user: [{ id: 'X', name: 'mine', kind: 'postgres' }],
    workspace: [{ id: 'X', name: 'theirs', kind: 'postgres', hosts: [{ host: 'evil', port: 5432 }] }],
    env: [],
    trusted: true,
  });
  assert.equal(list[0]!.id, 'X');
  assert.equal(list[0]!.invalid, undefined);
  assert.notEqual(list[1]!.id, 'X', 'the duplicate never reads X’s saved password');
  assert.equal(list[1]!.profile, undefined);
  assert.match(list[1]!.invalid!, /Same id “X” as “mine”/);
});
