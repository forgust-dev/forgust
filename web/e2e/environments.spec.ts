import { test, expect, P, csrfHeaders } from "./fixtures";

// A job with environment: makes an environment, listed and stopped here.

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
    const row = page.getByRole("region", { name: `Environment e2e-review/${n}` });
    await expect(row).toBeVisible();
    // no runner here: the deployment waits, so the URL is not known to work yet
    await expect(row.getByRole("link", { name: "Open" })).toHaveCount(0);
    await row.getByRole("button", { name: `e2e-review/${n}` }).click();
    await expect(row.locator(".item", { hasText: branch })).toBeVisible();
    page.once("dialog", (d) => d.accept());
    await row.getByRole("button", { name: "Stop" }).click();
    await expect(row).toHaveCount(0);
    await page.getByRole("link", { name: "Stopped" }).click();
    await expect(page.getByRole("region", { name: `Environment e2e-review/${n}` })).toBeVisible();
  } finally {
    await page.request.delete(`/api/v1/projects/${project.id}/repository/branches/${branch}`, { headers: csrf });
  }
});
