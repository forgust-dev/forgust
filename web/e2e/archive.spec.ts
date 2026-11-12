import { test, expect, freshProject } from "./fixtures";

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
