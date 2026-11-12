import { test, expect, P, isMobile } from "./fixtures";

// The administrator's views of the instance.

test("admin: overview, storage, jobs and health", async ({ page }) => {
  await page.goto("/admin");
  await expect(page.locator(".stat-label", { hasText: /^Projects$/ })).toBeVisible();
  await expect(page.getByLabel(/Pipelines per day/)).toBeVisible();

  await page.getByRole("link", { name: "Storage" }).click();
  await expect(page.locator(".stat-label", { hasText: /^Git repositories$/ })).toBeVisible();
  // every project is listed (the seed's among them)
  await expect(page.getByRole("link", { name: P.slice(1), exact: true }).first()).toBeVisible();
  await page.getByRole("button", { name: /^(Scan|Scan again)$/ }).click();
  await expect(page.getByText(/^Scanned /)).toBeVisible({ timeout: 20_000 });
  // with what nothing references (a report)
  await expect(page.getByRole("heading", { name: "Not referenced" })).toBeVisible();

  await page.goto("/admin/projects");
  await page.getByRole("radio", { name: "Size" }).click();
  await expect(page.getByRole("link", { name: P.slice(1), exact: true })).toBeVisible();
  await page.getByRole("radio", { name: "Groups" }).click();
  await expect(page.getByRole("table").getByRole("link", { name: P.slice(1).split("/")[0], exact: true })).toBeVisible();

  await page.getByRole("link", { name: "Jobs" }).click();
  await expect(page.getByText("Failed jobs")).toBeVisible();

  await page.getByRole("link", { name: "Health", exact: true }).first().click();
  for (const name of ["Database", "Blob store", "Git"]) {
    await expect(page.locator(".item", { hasText: name }).getByLabel("OK")).toBeVisible();
  }
});

test("admin: a project's storage quota", async ({ page }) => {
  await page.goto("/admin/storage");
  const edit = page.getByRole("button", { name: `Edit Quota of ${P.slice(1)}`, exact: true });
  await edit.click();
  const field = page.getByLabel(`Quota of ${P.slice(1)} (GB)`);
  const save = page.locator("form", { has: field }).getByRole("button", { name: "Save", exact: true });
  await field.fill("1000");
  await save.click();
  await expect(edit).toContainText("1000 GB");
  try {
    await page.goto(`${P}/-/settings`);
    await expect(page.getByRole("meter", { name: "This project's quota" })).toBeVisible();
  } finally {
    await page.goto("/admin/storage");
    await edit.click();
    await field.fill("");
    await save.click();
    await expect(edit).toHaveText("—");
  }
});

test("admin: people", async ({ page }) => {
  await page.goto("/admin/users");
  await page.getByRole("button", { name: "Admins", exact: true }).click();
  await expect(page.locator(".admin-user").first()).toContainText("admin");
  await page.locator(".admin-user").first().getByRole("button", { name: /^Actions for / }).click();
  await expect(page.getByRole("menuitem", { name: "Remove admin rights" })).toBeVisible();
  await page.keyboard.press("Escape");
});

// The banner is instance-wide: one run sets it (the mobile run would race this one).
test("admin: the banner", async ({ page }) => {
  test.skip(isMobile(page), "instance-wide setting: desktop run only");
  await page.goto("/admin/settings");
  const text = `Maintenance window ${Date.now() % 100000}`;
  await page.getByLabel("Message").fill(text);
  await page.getByRole("button", { name: "Save" }).last().click();
  await expect(page.getByText("Banner saved")).toBeVisible();
  await page.goto("/");
  await expect(page.getByRole("status").filter({ hasText: text })).toBeVisible();
  await page.goto("/admin/settings");
  await page.getByRole("button", { name: "Remove the banner" }).click();
  await expect(page.getByText("Banner saved")).toBeVisible();
  await page.goto("/");
  await expect(page.getByText(text)).toHaveCount(0);
});
