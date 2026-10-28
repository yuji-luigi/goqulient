// Launches VS Code with the extension and test-e2e/suite.cjs (@vscode/test-electron), on a workspace whose .env points
// at the Docker test databases. Run under a display: `xvfb-run -a node test-e2e/run.mjs` (scripts/dev.sh vscode e2e).
// Env: GOKU_ENGINE_BIN (engine build), GOKU_E2E_VSCODE (VS Code version, default stable),
// GOKU_E2E_SCREENSHOTS (directory for screenshots of the window; none when unset).
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { runTests } from '@vscode/test-electron';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const engine = process.env.GOKU_ENGINE_BIN;
if (!engine) throw new Error('Set GOKU_ENGINE_BIN to a goku-engine-server build.');

const scratch = mkdtempSync(join(tmpdir(), 'goqulient-e2e-'));
const workspace = join(scratch, 'workspace');
mkdirSync(workspace);
// Test databases only (docker-compose.yml); the passwords are the throwaway ones from there.
writeFileSync(
  join(workspace, '.env'),
  [
    'DATABASE_URL=postgres://dbc:dbcpass@pg-primary:5432/shop',
    'MYSQL_URL=mysql://dbc:dbcpass@mysql-primary:3306/shop?ssl-mode=REQUIRED',
    'MONGODB_URI=mongodb://mongo:27017/shop',
    '',
  ].join('\n'),
);
const userData = join(scratch, 'user-data');
mkdirSync(join(userData, 'User'), { recursive: true });
writeFileSync(
  join(userData, 'User', 'settings.json'),
  JSON.stringify({ 'workbench.startupEditor': 'none', 'window.restoreWindows': 'none', 'telemetry.telemetryLevel': 'off' }),
);

const code = await runTests({
  version: process.env.GOKU_E2E_VSCODE || 'stable',
  cachePath: process.env.GOKU_E2E_CACHE || join(root, '.vscode-test'),
  extensionDevelopmentPath: root,
  extensionTestsPath: join(root, 'test-e2e', 'suite.cjs'),
  launchArgs: [
    workspace,
    '--user-data-dir', userData,
    '--extensions-dir', join(scratch, 'extensions'),
    '--disable-extensions',
    '--disable-workspace-trust',
    '--disable-gpu',
    '--no-sandbox',
    '--password-store=basic',
    '--skip-welcome',
    '--skip-release-notes',
  ],
  extensionTestsEnv: { GOKU_ENGINE_PATH: engine, GOKU_E2E_SCREENSHOTS: process.env.GOKU_E2E_SCREENSHOTS ?? '' },
}).catch((e) => {
  console.error(e instanceof Error ? e.message : String(e));
  return 1;
});
process.exit(code);
