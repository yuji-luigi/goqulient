import assert from 'node:assert/strict';
import { describe, test } from 'node:test';
import { parseEnv } from '../src/connections/envParse.ts';
import { envConnections, envId, profileFromUrl, sanitizeMappings, sslModeOf, type EnvFile } from '../src/connections/envProfiles.ts';
import { endpointSummary, needsPassword, secretFingerprint } from '../src/connections/profile.ts';
import { EngineClient } from '../src/engine/client.ts';

const FILE: EnvFile = { key: 'file:///ws|.env', label: '.env', path: '.env' };
const env = (text: string, mappings: unknown = [], file = FILE) => envConnections(parseEnv(text).vars, file, sanitizeMappings(mappings));

/** No password may appear anywhere in what gets kept, shown or validated. */
function assertNoSecret(value: unknown, ...secrets: string[]) {
  const json = JSON.stringify(value);
  for (const s of secrets) {
    assert.ok(!json.includes(s), `found a secret in ${json}`);
    assert.ok(!json.includes(encodeURIComponent(s)), `found an encoded secret in ${json}`);
  }
}

describe('profileFromUrl', () => {
  test('postgres and postgresql, with defaults, encoding and sslmode', () => {
    const { profile, credentials } = profileFromUrl('postgresql://app%40corp:p%2Fw%40rd@db.example.com:6543/my%20db?sslmode=verify-full&connect_timeout=5');
    assert.deepEqual(profile, {
      name: 'PostgreSQL',
      kind: 'postgres',
      mode: 'single',
      hosts: [{ host: 'db.example.com', port: 6543 }],
      user: 'app@corp',
      database: 'my db',
      ssl: { mode: 'verifyFull' },
      connectTimeoutSeconds: 5,
    });
    assert.deepEqual(credentials, { password: 'p/w@rd' });
    assertNoSecret(profile, 'p/w@rd');
    const bare = profileFromUrl('postgres://h');
    assert.deepEqual(bare.profile.hosts, [{ host: 'h', port: 5432 }]);
    assert.equal(bare.profile.user, undefined, 'the engine fills its default user');
    assert.deepEqual(bare.credentials, {});
  });

  test('sslmode values map to the profile modes; unknown is the strictest', () => {
    const cases: [string, string][] = [
      ['disable', 'disable'], ['allow', 'prefer'], ['prefer', 'prefer'], ['require', 'require'],
      ['verify-ca', 'verifyCA'], ['verify-full', 'verifyFull'], ['VERIFY_IDENTITY', 'verifyFull'], ['REQUIRED', 'require'],
      ['DISABLED', 'disable'], ['PREFERRED', 'prefer'], ['VERIFY_CA', 'verifyCA'], ['true', 'require'], ['false', 'disable'],
      ['bogus', 'verifyFull'],
    ];
    for (const [given, mode] of cases) assert.equal(sslModeOf(given), mode, given);
    assert.equal(profileFromUrl('postgres://u@h/d?sslmode=require&sslrootcert=/ca.pem').profile.ssl?.caCertificatePath, '/ca.pem');
    assert.equal(profileFromUrl('postgres://u@h/d').profile.ssl, undefined, 'no sslmode: the profile default (prefer)');
  });

  test('postgres multi-host, IPv6, target_session_attrs and query parameters', () => {
    const { profile, credentials } = profileFromUrl('postgres://u:pw@a:5432,b,[::1]:5434/d?target_session_attrs=read-write&port=6000');
    assert.equal(profile.mode, 'multiHost');
    assert.deepEqual(profile.hosts, [{ host: 'a', port: 5432 }, { host: 'b', port: 6000 }, { host: '::1', port: 5434 }]);
    assert.equal(profile.targetSession, 'readWrite');
    assert.equal(endpointSummary(profile), 'a:5432, b:6000, [::1]:5434');
    assert.deepEqual(credentials, { password: 'pw' });
    const q = profileFromUrl('postgresql:///?host=h&user=me&password=s3cret&dbname=x');
    assert.deepEqual([q.profile.hosts, q.profile.user, q.profile.database], [[{ host: 'h', port: 5432 }], 'me', 'x']);
    assert.deepEqual(q.credentials, { password: 's3cret' });
    assertNoSecret(q.profile, 's3cret');
  });

  test('an unencoded @ / ? # in the password still parses', () => {
    const { profile, credentials } = profileFromUrl('postgres://u:p@ss/w?r#d@h:1/db');
    assert.deepEqual([profile.hosts, profile.database, credentials.password], [[{ host: 'h', port: 1 }], 'db', 'p@ss/w?r#d']);
  });

  test('mysql, mariadb and driver suffixes', () => {
    for (const scheme of ['mysql', 'mariadb', 'mysql2', 'mysql+pymysql']) {
      const { profile, credentials } = profileFromUrl(`${scheme}://root:secret@127.0.0.1:3307/shop?ssl-mode=REQUIRED`);
      assert.deepEqual(profile, {
        name: 'MySQL / MariaDB',
        kind: 'mysql',
        mode: 'single',
        hosts: [{ host: '127.0.0.1', port: 3307 }],
        user: 'root',
        database: 'shop',
        ssl: { mode: 'require' },
      });
      assert.deepEqual(credentials, { password: 'secret' });
    }
    assert.equal(profileFromUrl('mysql://h').profile.hosts?.[0]?.port, 3306);
    assert.equal(profileFromUrl('postgresql+psycopg://u@h/d').profile.kind, 'postgres');
  });

  test('mongodb:// is URI mode with the password moved to the credentials', () => {
    const url = 'mongodb://admin:pa%3Ass@m1:27017,m2:27018/app?replicaSet=rs0&authSource=admin';
    const { profile, credentials } = profileFromUrl(url);
    assert.deepEqual(profile, {
      name: 'MongoDB',
      kind: 'mongodb',
      mode: 'uri',
      uri: 'mongodb://admin@m1:27017,m2:27018/app?replicaSet=rs0&authSource=admin',
      user: 'admin',
      database: 'app',
    });
    assert.deepEqual(credentials, { password: 'pa:ss' });
    assertNoSecret(profile, 'pa:ss', 'pa%3Ass');
    assert.ok(needsPassword(profile));
    const anon = profileFromUrl('mongodb://localhost/test');
    assert.equal(anon.profile.uri, 'mongodb://localhost/test');
    assert.deepEqual(anon.credentials, {});
    assert.equal(needsPassword(anon.profile), false);
  });

  test('mongodb+srv:// is URI mode too (keeps every option, which the engine accepts)', () => {
    const { profile, credentials } = profileFromUrl('mongodb+srv://u:s3cr3t@cluster0.abc.mongodb.net/db?retryWrites=true&w=majority');
    assert.equal(profile.mode, 'uri');
    assert.equal(profile.uri, 'mongodb+srv://u@cluster0.abc.mongodb.net/db?retryWrites=true&w=majority');
    assert.equal(credentials.password, 's3cr3t');
    assertNoSecret(profile, 's3cr3t');
  });

  test('errors never echo the URL', () => {
    const bad = [
      ['redis://u:hunter2@h', /Unsupported URL scheme “redis”/],
      ['not a url hunter2', /Not a connection URL/],
      ['postgres://u:hunter2@h:99999/d', /port must be a number/],
      ['postgres://u:hunter2@%2Ftmp/d', /Unix socket/],
    ] as const;
    for (const [url, message] of bad) {
      assert.throws(() => profileFromUrl(url), (e: Error) => message.test(e.message) && !e.message.includes('hunter2'));
    }
    assert.throws(() => profileFromUrl('mysql://h', 'postgres'), /for MySQL \/ MariaDB, not PostgreSQL/);
    assert.equal(profileFromUrl('tcp://h:1/d', 'postgres').profile.kind, 'postgres', 'kind decides when the scheme does not');
  });
});

