import { test, expect, freshProject } from "./fixtures";

// A Maintainer turns dependency updates on; Forgust adds its Renovate
// bot to the project and runs Renovate as a pipeline of its own.

test("dependency updates: turn on, run now, turn off", async ({ page }) => {
  const { project, web, remove } = await freshProject(page, "deps", { "package.json": '{ "dependencies": { "left-pad": "1.0.0" } }\n' });
  try {
    await page.goto(`${web}/-/settings/automation`);
    const section = page.locator("section.settings-section", { has: page.getByRole("heading", { name: "Dependency updates" }) });
    await section.getByRole("checkbox", { name: /update dependencies/ }).check();
    await section.getByRole("button", { name: "Save", exact: true }).click();
    await expect(section.getByText("the first run starts within a minute")).toBeVisible();
    await expect(section.getByText(`@renovate-bot-${project.id}`)).toBeVisible();
    await section.getByRole("button", { name: "Run now" }).click();
    await expect(section.getByText(/Started pipeline #\d+/)).toBeVisible();
    // no runner here: the run waits; its pipeline is linked
    await expect(section.locator("a.status-badge")).toBeVisible();
    await page.goto(`${web}/-/members`);
    await expect(page.getByText(`renovate-bot-${project.id}`)).toBeVisible();

    // a private registry: the password is never shown again
    await page.goto(`${web}/-/settings/automation`);
    await section.getByLabel("Host").fill("npm.example.com");
    await section.getByLabel("Type").fill("npm");
    await section.getByLabel("Password or token").fill("e2e-npm-token");
    await section.getByRole("button", { name: "Add registry" }).click();
    await expect(section.locator(".item", { hasText: "npm.example.com" })).toBeVisible();
    await expect(page.getByText("e2e-npm-token")).toHaveCount(0);
    await section.getByRole("button", { name: "Delete npm.example.com" }).click();
    await expect(section.locator(".item", { hasText: "npm.example.com" })).toHaveCount(0);

    await section.getByRole("checkbox", { name: /update dependencies/ }).uncheck();
    await section.getByRole("button", { name: "Save", exact: true }).click();
    await expect(section.getByText("Dependency updates are off.")).toBeVisible();
  } finally {
    await remove();
  }
});

test("dependency updates: the administrator's settings", async ({ page }) => {
  await page.goto("/admin/dependency-updates");
  await expect(page.getByLabel("Renovate image")).toHaveValue(/renovate\/renovate:/);
  await page.getByLabel("Runs at once").fill("3");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved.")).toBeVisible();
  await page.getByLabel("Runs at once").fill("2");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved.")).toBeVisible();
});
