// Screenshots of both webviews in the dev harness, plus rough frame timings. Runs in the Playwright image (docs/vscode-extension.md, "Webviews"):
//   node webview-dev/snapshot.mjs   → webview-dev/snapshots/*.png (gitignored)
// Serves the repo root (the ER fixtures live in Tests/Parity). Fails on page errors and CSP violations.
import { createServer } from 'node:http';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const root = fileURLToPath(new URL('../../', import.meta.url));
const out = fileURLToPath(new URL('./snapshots/', import.meta.url));
await mkdir(out, { recursive: true });
const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.json': 'application/json' };
const server = createServer(async (req, res) => {
  const path = normalize(decodeURIComponent(new URL(req.url, 'http://x').pathname));
  try {
    const body = await readFile(join(root, path));
    res.writeHead(200, { 'content-type': types[extname(path)] ?? 'application/octet-stream' }).end(body);
  } catch {
    res.writeHead(404).end();
  }
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}/vscode/webview-dev`;

const browser = await chromium.launch();
const problems = [];
const report = [];

async function open(name, query, size = { width: 1280, height: 760 }) {
  const page = await browser.newPage({ viewport: size, deviceScaleFactor: 1 });
  page.on('pageerror', (e) => problems.push(`${name}: ${e.message}`));
  page.on('console', (m) => {
    if (m.type() === 'error' || /Content Security Policy/.test(m.text())) problems.push(`${name}: ${m.text()}`);
  });
  await page.goto(`${base}/${query}`);
  return page;
}

const shot = (page, file) => page.screenshot({ path: join(out, file) });

/** Frame times while scrolling the grid (`dx`, `dy` per frame) or wheeling the ER stage, one step per frame. */
async function frameTimes(page, frames, step) {
  return page.evaluate(
    async ([frames, step]) => {
      // No `new Function`: the page's CSP forbids eval, like the real webview's.
      const fn = () => {
        if (step.kind === 'scroll') {
          const b = document.querySelector('.body');
          b.scrollTop += step.dy;
          b.scrollLeft += step.dx;
        } else {
          document.querySelector('.stage').dispatchEvent(new WheelEvent('wheel', { deltaX: step.dx, deltaY: step.dy, cancelable: true }));
        }
      };
      const times = [];
      let last = performance.now();
      for (let i = 0; i < frames; i++) {
        fn();
        await new Promise((r) => requestAnimationFrame(() => r()));
        const now = performance.now();
        times.push(now - last);
        last = now;
      }
      times.sort((a, b) => a - b);
      return { avg: +(times.reduce((a, b) => a + b, 0) / times.length).toFixed(1), p95: +times[Math.floor(times.length * 0.95)].toFixed(1), max: +times.at(-1).toFixed(1) };
    },
    [frames, step],
  );
}

// MARK: - Table

{
  const page = await open('table-dark', 'table.html?theme=dark');
  await page.waitForSelector('.cell');
  const cell = (r, c) => page.locator(`.cell[data-r="${r}"][data-c="${c}"]`);
  await cell(2, 1).click();
  await cell(5, 3).click({ modifiers: ['Shift'] });
  await shot(page, 'table-dark.png');
  report.push(`table: ${await page.locator('.cell').count()} cells in the DOM for 1,000 × 50`);
  report.push(`table scroll (1,000 × 50, 180 px down + 40 px right per frame): ${JSON.stringify(await frameTimes(page, 120, { kind: 'scroll', dx: 40, dy: 180 }))}`);
  await page.keyboard.press('Control+c');
  await page.evaluate(() => document.querySelector('.body').scrollTo(0, 0));
  await cell(2, 1).click();
  await cell(3, 2).click({ modifiers: ['Shift'] });
  await page.keyboard.press('Control+c');
  const copied = await page.evaluate(() => window.__goku.copied.at(-1));
  report.push(`table copy of 2×2 selection: ${JSON.stringify(copied)}`);
  // Sort by a numeric column, then page forward.
  await page.locator('.hcell', { has: page.locator('.hname', { hasText: /^amount_2$/ }) }).click();
  await page.waitForFunction(() => window.__goku.received.some((m) => m.type === 'load' && m.sort?.column === 'amount_2'));
  await page.waitForSelector('.hcell.sorted');
  await page.getByRole('button', { name: 'Next ›' }).click();
  await page.waitForFunction(() => document.querySelector('.status').textContent.startsWith('Rows 1,001'));
  await page.evaluate(() => {
    const b = document.querySelector('.body');
    b.scrollTop = 22 * 400;
    b.scrollLeft = 900;
  });
  await page.waitForTimeout(100);
  await shot(page, 'table-dark-sorted-page2.png');
  await page.close();
}
{
  const page = await open('table-light', 'table.html?theme=light');
  await page.waitForSelector('.cell');
  const target = page.locator('.cell[data-r="3"][data-c="1"]');
  const box = await target.boundingBox();
  await page.mouse.click(box.x + 40, box.y + 10, { button: 'right' });
  await page.waitForSelector('.menu');
  await shot(page, 'table-light-menu.png');
  await page.close();
}
for (const [name, query] of [
  ['table-light-mongo-documents', 'table.html?theme=light&fixture=mongo'],
  ['table-dark-mongo-grid', 'table.html?theme=dark&fixture=mongo'],
  ['table-dark-error', 'table.html?theme=dark&fixture=error'],
  ['table-light-loading', 'table.html?theme=light&fixture=loading'],
  ['table-light-empty', 'table.html?theme=light&fixture=empty'],
  ['table-hc', 'table.html?theme=hc'],
]) {
  const page = await open(name, query);
  await page.waitForTimeout(400);
  if (name.includes('documents')) await page.getByRole('button', { name: 'Documents' }).click();
  await shot(page, `${name}.png`);
  await page.close();
}

// MARK: - ER

async function openEr(name, query, size) {
  const page = await open(name, query, size);
  await page.waitForSelector('.entity');
  await page.waitForTimeout(150);
  return page;
}

/** Clicks the middle of an entity's header. */
async function clickEntity(page, id) {
  const box = await page.locator(`.entity[data-id="${id}"] .head`).boundingBox();
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
}

{
  const page = await openEr('er-postgres-dark', 'er.html?theme=dark&fixture=postgres');
  await shot(page, 'er-postgres-dark.png');
  await page.getByRole('button', { name: 'PNG' }).click();
  await page.waitForFunction(() => window.__goku.exports.length === 1);
  await page.getByRole('button', { name: 'SVG' }).click();
  await page.waitForFunction(() => window.__goku.exports.length === 2);
  const [png, svgExport] = await page.evaluate(() => window.__goku.exports.map((e) => e.data));
  await writeFile(join(out, 'er-export-postgres.png'), Buffer.from(png, 'base64'));
  await writeFile(join(out, 'er-export-postgres.svg'), svgExport);
  // Drag a box: the extension must get its new position.
  const head = await page.locator('.entity[data-id="inventory.warehouses"] .head').boundingBox();
  await page.mouse.move(head.x + 20, head.y + 10);
  await page.mouse.down();
  await page.mouse.move(head.x + 80, head.y + 160, { steps: 8 });
  await page.mouse.up();
  const moved = await page.evaluate(() => window.__goku.received.filter((m) => m.type === 'moved').at(-1));
  report.push(`er drag → ${JSON.stringify(moved)}`);
  await page.close();
}
{
  const page = await openEr('er-postgres-light', 'er.html?theme=light&fixture=postgres');
  await clickEntity(page, 'public.orders');
  await page.getByRole('button', { name: '+' }).click();
  await page.waitForTimeout(100);
  await shot(page, 'er-postgres-light-selected.png');
  await page.close();
}
{
  const page = await openEr('er-mysql-dark', 'er.html?theme=dark&fixture=mysql');
  const box = await page.locator('.entity[data-id="order_items"] .head').boundingBox();
  await page.mouse.move(box.x + box.width / 2, box.y + 5);
  await page.waitForTimeout(100);
  await shot(page, 'er-mysql-dark-hover.png');
  await page.close();
}
{
  const page = await openEr('er-mongodb-light', 'er.html?theme=light&fixture=mongodb');
  const at = await page.evaluate(() => {
    const p = document.querySelector('.rel .line');
    const pt = p.getPointAtLength(p.getTotalLength() / 2).matrixTransform(p.getScreenCTM());
    return { x: pt.x, y: pt.y };
  });
  await page.mouse.click(at.x, at.y);
  await page.waitForTimeout(100);
  await shot(page, 'er-mongodb-light-relationship.png');
  await page.close();
}
{
  const page = await openEr('er-search-dark', 'er.html?theme=dark&fixture=postgres');
  await page.locator('input.search').fill('order');
  await page.waitForTimeout(100);
  await shot(page, 'er-postgres-dark-search.png');
  await page.close();
}
{
  const page = await openEr('er-300', 'er.html?theme=dark&fixture=synthetic300', { width: 1440, height: 900 });
  await shot(page, 'er-synthetic300-dark-fit.png');
  const count = () => page.evaluate(() => [document.querySelectorAll('.entity').length, document.querySelectorAll('svg.er *').length]);
  report.push(`er 300 at fit (${await page.locator('.zoom').textContent()}): [entities, svg nodes] = ${JSON.stringify(await count())}`);
  report.push(`er 300 pan at fit: ${JSON.stringify(await frameTimes(page, 90, { kind: 'wheel', dx: 6, dy: 4 }))}`);
  await page.locator('input.search').fill('golf_12');
  await page.locator('input.search').press('Enter'); // selects and centres the match
  for (let i = 0; i < 9; i++) await page.getByRole('button', { name: '+' }).click();
  await page.waitForTimeout(100);
  report.push(`er 300 zoomed in (${await page.locator('.zoom').textContent()}): [entities, svg nodes] = ${JSON.stringify(await count())}`);
  await shot(page, 'er-synthetic300-dark-zoomed.png');
  report.push(`er 300 pan zoomed in: ${JSON.stringify(await frameTimes(page, 90, { kind: 'wheel', dx: 25, dy: 18 }))}`);
  await page.close();
}
{
  const page = await openEr('er-hc', 'er.html?theme=hc&fixture=mysql');
  await shot(page, 'er-mysql-hc.png');
  await page.close();
}

await browser.close();
server.close();
console.log(report.join('\n'));
if (problems.length) {
  console.error(`\n${problems.length} problem(s):\n${problems.join('\n')}`);
  process.exit(1);
}
