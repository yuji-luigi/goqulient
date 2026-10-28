import assert from 'node:assert/strict';
import { join } from 'node:path';
import { test } from 'node:test';
import { bundledBinaryPath, resolveBinary } from '../src/engine/binary.ts';

test('bundled binary per platform, .exe on Windows', () => {
  assert.equal(bundledBinaryPath('/ext', 'linux', 'arm64'), join('/ext', 'bin', 'linux-arm64', 'goku-engine-server'));
  assert.equal(bundledBinaryPath('/ext', 'win32', 'x64'), join('/ext', 'bin', 'win32-x64', 'goku-engine-server.exe'));
});

test('override wins when it exists, and fails loudly when it does not', () => {
  assert.equal(resolveBinary({ extensionPath: '/ext', override: ' /dev/engine ', exists: () => true }), '/dev/engine');
  assert.throws(() => resolveBinary({ extensionPath: '/ext', override: '/nope', exists: () => false }), /enginePath points to “\/nope”/);
});

test('missing bundled binary: reinstall for supported targets, list them otherwise', () => {
  const base = { extensionPath: '/ext', exists: () => false };
  assert.throws(() => resolveBinary({ ...base, platform: 'darwin', arch: 'arm64' }), /for darwin-arm64 is missing/);
  assert.throws(() => resolveBinary({ ...base, platform: 'freebsd', arch: 'x64' }), /no engine for freebsd-x64\. Supported: win32-x64/);
  assert.equal(resolveBinary({ ...base, platform: 'linux', arch: 'x64', exists: () => true }), join('/ext', 'bin', 'linux-x64', 'goku-engine-server'));
});
