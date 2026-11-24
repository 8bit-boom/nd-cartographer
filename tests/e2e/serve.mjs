// Boot the real studio binary for the Playwright smoke test on a scratch data
// dir. Build it first: cargo build -p ndcart-server --bin ndcart.
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const exe = process.platform === 'win32' ? 'ndcart.exe' : 'ndcart';
const bin = join('target', 'debug', exe);
if (!existsSync(bin)) {
  console.error(`ndcart binary not found at ${bin}; run: cargo build -p ndcart-server --bin ndcart`);
  process.exit(1);
}
const data = mkdtempSync(join(tmpdir(), 'ndcart-e2e-'));
const child = spawn(bin, ['serve', '--listen', '127.0.0.1:8399', '--data-dir', data], {
  stdio: 'inherit',
});
const stop = () => { try { child.kill(); } catch { /* already gone */ } };
process.on('SIGTERM', stop);
process.on('SIGINT', stop);
child.on('exit', (code) => process.exit(code ?? 0));