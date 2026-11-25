import { test, expect, SEED } from "./fixtures";

// Repository settings > Mirroring: a push mirror is added (after a confirmation),
// shows how its updates go, and is removed. Its address is public: servers refuse
// private ones (this one, too) unless `features.imports_allow_local` is on.

test("a push mirror is added and removed in the repository settings", async ({ page }) => {
  const url = `https://example.com/forgust-e2e/mirror-${Date.now()}.git`;
  await page.goto(`/${SEED.group}/${SEED.other}/-/settings/repository#mirroring`);
  const section = page.locator("#mirroring");
  await expect(section.getByText("No mirrors.")).toBeVisible();
  // the list first: the form opens on asking
  await section.getByRole("button", { name: "Add a mirror" }).click();
  await section.getByRole("textbox", { name: /^Repository address/ }).fill(url);
  await section.getByRole("button", { name: "Add mirror" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Push mirror" }).click();
  const row = section.locator(".mirror-row");
  await expect(row).toContainText(url);
  await expect(row).toContainText("Push");
  // its first update reports (there is no repository there: it fails)
  await expect(row.getByText(/Updated|Failed/)).toBeVisible({ timeout: 15_000 });

  await row.getByRole("button", { name: "Pause" }).click();
  await expect(row.getByText("Paused")).toBeVisible();
  await row.getByRole("button", { name: "Remove" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Remove" }).click();
  await expect(section.getByText("No mirrors.")).toBeVisible();
});
