// Mirrors `uri_password_is_removed_up_to_the_last_at` in engine/crates/goku-engine/src/redact.rs.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { redactUri } from '../src/connections/redact.ts';

test('uri password is removed up to the last @', () => {
  assert.equal(redactUri('mongodb://u:p@ss@h:27017/db'), 'mongodb://u:****@h:27017/db');
  assert.equal(redactUri('postgres://u@h/db'), 'postgres://u@h/db');
  assert.equal(redactUri('mysql://h/db?user=u&password=s3cret&ssl=1'), 'mysql://h/db?user=u&password=****&ssl=1');
  assert.equal(redactUri('x?PWD=abc'), 'x?PWD=****');
});

test('every occurrence, and the URI part of an error message', () => {
  assert.equal(redactUri('failed for postgres://a:b@h and mysql://c:d@k'), 'failed for postgres://a:****@h and mysql://c:****@k');
  assert.equal(redactUri('mongodb://h/?tlsCertificateKeyFilePassword=x&w=1'), 'mongodb://h/?tlsCertificateKeyFilePassword=****&w=1');
});
