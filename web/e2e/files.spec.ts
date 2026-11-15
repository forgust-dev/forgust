import { test, expect, freshProject } from "./fixtures";

// File and branch names are names, not URL syntax: a `#`, `?`, `%` or space in
// them is encoded in links, and an address with a malformed escape shows a page.

test("a file named with # and % opens from the tree", async ({ page }) => {
  const { web, remove } = await freshProject(page, "names", { "100% #1.md": "# Odd name\n", "README.md": "# Names\n" });
  try {
    await page.goto(web);
    await page.getByRole("link", { name: "100% #1.md" }).click();
    await expect.poll(() => page.evaluate(() => location.pathname)).toContain("100%25%20%231.md");
    await expect(page.getByRole("heading", { name: "Odd name" })).toBeVisible();
    // a `%` typed as is in the address: a page (not found), not a blank window
    await page.goto(`${web}/-/blob/main/100%.md`);
    await expect(page.locator("#root")).not.toBeEmpty();
  } finally {
    await remove();
  }
});
