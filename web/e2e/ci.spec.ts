import { test, expect, P, at, isMobile } from "./fixtures";

test("pipelines list, graph and a job log with sections", async ({ page }) => {
  await page.goto(`${P}/-/pipelines`);
  const row = page.locator(".pipeline-row").filter({ hasText: "main" }).last();
  await expect(row).toBeVisible();
  await row.locator(".status-badge").click();
  await at(page, new RegExp(`^${P}/-/pipelines/\\d+$`));
  const graph = page.getByRole("list", { name: "Stages" });
  await expect(graph.locator(".job-card", { hasText: "build" })).toBeVisible();
  await expect(graph.locator(".job-card", { hasText: "build" }).getByRole("img", { name: "passed" })).toBeVisible();
  await expect(graph.locator(".job-card", { hasText: "test" }).getByRole("img", { name: "pending" })).toBeVisible();

  await graph.getByRole("link", { name: "build" }).click();
  await at(page, new RegExp(`^${P}/-/jobs/\\d+$`));
  await expect(page.locator(".log-header", { hasText: "Executing \"step_script\"" })).toBeVisible();
  await expect(page.locator(".log-line", { hasText: "Compiling shop" })).toBeVisible();
  // colors come through as styles, not escape codes
  await expect(page.locator(".log-body")).not.toContainText("[32;1m");
  await expect(page.locator(".log-header", { hasText: "Preparing environment" }).locator(".log-dur")).toHaveText("2s");
  // collapse a section
  await page.locator(".log-header", { hasText: "step_script" }).getByRole("button").click();
  await expect(page.locator(".log-line", { hasText: "Compiling shop" })).toHaveCount(0);
  await expect(page.locator(".log-line", { hasText: "Job succeeded" })).toBeVisible();
});

test("run a pipeline from the UI", async ({ page }) => {
  await page.goto(`${P}/-/pipelines/new`);
  await page.getByRole("button", { name: "Add variable" }).click();
  await page.getByLabel("Variable name").fill("E2E");
  await page.getByLabel("Variable value").fill("1");
  await page.getByRole("button", { name: "Run pipeline" }).click();
  await at(page, new RegExp(`^${P}/-/pipelines/\\d+$`));
  await expect(page.locator(".status-badge").first()).toContainText("pending");
  // cancel it again
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.locator(".status-badge").first()).toContainText("canceled");
});

test("CI/CD settings: runner token, variables", async ({ page }) => {
  await page.goto(`${P}/-/settings/ci_cd`);
  await expect(page.getByText("e2e fake runner")).toBeVisible();
  await page.getByRole("button", { name: "New runner" }).click();
  await page.getByPlaceholder("e.g. docker on build-01").fill("from the UI");
  await page.getByPlaceholder("docker, linux").fill("e2e");
  await page.getByRole("button", { name: "Create runner" }).click();
  await expect(page.getByText("Copy this token now", { exact: false })).toBeVisible();
  await expect(page.locator("pre code")).toContainText("gitlab-runner register");
  await expect(page.locator("pre code")).toContainText("glrt-");

  const key = `E2E_${Date.now() % 100000}`;
  await page.getByRole("button", { name: "Add variable" }).click();
  await page.getByLabel("Key", { exact: true }).fill(key);
  await page.getByLabel("Value", { exact: true }).fill("super-secret-value");
  await page.getByRole("checkbox", { name: /Masked/ }).check();
  await page.getByRole("button", { name: "Save", exact: true }).last().click();
  const item = page.locator(".item", { hasText: key });
  await expect(item).toContainText("masked");
  await expect(item).not.toContainText("super-secret-value");
  page.once("dialog", (d) => d.accept());
  await item.getByRole("button", { name: `Delete ${key}` }).click();
  await expect(page.locator(".item", { hasText: key })).toHaveCount(0);
});

test("commit list shows pipeline status", async ({ page }) => {
  await page.goto(`${P}/-/commits/main`);
  await expect(page.locator(".commit-row").first().locator(".st")).toBeVisible();
});

test("schedules", async ({ page }) => {
  test.skip(isMobile(page), "the form is the same on mobile");
  await page.goto(`${P}/-/pipeline_schedules`);
  await page.getByRole("button", { name: "New schedule" }).click();
  await page.getByPlaceholder("Nightly build").fill("Nightly e2e");
  await page.getByRole("button", { name: "Every weekday (03:00)" }).click();
  await page.getByRole("button", { name: "Save schedule" }).click();
  const row = page.locator(".item", { hasText: "Nightly e2e" }).first();
  await expect(row).toContainText("0 3 * * 1-5");
  page.once("dialog", (d) => d.accept());
  await row.getByRole("button", { name: "Delete schedule" }).click();
  await expect(page.locator(".item", { hasText: "Nightly e2e" })).toHaveCount(0);
});
