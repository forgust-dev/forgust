import { test, expect, P, at } from "./fixtures";

test("a confidential issue with an internal comment, then made visible to everyone", async ({ page }) => {
  // opened confidential with the form's switch
  await page.goto(`${P}/-/issues/new`);
  const title = `Secret ${Date.now()}`;
  await page.getByLabel("Title").fill(title);
  const create = page.locator(".switch-row", { hasText: "Confidential" }).getByRole("switch");
  await create.click();
  await expect(create).toHaveAttribute("aria-checked", "true");
  await page.getByRole("button", { name: "Create issue" }).click();
  await at(page, new RegExp(`^${P}/-/issues/\\d+$`));
  await expect(page.getByRole("heading", { name: title })).toBeVisible();
  await expect(page.locator(".mr-status .confidential-mark")).toBeVisible();
  const aside = page.getByRole("complementary", { name: "Issue details" });
  const toggle = aside.locator(".switch-row", { hasText: "Confidential" }).getByRole("switch");
  await expect(toggle).toHaveAttribute("aria-checked", "true");

  // an internal comment is marked as such
  await page.getByRole("checkbox", { name: "Internal comment" }).check();
  await page.locator(".composer textarea").fill("Only for the team");
  await page.getByRole("button", { name: "Comment", exact: true }).click();
  const note = page.locator(".thread.plain.internal", { hasText: "Only for the team" });
  await expect(note).toBeVisible();
  await expect(note.locator(".internal-mark")).toBeVisible();
  await expect(page.getByRole("checkbox", { name: "Internal comment" })).not.toBeChecked();

  // the list marks it
  await page.goto(`${P}/-/issues`);
  await page.getByPlaceholder("Filter by title or #number").fill(title);
  const row = page.locator(".issue-row", { hasText: title });
  await expect(row.locator(".confidential-mark")).toBeVisible();

  // visible to everyone again, from the sidebar
  await row.getByRole("link", { name: title }).click();
  await at(page, new RegExp(`^${P}/-/issues/\\d+$`));
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-checked", "false");
  await expect(page.locator(".mr-status .confidential-mark")).toHaveCount(0);
  await expect(page.locator(".tl-body", { hasText: "made the issue visible to everyone" })).toBeVisible();
  // the internal comment stays internal
  await expect(page.locator(".thread.plain.internal", { hasText: "Only for the team" })).toBeVisible();
});
