import { test, expect, P, at, isMobile } from "./fixtures";

test("dashboard → project through the sidebar and breadcrumbs", async ({ page }) => {
  await page.goto("/dashboard/projects");
  await expect(page.getByRole("heading", { name: "Your projects" })).toBeVisible();
  if (isMobile(page)) {
    await expect(page.locator(".sidebar")).not.toBeInViewport();
    await page.getByLabel("Open navigation").click();
    await expect(page.locator(".sidebar")).toBeInViewport();
    // tap the dimmed area beside the menu (its middle is under the 260 px menu)
    await page.locator(".scrim").click({ position: { x: 370, y: 200 } });
    await expect(page.locator(".sidebar")).not.toBeInViewport();
  } else {
    await expect(page.locator(".sidebar").getByRole("link", { name: "Projects" })).toHaveClass(/active/);
  }
  await page.locator(".page").getByRole("link", { name: "e2e/shop" }).click();
  await at(page, P);
  await expect(page.locator(".page-head h1")).toBeVisible();
  // project sidebar replaces "Your work"
  if (isMobile(page)) await page.getByLabel("Open navigation").click();
  const side = page.locator(".sidebar");
  await expect(side.getByRole("link", { name: "Merge requests" })).toBeVisible();
  await side.getByRole("link", { name: "Branches" }).click();
  await at(page, `${P}/-/branches`);
  await expect(page.locator(".breadcrumbs .current")).toHaveText("Branches");
});

test("theme switch cycles and persists", async ({ page }) => {
  await page.goto("/");
  const btn = page.getByRole("button", { name: /^Theme:/ });
  const theme = () => page.evaluate(() => document.documentElement.dataset.theme ?? "system");
  const start = await theme();
  await btn.click();
  const next = await theme();
  expect(next).not.toBe(start);
  await page.reload();
  expect(await theme()).toBe(next);
  // back to system so other tests see the default
  while ((await theme()) !== "system") await page.getByRole("button", { name: /^Theme:/ }).click();
});

test("sidebar collapse is remembered", async ({ page }) => {
  test.skip(isMobile(page), "the sidebar is an overlay on small screens");
  await page.goto("/");
  await page.getByRole("button", { name: "Collapse sidebar" }).click();
  await expect(page.locator(".sidebar")).toHaveClass(/collapsed/);
  await page.reload();
  await expect(page.locator(".sidebar")).toHaveClass(/collapsed/);
  await page.getByRole("button", { name: "Expand sidebar" }).click();
  await expect(page.locator(".sidebar")).not.toHaveClass(/collapsed/);
});

test("command palette jumps to projects and files", async ({ page }) => {
  await page.goto("/dashboard/projects");
  await expect(page.getByRole("heading", { name: "Your projects" })).toBeVisible();
  await page.keyboard.press("Control+k");
  await page.getByRole("dialog", { name: "Search" }).locator("input").fill("e2e/shop");
  await page.keyboard.press("Enter");
  await at(page, P);
  await expect(page.locator(".page-head h1")).toBeVisible();
  // `t`: file finder in the current project
  await page.keyboard.press("t");
  const dialog = page.getByRole("dialog", { name: "Search" });
  await dialog.locator("input").fill("cart");
  await expect(dialog.getByRole("option").first()).toContainText("src/cart.rs");
  await page.keyboard.press("Enter");
  await at(page, `${P}/-/blob/main/src/cart.rs`);
  await expect(page.locator(".code-line").first()).toBeVisible();
  // `?`: help
  await page.keyboard.press("?");
  await expect(page.getByRole("dialog", { name: "Keyboard shortcuts" })).toBeVisible();
  await page.keyboard.press("Escape").catch(() => {});
  await page.getByRole("button", { name: "Close" }).click();
});

