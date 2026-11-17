import { test, expect, P, csrfHeaders } from "./fixtures";

// A job with environment: makes an environment, listed, shown on its own page and stopped there.

test("an environment from a pipeline shows, and stops", async ({ page }) => {
  const csrf = await csrfHeaders(page);
  const project = await (await page.request.get(`/api/v1/resolve?path=${P.slice(1)}`)).json();
  const n = Date.now() % 1_000_000;
  const branch = `e2e-env-${n}`;
  const ci = `deploy:\n  script: [deploy]\n  environment:\n    name: e2e-review/${n}\n    url: https://review-${n}.example.com\n`;
  const r = await page.request.post(`/api/v1/projects/${project.id}/repository/commits`, {
    headers: csrf,
    data: { branch, start_branch: "main", commit_message: "an environment", actions: [{ action: "update", file_path: ".gitlab-ci.yml", content: ci }] },
  });
  expect(r.ok(), await r.text()).toBeTruthy();
  try {
    await page.goto(`${P}/-/environments`);
    // in its folder (e2e-review/), open: it holds only a few
    const row = page.getByRole("region", { name: `Environment e2e-review/${n}` });
    await expect(row).toBeVisible();
    // no runner here: the deployment waits, so the URL is not known to work yet
    await expect(row.getByRole("link", { name: "Open" })).toHaveCount(0);
    // its page: the deployment from the branch
    await row.getByRole("link", { name: String(n), exact: true }).click();
    await expect(page.getByRole("heading", { name: `e2e-review/${n}`, level: 1 })).toBeVisible();
    await expect(page.getByRole("region", { name: "Deployments" }).locator(".item", { hasText: branch })).toBeVisible();
    // stopped there, once asked in the app's dialog (no stop job: at once)
    await page.getByRole("button", { name: "Stop", exact: true }).click();
    await expect(page.getByRole("alertdialog")).toContainText("no stop job");
    await page.getByRole("alertdialog").getByRole("button", { name: "Stop environment" }).click();
    await expect(page.locator(".mr-state", { hasText: "Stopped" })).toBeVisible();
    await page.goto(`${P}/-/environments/stopped`);
    await expect(page.getByRole("region", { name: `Environment e2e-review/${n}` })).toBeVisible();
  } finally {
    await page.request.delete(`/api/v1/projects/${project.id}/repository/branches/${branch}`, { headers: csrf });
  }
});