describe('envConnections', () => {
  test('well-known URL variables: read-only, stable ids, named after the variable and file', () => {
    const list = env(
      'DATABASE_URL=postgres://u:pg-pass@h/app\nMONGODB_URI="mongodb://m:mongo-pass@h/db"\nMYSQL_URL=mysql://r:my-pass@h/s\n' +
        'POSTGRESQL_URL=postgresql://h\nPOSTGRES_URL=postgres://h\nMONGO_URL=mongodb://h\nMONGODB_URL=mongodb://h2',
    );
    assert.deepEqual(
      list.map((c) => c.name),
      ['DATABASE_URL (.env)', 'POSTGRES_URL (.env)', 'POSTGRESQL_URL (.env)', 'MYSQL_URL (.env)', 'MONGODB_URI (.env)', 'MONGO_URL (.env)', 'MONGODB_URL (.env)'],
    );
    for (const c of list) {
      assert.equal(c.profile?.readOnly, true, `${c.name} is read-only by default`);
      assert.equal(c.profile?.id, c.id);
      assert.equal(c.id, envId(FILE.key, c.variable));
      assert.match(c.id, /^env-[0-9a-f]{16}$/);
    }
    assertNoSecret(list.map((c) => c.profile), 'pg-pass', 'mongo-pass', 'my-pass');
    assert.deepEqual(list.map((c) => c.credentials?.password).slice(0, 1), ['pg-pass']);
    assert.equal(list.find((c) => c.variable === 'MONGODB_URI')?.credentials?.password, 'mongo-pass');
  });

  test('ids are stable for the same file and variable, and differ otherwise', () => {
    const a = env('DATABASE_URL=postgres://h')[0]!.id;
    assert.equal(env('DATABASE_URL=postgres://other')[0]!.id, a, 'the value does not change the id');
    assert.notEqual(env('DATABASE_URL=postgres://h', [], { ...FILE, key: 'file:///other|.env' })[0]!.id, a);
    assert.notEqual(env('POSTGRES_URL=postgres://h')[0]!.id, a);
  });

  test('DATABASE_URL with another scheme is skipped; a specific variable with a bad value is listed with the reason', () => {
    assert.deepEqual(env('DATABASE_URL=sqlite:///tmp/x.db\nREDIS_URL=redis://h'), []);
    const [bad] = env('MYSQL_URL=redis://u:hunter2@h');
    assert.equal(bad!.profile, undefined);
    assert.match(bad!.error!, /^MYSQL_URL in \.env: Unsupported URL scheme/);
    assert.ok(!bad!.error!.includes('hunter2'));
  });

  test('PG* and MYSQL_* parts', () => {
    const [pg, my] = env(
      'PGHOST=db1,db2\nPGPORT=5433\nPGUSER=me\nPGPASSWORD=pg-secret\nPGDATABASE=app\nPGSSLMODE=verify-ca\n' +
        'MYSQL_HOST=localhost\nMYSQL_PORT=3307\nMYSQL_USER=root\nMYSQL_PASSWORD=my-secret\nMYSQL_DATABASE=shop',
    );
    assert.deepEqual(pg!.profile, {
      name: 'PG* (.env)',
      kind: 'postgres',
      mode: 'multiHost',
      hosts: [{ host: 'db1', port: 5433 }, { host: 'db2', port: 5433 }],
      user: 'me',
      database: 'app',
      ssl: { mode: 'verifyCA' },
      id: envId(FILE.key, 'PG*'),
      readOnly: true,
    });
    assert.deepEqual(pg!.credentials, { password: 'pg-secret' });
    assert.deepEqual([my!.name, my!.profile?.hosts, my!.profile?.user, my!.profile?.database], ['MYSQL_* (.env)', [{ host: 'localhost', port: 3307 }], 'root', 'shop']);
    assert.deepEqual(my!.credentials, { password: 'my-secret' });
    assertNoSecret([pg!.profile, my!.profile], 'pg-secret', 'my-secret');
    assert.deepEqual(env('PGUSER=me\nPGPASSWORD=x'), [], 'parts need a host or a database');
    assert.deepEqual(env('PGDATABASE=d')[0]!.profile?.hosts, [{ host: 'localhost', port: 5432 }]);
    assert.deepEqual(env('PGHOST=a,b\nPGPORT=1,2')[0]!.profile?.hosts, [{ host: 'a', port: 1 }, { host: 'b', port: 2 }]);
  });

  test('goku.envConnections: custom variables, overrides, file filter, kind', () => {
    const text = 'ANALYTICS=postgres://u:an-pass@a/stats\nDATABASE_URL=mysql://h/x\nLEGACY=tcp://h:5000/old';
    const list = env(text, [
      { url: 'ANALYTICS', name: 'Analytics', readOnly: false },
      { url: 'DATABASE_URL', name: 'Main' },
      { url: 'LEGACY', kind: 'postgres' },
      { url: 'ELSEWHERE', file: '.env.local' },
      { url: 'MISSING', file: './.env' },
      { url: 'MISSING_ANYWHERE' },
      { name: 'no variable' },
      'garbage',
      null,
    ]);
    assert.deepEqual(
      list.map((c) => [c.name, c.profile?.readOnly, c.profile?.kind ?? c.error]),
      [
        ['Analytics', false, 'postgres'],
        ['Main', true, 'mysql'],
        ['LEGACY (.env)', true, 'postgres'],
        ['MISSING (.env)', undefined, 'MISSING isn’t set in .env.'],
      ],
    );
    assert.equal(list.filter((c) => c.variable === 'DATABASE_URL').length, 1, 'a mapping replaces the default handling');
    assert.equal(list[1]!.id, envId(FILE.key, 'DATABASE_URL'), 'same id as without the mapping');
    assertNoSecret(list.map((c) => c.profile), 'an-pass');
  });
});

