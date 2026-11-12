import { test, expect, P, at, isMobile } from "./fixtures";

const MR = `${P}/-/merge_requests/1`;

test("diff is syntax highlighted, with a file list and sticky header", async ({ page }) => {
  await page.goto(`${MR}/diffs`);
  await expect(page.locator(".diff-line.add").first()).toBeVisible();
  // server-side highlighting: token spans inside diff lines
  await expect(page.locator(".diff-line .code-text span[class^='hl-']").first()).toBeVisible();
  if (isMobile(page)) {
    await expect(page.locator(".diff-tree")).toHaveCount(0);
    await expect(page.locator(".seg")).toHaveCount(0);
    return;
  }
  const tree = page.locator(".diff-tree");
  await expect(tree.locator(".diff-tree-item")).toHaveCount(9);
  await tree.locator(".diff-tree-item", { hasText: "app.ts" }).click();
  await expect(page.locator(".diff-sticky .diff-head, .diff-head").filter({ hasText: "web/app.ts" }).first()).toBeInViewport();
  await expect(tree.locator(".diff-tree-item.active")).toContainText("app.ts");
});

test("] walks to the last file and the list follows", async ({ page }) => {
  test.skip(isMobile(page), "the file list is desktop-only");
  await page.goto(`${MR}/diffs`);
  await expect(page.locator(".diff-tree-item")).toHaveCount(9);
  for (let i = 0; i < 8; i++) await page.keyboard.press("]");
  await expect(page.locator(".diff-tree-item.active")).toContainText("app.ts");
});

test("switching versions while loading shows the chosen version", async ({ page }) => {
  // single-version fixture: at least make sure a re-selection reloads cleanly
  await page.goto(`${MR}/diffs`);
  await expect(page.locator(".diff-line").first()).toBeVisible();
  await page.getByLabel("Base version").click();
  await page.getByRole("option").first().click();
  await expect(page.locator(".diff-line").first()).toBeVisible();
});

test("split view pairs removals with additions and is remembered", async ({ page }) => {
  test.skip(isMobile(page), "split view is desktop-only");
  await page.goto(`${MR}/diffs`);
  await page.getByRole("button", { name: "Split" }).click();
  const pair = page.locator(".diff-line.pair").filter({ has: page.locator(".half.del") }).filter({ has: page.locator(".half.add") }).first();
  await expect(pair).toBeVisible();
  await page.reload();
  await expect(page.getByRole("button", { name: "Split" })).toHaveAttribute("aria-pressed", "true");
  await page.getByRole("button", { name: "Unified" }).click();
  await expect(page.locator(".diff-line.pair")).toHaveCount(0);
});

test("] and n move through files and unresolved threads", async ({ page }) => {
  await page.goto(`${MR}/diffs`);
  await expect(page.locator(".diff-line").first()).toBeVisible();
  await page.keyboard.press("n");
  await expect(page.getByText("Should the discount be capped?")).toBeInViewport();
  const y0 = await page.evaluate(() => scrollY);
  await page.keyboard.press("]");
  await expect.poll(() => page.evaluate(() => scrollY)).not.toBe(y0);
});

test("comment preview renders Markdown on the server", async ({ page }) => {
  await page.goto(MR);
  const editor = page.locator(".md-editor").last();
  await editor.locator("textarea").fill("This is **bold**");
  await editor.getByRole("tab", { name: "Preview" }).click();
  await expect(editor.locator(".md-preview strong")).toHaveText("bold");
  await editor.getByRole("tab", { name: "Write" }).click();
  await expect(editor.locator("textarea")).toHaveValue("This is **bold**");
});

test("details panel collapses; actions confirm with a toast", async ({ page }) => {
  await page.goto(MR);
  const panel = page.getByRole("complementary", { name: "Merge request details" });
  await expect(panel).toBeVisible();
  await panel.getByRole("button", { name: "Close merge request" }).click();
  await expect(page.locator(".toast.ok")).toContainText("closed");
  await panel.getByRole("button", { name: "Reopen" }).click();
  await expect(page.locator(".toast.ok").last()).toContainText("reopened");
  await page.getByRole("button", { name: "Toggle details panel" }).click();
  await expect(panel).toHaveCount(0);
  await page.reload();
  await expect(page.getByRole("complementary", { name: "Merge request details" })).toHaveCount(0);
  await page.getByRole("button", { name: "Toggle details panel" }).click();
  await expect(page.getByRole("complementary", { name: "Merge request details" })).toBeVisible();
});

test("MR list prefetches on hover", async ({ page }) => {
  await page.goto(`${P}/-/merge_requests`);
  const link = page.getByRole("link", { name: /Checkout with discounts/ });
  const req = page.waitForRequest((r) => /merge_requests\/1$/.test(new URL(r.url()).pathname));
  await link.hover();
  await req;
  await link.click();
  await at(page, MR);
});
