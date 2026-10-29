import assert from 'node:assert/strict';
import { test } from 'node:test';
import { parseEnv } from '../src/connections/envParse.ts';

const vars = (text: string) => Object.fromEntries(parseEnv(text).vars);

test('plain values, export, whitespace, comments and blank lines', () => {
  assert.deepEqual(
    vars('﻿# comment\n\nA=1\nexport B = two words  \n  C=\r\nexport\tD=x\n   # indented comment\n'),
    { A: '1', B: 'two words', C: '', D: 'x' },
  );
});

test('inline comments need whitespace before # on unquoted values', () => {
  assert.deepEqual(vars('A=value # note\nB=pa#ss\nC=#not-a-comment\nD= # x'), { A: 'value', B: 'pa#ss', C: '#not-a-comment', D: '' });
});

test('single quotes and backticks are literal; double quotes take escapes', () => {
  assert.deepEqual(vars(String.raw`A='a\nb # not a comment'` + '\n' + 'B=`x"y`' + '\n' + String.raw`C="l1\nl2\t\"q\" \\ \x"`), {
    A: String.raw`a\nb # not a comment`,
    B: 'x"y',
    C: 'l1\nl2\t"q" \\ \\x',
  });
  assert.deepEqual(vars(`A="v" # c\nB='w'   `), { A: 'v', B: 'w' });
});

test('quoted values can span lines', () => {
  assert.deepEqual(vars('KEY="-----BEGIN\nabc\n-----END"\nNEXT=1'), { KEY: '-----BEGIN\nabc\n-----END', NEXT: '1' });
  assert.deepEqual(vars("S='a\r\nb'"), { S: 'a\nb' });
});

test('no variable expansion', () => {
  assert.deepEqual(vars('A=1\nB=$A\nC="${A}x"'), { A: '1', B: '$A', C: '${A}x' });
});

test('last duplicate wins', () => {
  assert.deepEqual(vars('A=1\nA=2'), { A: '2' });
});

test('malformed lines are reported by line number, without values, and skipped', () => {
  const r = parseEnv('GOOD=1\njust text\n1BAD=x\nOPEN="secret\nAFTER=2');
  assert.deepEqual(Object.fromEntries(r.vars), { GOOD: '1', AFTER: '2' }, 'an unterminated quote does not swallow the file');
  assert.deepEqual(
    r.errors.map((e) => e.line),
    [2, 3, 4],
  );
  assert.ok(r.errors.every((e) => !e.message.includes('secret')));
  const trail = parseEnv('TRAIL="a" b\nLAST=3');
  assert.deepEqual(Object.fromEntries(trail.vars), { LAST: '3' });
  assert.deepEqual(trail.errors, [{ line: 1, message: 'Unexpected text after the closing quote.' }]);
});

test('a "# DATABASE_URL: Name" comment marks the next assignment, whatever the variable', () => {
  const r = parseEnv(
    "#DATABASE_URL:'Interaction test'\nDB_URI=\"mongodb://h/db\"\n" +
      '# database_url: "Reports"\n\n# DB_OLD=mongodb://old\nexport REPORTS_DB=postgres://h/r\n' +
      '# DATABASE_URL:\nPLAIN=mysql://h\n' +
      '# DATABASE_URL=postgres://commented-out\nOTHER=1\n# DATABASE_URL: Unquoted name  \nLAST=x',
  );
  assert.deepEqual(Object.fromEntries(r.named), { DB_URI: 'Interaction test', REPORTS_DB: 'Reports', PLAIN: '', LAST: 'Unquoted name' });
  assert.equal(r.vars.get('DB_URI'), 'mongodb://h/db');
  assert.deepEqual(r.errors, []);
  assert.deepEqual(Object.fromEntries(parseEnv('# DATABASE_URL: Dangling').named), {}, 'a marker without a following variable marks nothing');
});
