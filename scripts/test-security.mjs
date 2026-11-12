import assert from 'node:assert/strict';
import { createServer } from 'vite';
import { chromium } from 'playwright-core';

const server = await createServer({ server: { host: '127.0.0.1', port: 0, strictPort: false, hmr: false } });
let browser;
try {
  await server.listen();
  const origin = `http://127.0.0.1:${server.httpServer.address().port}`;
  browser = await chromium.launch({ headless: true, executablePath: process.env.BROWSER_EXECUTABLE || undefined });
  let passed = 0;
  for (const [platform, userAgent] of [
    ['Windows', 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/130.0.0.0 Safari/537.36'],
    ['macOS', 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 Chrome/130.0.0.0 Safari/537.36'],
  ]) {
    const context = await browser.newContext({ userAgent });
    try {
      await context.route('**/*', route => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
      const page = await context.newPage(), errors = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.goto(`${origin}/tests/html-security.html`);
      await page.waitForFunction(() => window.securityResults);
      const result = await page.evaluate(() => window.securityResults);
      assert.equal(result.failed, 0, JSON.stringify(result));
      assert(result.passed > 0);
      passed += result.passed;
      console.log(`PASS ${platform} HTML security: ${result.passed}`);
      if (platform === 'Windows') {
        await page.goto(`${origin}/tests/security-settings.html`);
        await page.waitForFunction(() => document.querySelector('#results').textContent.includes('"failed"'));
        const settings = JSON.parse(await page.locator('#results').textContent());
        assert.equal(settings.failed, 0, JSON.stringify(settings));
        passed += settings.passed;
        console.log(`PASS security settings: ${settings.passed}`);
      }
      assert.deepEqual(errors, [], 'Browser script errors');
    } finally { await context.close(); }
  }
  console.log(`Security checks: ${passed} passed`);
} finally { await browser?.close(); await server.close(); }
