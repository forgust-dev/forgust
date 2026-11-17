import { test, expect, at, freshProject } from "./fixtures";

// An archived project is read-only until its Owner unarchives it.

test("archive a project, then unarchive it", async ({ page }) => {
  const { project, web, remove } = await freshProject(page, "arch", { "README.md": "# Archive me\n" });
  try {
    await page.goto(`${web}/-/settings`);
    page.once("dialog", (d) => d.accept());
    await page.getByRole("button", { name: "Archive project" }).click();
    await expect(page.getByRole("status").filter({ hasText: "This project is archived" })).toBeVisible();
    await expect(page.locator(".pill", { hasText: "Archived" })).toBeVisible();
    // writes are refused with the reason
    const r = await page.request.post(`/api/v1/projects/${project.id}/issues`, {
      headers: { "x-csrf-token": (await (await page.request.get("/api/v1/session")).json()).csrf_token },
      data: { title: "nope" },
    });
    expect(r.status()).toBe(403);
    expect(await r.text()).toContain("archived");

    await page.getByRole("button", { name: "Unarchive" }).click();
    await expect(page.getByText("This project is archived")).toHaveCount(0);
  } finally {
    await remove();
  }
});

test("an archived project's Owner renames it and unarchives it from its settings", async ({ page }) => {
  const { project, web, remove } = await freshProject(page, "arch-own", { "README.md": "# Archived\n" });
  try {
    await page.goto(`${web}/-/settings`);
    page.once("dialog", (d) => d.accept());
    await page.getByRole("button", { name: "Archive project" }).click();
    await expect(page.getByRole("status").filter({ hasText: "This project is archived" })).toBeVisible();

    // the general page only: the others hold settings that are read-only now
    await page.goto(`${web}/-/settings`);
    await expect(page.getByText(/^This project is archived: unarchive it to change its settings/)).toBeVisible();
    await expect(page.getByRole("button", { name: "Save" })).toHaveCount(0);
    // nothing on it is refused: no error from a section it cannot show
    await expect(page.locator(".alert.error")).toHaveCount(0);
    await page.goto(`${web}/-/settings/repository`);
    await expect(page.getByRole("button", { name: "Protect" })).toHaveCount(0);

    // renamed while archived: it stays archived at its new path
    await page.goto(`${web}/-/settings`);
    const renamed = `${project.path}-old`;
    await page.getByLabel("Change path").fill(renamed);
    await page.getByRole("button", { name: "Rename" }).click();
    await at(page, `${web}-old/-/settings`);
    await expect(page.getByRole("status").filter({ hasText: "This project is archived" })).toBeVisible();

    await page.getByRole("button", { name: "Unarchive project" }).click();
    await expect(page.getByText("This project is archived")).toHaveCount(0);
    // the whole settings page is back
    await expect(page.getByRole("button", { name: "Save" })).toBeVisible();
  } finally {
    await remove();
  }
});

test("cancelling archive or delete changes nothing and says nothing", async ({ page }) => {
  const { project, web, remove } = await freshProject(page, "keep", { "README.md": "# Keep me\n" });
  try {
    await page.goto(`${web}/-/settings`);
    page.once("dialog", (d) => d.dismiss());
    await page.getByRole("button", { name: "Archive project" }).click();
    page.once("dialog", (d) => d.dismiss());
    await page.getByRole("button", { name: "Delete project" }).click();
    await expect(page.getByText("Archived: the project is read-only.")).toHaveCount(0);
    await expect(page.getByText("Deleted.")).toHaveCount(0);
    await expect(page.getByText("This project is archived")).toHaveCount(0);
    expect((await page.request.get(`/api/v1/projects/${project.id}`)).ok()).toBe(true);
  } finally {
    await remove();
  }
});