test("tree keyboard navigation and hover prefetch", async ({ page }) => {
  await page.goto(P);
  await expect(page.locator(".tree-row").first()).toBeVisible();
  const requests: string[] = [];
  page.on("request", (r) => requests.push(r.url()));
  await page.locator(".tree-row a.tree-name", { hasText: "src" }).hover();
  await expect.poll(() => requests.some((u) => u.includes("/repository/tree") && u.includes("path=src"))).toBe(true);
  await page.mouse.move(0, 0);
  await page.locator("body").click({ position: { x: 5, y: 300 } }).catch(() => {});
  await page.keyboard.press("j");
  await expect(page.locator(".tree-row.selected")).toHaveCount(1);
  // walk to "src" and open it
  const names = await page.locator(".tree-row[data-row] .tree-name").allTextContents();
  const idx = names.indexOf("src");
  for (let i = 0; i < idx; i++) await page.keyboard.press("j");
  await page.keyboard.press("Enter");
  await at(page, `${P}/-/tree/main/src`);
});

test("line ranges with shift-click", async ({ page }) => {
  await page.goto(`${P}/-/blob/main/long.txt#L10`);
  await expect(page.locator("#L10")).toHaveClass(/target/);
  await page.locator("#L14 .ln a").click({ modifiers: ["Shift"] });
  await at(page, `${P}/-/blob/main/long.txt#L10-14`);
  await expect(page.locator(".code-line.target")).toHaveCount(5);
  await page.reload();
  await expect(page.locator(".code-line.target")).toHaveCount(5);
});

test("recent branch banner offers a merge request", async ({ page }) => {
  await page.goto(P);
  const banner = page.locator(".banner", { hasText: "fix/typo" });
  await expect(banner).toBeVisible();
  await banner.getByRole("link", { name: "Create merge request" }).click();
  await at(page, `${P}/-/merge_requests/new`);
  await expect(page.getByLabel(/^Source branch/)).toHaveText("fix/typo");
});

test("moving to another project forgets the previous project's branch", async ({ page }) => {
  await page.goto(`${P}/-/blob/${"feature/checkout"}/src/checkout.rs`);
  await expect(page.locator(".code-line").first()).toBeVisible();
  if (isMobile(page)) await page.getByLabel("Open navigation").click();
  await expect(page.locator(".sidebar").getByRole("link", { name: "Commits" })).toHaveAttribute("href", `${P}/-/commits/feature/checkout`);
  if (isMobile(page)) await page.locator(".scrim").click({ position: { x: 370, y: 200 } });
  await page.keyboard.press("Control+k");
  await page.getByRole("dialog", { name: "Search" }).locator("input").fill("e2e/blog");
  await expect(page.getByRole("dialog", { name: "Search" }).getByRole("option").first()).toContainText("e2e/blog");
  await page.keyboard.press("Enter");
  await at(page, "/e2e/blog");
  if (isMobile(page)) await page.getByLabel("Open navigation").click();
  await expect(page.locator(".sidebar").getByRole("link", { name: "Commits" })).toHaveAttribute("href", "/e2e/blog/-/commits/main");
});

test("Enter on a focused link is not taken by the tree shortcut", async ({ page }) => {
  await page.goto(P);
  await expect(page.locator(".tree-row").first()).toBeVisible();
  await page.keyboard.press("j"); // a tree row is now selected
  await page.locator(".list .item").first().getByRole("link", { name: "History" }).focus();
  await page.keyboard.press("Enter");
  await at(page, `${P}/-/commits/main`);
});

test("search issues and merge requests across projects", async ({ page }) => {
  await page.goto(P);
  await expect(page.locator(".page-head h1")).toBeVisible();
  await page.keyboard.press("Control+k");
  const dialog = page.getByRole("dialog", { name: "Search" });
  await dialog.locator("input").fill("discounts");
  await expect(dialog.getByRole("option", { name: /Checkout with discounts/ })).toBeVisible();
  await dialog.getByRole("option", { name: /Search everything for “discounts”/ }).click();
  await expect(page).toHaveURL(/\/search\?q=discounts/);
  await page.getByRole("button", { name: /Merge requests/ }).click();
  await expect(page.getByRole("link", { name: "Checkout with discounts" })).toBeVisible();
  await page.getByRole("button", { name: "Merged" }).click();
  await expect(page.getByText("Nothing matches")).toBeVisible();
});
