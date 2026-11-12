// Real category/cards with local image responses and mocked Tauri IPC.
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { createServer } from "vite";
import * as playwright from "playwright-core";
const engine = process.env.BROWSER_ENGINE || "chromium";
const output = `test-results/category-layout/${engine}`;
await mkdir(output, { recursive: true });
const vite = await createServer({ server: { host: "127.0.0.1", port: 0, strictPort: false } });
let browser;
const passed = [];
try {
  await vite.listen();
  const origin = vite.resolvedUrls.local[0];
  browser = await playwright[engine].launch({ headless: true });
  const page = await browser.newPage({ userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) HGhubLayoutTest", viewport: { width: 2048, height: 1000 } });
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.route("**/*", async route => {
    const url = route.request().url();
    if (url.includes("/img?u=") || url.includes("fixture-cover.svg")) {
      if (url.includes("broken")) return route.fulfill({ status: 404, body: "missing" });
      await new Promise(resolve => setTimeout(resolve, url.includes("%2F2.svg") ? 120 : 15));
      return route.fulfill({ contentType: "image/svg+xml", body: '<svg xmlns="http://www.w3.org/2000/svg" width="300" height="400"><rect width="300" height="400" fill="#206a98"/><circle cx="150" cy="160" r="80" fill="#60cdff"/></svg>' });
    }
    if (url.startsWith(origin)) return route.continue();
    throw new Error(`Unexpected upstream request: ${url}`);
  });
  const resize = async width => {
    await page.setViewportSize({ width, height: 1000 });
    await page.evaluate(width => window.layoutFixture.setWidth(width), width);
    await page.waitForFunction(() => [...document.querySelectorAll(".grid")].every(el => {
      const gap = parseFloat(getComputedStyle(el).gap);
      const columns = el.clientWidth <= 480 ? 2 : Math.max(2, Math.floor((el.clientWidth + gap) / (180 + gap)));
      return getComputedStyle(el).gridTemplateColumns.split(" ").length === columns;
    }));
  };
  const imagesLoaded = async selector => {
    await page.waitForFunction(selector => {
      const hosts = [...document.querySelectorAll(selector)];
      const visible = hosts.filter(el => { const b = el.getBoundingClientRect(); return b.top < innerHeight && b.bottom > 0 && b.left < innerWidth && b.right > 0; });
      return visible.length > 0 && visible.every(el => {
        const img = el.querySelector("img");
        return img?.complete && img.naturalWidth > 0 && getComputedStyle(img).opacity === "1";
      });
    }, selector);
  };
  for (const mode of ["channel", "posts"]) {
    await page.goto(`${origin}tests/category-layout.html?mode=${mode}`);
    await page.locator(".grid > button").first().waitFor();
    for (const width of [2048, 960, 320, 390, 768, 1440, 2048]) {
      await resize(width);
      await imagesLoaded(".grid > button");
      const rects = await page.locator(".grid > button").evaluateAll(nodes => nodes.map(el => {
        const { x, y, width, height } = el.getBoundingClientRect(); return { x, y, width, height };
      }));
      const cols = rects.findIndex(r => r.y > rects[0].y + 1);
      assert(cols >= 2);
      if (width === 390) assert.equal(cols, 2, "Phone keeps two columns");
      if (width === 2048) assert(cols >= 8, "Desktop computes enough columns for readable cover sizes");
      assert.equal(rects.length, 20);
      assert(rects.slice(0, cols).every(r => Math.abs(r.y - rects[0].y) < 1), "First row aligned");
      assert(rects[cols].y > rects[0].y + rects[0].height - 1, "Next row below first");
      assert(Math.abs(rects[0].height / rects[0].width - (mode === "posts" ? 1.28 : 1.32)) < .02, "Stable cover ratio");
      assert(rects[cols - 1].x + rects[cols - 1].width >= width - 20, "Row fills container");
    }
    await page.locator(".scroll-y").evaluate(el => { el.scrollTop = el.scrollHeight; });
    await imagesLoaded(".grid > button");
    await page.locator(".scroll-y").evaluate(el => { el.scrollTop = 0; });
    await imagesLoaded(".grid > button");
    if (mode === "channel") {
      await page.getByRole("button", { name: "最热", exact: true }).click();
      await imagesLoaded(".grid > button");
    }
    // A narrow content pane inside a wide desktop window must use its own width.
    await page.evaluate(() => window.layoutFixture.setWidth(390));
    await page.waitForFunction(() => getComputedStyle(document.querySelector(".grid")).gridTemplateColumns.split(" ").length === 2);
    await imagesLoaded(".grid > button");
    await resize(2048);
    await imagesLoaded(".grid > button");
    const screenshot = await page.screenshot({ path: `${output}/${mode}.png` });
    const pixels = await page.evaluate(async encoded => {
      const image = new Image(); image.src = `data:image/png;base64,${encoded}`; await image.decode();
      const canvas = document.createElement("canvas"); canvas.width = image.width; canvas.height = image.height;
      const context = canvas.getContext("2d"); context.drawImage(image, 0, 0);
      const cards = [...document.querySelectorAll(".grid > button")];
      const firstY = cards[0].getBoundingClientRect().y;
      return cards.filter(el => Math.abs(el.getBoundingClientRect().y - firstY) < 1).map(el => {
        const r = el.getBoundingClientRect();
        return [...context.getImageData(Math.floor(r.x + r.width / 2), Math.floor(r.y + r.height / 4), 1, 1).data];
      });
    }, screenshot.toString("base64"));
    assert(pixels.every(([r, g, b]) => r === 96 && g === 205 && b === 255), "Every first-row cover actually paints, including later columns");
    passed.push(`${mode}: all visible covers, resize 320–2048px, narrow pane in wide window, scroll, stable row geometry`);
  }
  await page.goto(`${origin}tests/category-layout.html?mode=overview`);
  const row = page.locator(".category-preview").first();
  await row.locator("button").nth(7).waitFor();
  for (const width of [2048, 960, 390]) {
    await resize(width);
    const geometry = await row.evaluate(el => {
      const b = el.getBoundingClientRect(); const last = el.lastElementChild.getBoundingClientRect();
      return { end: last.right, right: b.right, scroll: el.scrollWidth, width: el.clientWidth };
    });
    if (width > 900) assert(Math.abs(geometry.end - (geometry.right - 16)) < 2, "Eight previews fill row");
    else assert(geometry.scroll > geometry.width, "Mobile keeps horizontal scrolling");
  }
  await resize(2048);
  await imagesLoaded(".category-preview > button");
  await page.screenshot({ path: `${output}/overview.png` });
  passed.push("overview: fills wide row, mobile horizontal scroll");
  await page.goto(`${origin}tests/category-layout.html?mode=image`);
  await imagesLoaded(".w-40");
  await page.evaluate(() => window.layoutFixture.setSrc("/fixture-cover.svg?broken"));
  await page.waitForFunction(() => document.querySelector(".w-40 .bg-s3"));
  await page.evaluate(() => window.layoutFixture.setSrc("/fixture-cover.svg?recovered"));
  await imagesLoaded(".w-40");
  passed.push("image: changed source recovers from previous error");
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ engine, passed }, null, 2));
  await writeFile(`${output}/report.json`, JSON.stringify({ engine, passed }, null, 2));
} finally {
  await browser?.close();
  await vite.close();
}
