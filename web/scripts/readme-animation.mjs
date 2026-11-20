// Captures frames of the essay's first figure (the boundary attack) from a running
// preview of the site, for the README's animation. `just animation` runs the preview,
// this script and web/scripts/apng.py.
//
//   node web/scripts/readme-animation.mjs <frames-dir> [base-url]

import { chromium } from "@playwright/test";

const out = process.argv[2];
const base = process.argv[3] ?? "http://localhost:4173/distributed-rate-limiter/";
if (!out) {
  console.error("usage: readme-animation.mjs <frames-dir> [base-url]");
  process.exit(2);
}
const browser = await chromium.launch();
try {
  const page = await browser.newPage({ viewport: { width: 1200, height: 900 }, colorScheme: "light" });
  await page.goto(`${base}#/`);
  await page.waitForSelector("#boundary .attack", { timeout: 20_000 });
  const figure = page.locator("#boundary figure");
  await figure.scrollIntoViewIfNeeded();
  await page.getByRole("button", { name: "Start again" }).click();
  await page.mouse.move(0, 0); // no hover state in the frames
  for (let i = 0; i < 34; i++) {
    await figure.screenshot({ path: `${out}/f${String(i).padStart(3, "0")}.png` });
    await page.waitForTimeout(190);
  }
  console.log(`captured 34 frames into ${out}`);
} finally {
  await browser.close();
}