describe('secrets', () => {
  test('a saved password is bound to kind, servers and user, not to the database or name', () => {
    const base = profileFromUrl('postgres://u@h:5432/a').profile;
    assert.equal(secretFingerprint(base), secretFingerprint({ ...base, database: 'b', name: 'other' }));
    assert.notEqual(secretFingerprint(base), secretFingerprint({ ...base, hosts: [{ host: 'evil', port: 5432 }] }));
    assert.notEqual(secretFingerprint(base), secretFingerprint({ ...base, user: 'admin' }));
    const m = profileFromUrl('mongodb://u:pw@h/a').profile;
    assert.equal(secretFingerprint(m), secretFingerprint({ ...m, uri: 'mongodb://u@h/b?x=1' }));
    assert.notEqual(secretFingerprint(m), secretFingerprint({ ...m, uri: 'mongodb://u@evil/a' }));
  });
});

const bin = process.env.GOKU_ENGINE_BIN;

describe('the engine accepts what env discovery builds', { skip: bin ? false : 'GOKU_ENGINE_BIN not set' }, () => {
  test('profile/validate', async () => {
    const c = new EngineClient({ binary: () => bin!, clientName: 'node-test', log: { info() {}, error() {} } });
    try {
      const list = env(
        'DATABASE_URL=postgres://u:pw@h:5433/app?sslmode=verify-ca\nMYSQL_URL=mariadb://r:pw@h/s\nMONGODB_URI=mongodb://u:pw@a,b:27018/db?replicaSet=rs\n' +
          'MONGO_URL=mongodb+srv://u:pw@cluster.example.net/db\nPGHOST=x,y\nPGPORT=1,2\nPGDATABASE=d',
      );
      for (const conn of list) {
        const r = await c.request<{ profile: Record<string, unknown>; endpoint: string }>('profile/validate', { profile: conn.profile });
        assert.equal(r.profile.id, conn.id, conn.name);
        assert.equal(r.profile.readOnly, true, conn.name);
        assert.ok(!r.endpoint.includes('pw'), r.endpoint);
      }
    } finally {
      await c.stop();
    }
  });
});
