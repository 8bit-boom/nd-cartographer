import { defineConfig } from '@playwright/test';

// End-to-end smoke: build the server first (cargo build -p ndcart-server), then
// `npm run test:e2e`. The webServer launcher boots the real binary on a scratch
// data dir so the test exercises the actual studio, not a mock.
export default defineConfig({
  testDir: './tests/e2e',
  timeout: 30_000,
  retries: 0,
  use: {
    baseURL: 'http://127.0.0.1:8399',
  },
  webServer: {
    command: 'node tests/e2e/serve.mjs',
    url: 'http://127.0.0.1:8399/health',
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});