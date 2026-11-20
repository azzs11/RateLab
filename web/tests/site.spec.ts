import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

const ROUTES = ["", "lab", "cluster", "explorer", "replay", "results"];

async function open(page: Page, route: string) {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  page.on("console", (m) => {
    if (m.type() === "error") errors.push(m.text());
  });
  await page.goto(`/#/${route}`);
  await expect(page.locator("main h1")).toBeVisible();
  return errors;
}

for (const route of ROUTES) {
  for (const scheme of ["light", "dark"] as const) {
    test(`/${route} renders and has no accessibility violations (${scheme})`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: scheme });
      const errors = await open(page, route);
      const result = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"]).analyze();
      expect(result.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target).join(" ")}`)).toEqual([]);
      expect(errors).toEqual([]);
    });
  }
}

test("the boundary attack plays on load and fixed window reaches twice the limit", async ({ page }) => {
  await open(page, "");
  const fixed = page.locator(".attack-row").first().locator(".num");
  await expect.poll(async () => Number(await fixed.textContent()), { timeout: 15_000 }).toBe(20);
  const gcra = page.locator(".attack-row").nth(2).locator(".num");
  expect(Number(await gcra.textContent())).toBeLessThanOrEqual(10);
});

test("with reduced motion the attack still advances, in steps", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await open(page, "");
  const fixed = page.locator(".attack-row").first().locator(".num");
  await expect.poll(async () => Number(await fixed.textContent()), { timeout: 15_000 }).toBeGreaterThan(0);
});

test("the lab can compare two algorithms", async ({ page }) => {
  await open(page, "lab?a=fixed_window&b=gcra&pattern=boundary_attack");
  await expect(page.locator(".lab-panel")).toHaveCount(2);
  await expect(page.locator(".lab-panel h2")).toHaveText(["Fixed window", "GCRA"]);
});

test("pareto points are reachable by keyboard", async ({ page }) => {
  await open(page, "explorer?nodes=3");
  const point = page.locator(".pareto .point").first();
  await point.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("link", { name: /Replay in the cluster simulator/ })).toBeVisible();
});

test("a log can be replayed end to end", async ({ page }) => {
  await open(page, "replay");
  await page.getByRole("button", { name: "Try a generated sample log" }).click();
  await page.getByRole("button", { name: /Replay this log/ }).click();
  await expect(page.getByText("Recommendation")).toBeVisible({ timeout: 30_000 });
  await expect(page.locator(".replay-results table")).toHaveCount(2);
});

test("the skip link is the first thing a keyboard reaches", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("main h1")).toBeVisible();
  await page.keyboard.press("Tab");
  await expect(page.getByRole("link", { name: "Skip to content" })).toBeFocused();
});

test("no horizontal scrolling", async ({ page }) => {
  for (const route of ROUTES) {
    await open(page, route);
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow, `/${route}`).toBeLessThanOrEqual(1);
  }
});
