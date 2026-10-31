import { defineConfig } from '@playwright/test';

const runDir = process.env.VERIFY_RUN_DIR;

export default defineConfig({
  testDir: '.',
  testMatch: 'extension-capture.spec.ts',
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 10 * 60 * 1000,
  expect: { timeout: 15_000 },
  reporter: [
    ['list'],
    ['json', { outputFile: runDir ? `${runDir}/playwright-report.json` : 'test-results/report.json' }],
  ],
  use: {
    headless: false,
    trace: 'off',
  },
});
