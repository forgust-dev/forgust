import { test, expect, P, csrfHeaders } from "./fixtures";

// Push rules in settings, GPG keys in the profile.

test("push rules are saved from settings, and a bad pattern is explained", async ({ page }) => {
  const csrf = await csrfHeaders(page);
  const project = await (await page.request.get(`/api/v1/resolve?path=${P.slice(1)}`)).json();
  try {
    await page.goto(`${P}/-/settings`);
    const field = page.getByLabel("Commit messages must match");
    await field.fill("(");
    await page.getByRole("button", { name: "Save push rules" }).click();
    await expect(page.locator(".alert, .error-box, [role=alert]").first()).toContainText("not a valid regular expression");
    await field.fill("^(feat|fix): ");
    await page.getByLabel("Largest file, in MB").fill("5");
    await page.getByRole("button", { name: "Save push rules" }).click();
    await expect(page.getByText("Saved.")).toBeVisible();
    const saved = await (await page.request.get(`/api/v1/projects/${project.id}/push_rule`)).json();
    expect(saved.commit_message_regex).toBe("^(feat|fix): ");
    expect(saved.max_file_size_mb).toBe(5);
  } finally {
    await page.request.put(`/api/v1/projects/${project.id}/push_rule`, { headers: csrf, data: {} });
  }
});

test("a GPG key that is not one is refused in the profile", async ({ page }) => {
  await page.goto("/profile/gpg_keys");
  await page.getByLabel("Public key").fill("hello");
  await page.getByRole("button", { name: "Add key" }).click();
  await expect(page.locator(".alert, .error-box, [role=alert]").first()).toContainText("not an armored OpenPGP public key");
  await expect(page.getByText("No GPG keys yet.")).toBeVisible();
});
