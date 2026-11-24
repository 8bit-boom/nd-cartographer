// Capture README screenshots from the real running studio (not mockups).
// Build first: cargo build -p ndcart-server --bin ndcart
// Run:        node scripts/capture-screenshots.mjs
import { chromium } from '@playwright/test';
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const exe = process.platform === 'win32' ? 'ndcart.exe' : 'ndcart';
const bin = join('target', 'debug', exe);
if (!existsSync(bin)) {
  console.error('build the server first: cargo build -p ndcart-server --bin ndcart');
  process.exit(1);
}
const data = mkdtempSync(join(tmpdir(), 'ndcart-shots-'));
const child = spawn(bin, ['serve', '--listen', '127.0.0.1:8399', '--data-dir', data], { stdio: 'ignore' });
const stop = () => { try { child.kill(); } catch { /* gone */ } };
process.on('exit', stop);
process.on('SIGINT', () => { stop(); process.exit(0); });

const base = 'http://127.0.0.1:8399';
async function up() {
  for (let i = 0; i < 60; i++) {
    try { const r = await fetch(base + '/health'); if (r.ok) return; } catch { /* starting */ }
    await new Promise((r) => setTimeout(r, 250));
  }
  throw new Error('server did not come up');
}
const post = (path, body) => fetch(base + path, {
  method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
}).then((r) => r.json());

await up();
mkdirSync('docs/screenshots', { recursive: true });
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });

// --- Battle map: generate a dungeon, frame it, paint a little, screenshot. ---
await page.goto(base + '/');
await page.locator('#doc-name').waitFor({ state: 'visible' });
await page.click('#btn-new');
await page.fill('#nd-name', 'The Sunken Crypt');
await page.check('input[name=nd-kind][value=battle]');
await page.selectOption('#nd-start', 'dungeon');
await page.click('#nd-ok');
await page.waitForTimeout(1500);
// Frame the whole dungeon: zoom out to z0 and centre via the minimap jump.
await page.evaluate(() => { state.z = 0; state.px = 40; state.py = 40; render(); });
await page.waitForTimeout(600);
await page.screenshot({ path: 'docs/screenshots/battle-map.png' });

// --- World map: generate an overland world sized to fit the viewport at z0. ---
await page.click('#btn-new');
await page.fill('#nd-name', 'The Sword Coast');
await page.check('input[name=nd-kind][value=region]');
await page.fill('#nd-cols', '80');
await page.fill('#nd-rows', '50');
await page.click('#nd-ok');
await page.waitForTimeout(2500);
await page.evaluate(() => { state.z = 0; state.px = 0; state.py = 0; render(); });
await page.waitForTimeout(800);
await page.screenshot({ path: 'docs/screenshots/world-map.png' });

// --- Player view: back to the dungeon, switch on the fog preview. ---
await page.selectOption('#doc-select', { index: 0 });
await page.waitForTimeout(800);
await page.click('#btn-preview');
await page.waitForTimeout(1200);
await page.screenshot({ path: 'docs/screenshots/player-view.png' });

await browser.close();
stop();
console.log('screenshots written to docs/screenshots/');