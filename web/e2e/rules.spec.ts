import { test, expect, P, csrfHeaders } from "./fixtures";

// Push rules in settings, GPG keys in the profile.

test("push rules are saved from settings, and a bad pattern is explained", async ({ page }) => {
  const csrf = await csrfHeaders(page);
  const project = await (await page.request.get(`/api/v1/resolve?path=${P.slice(1)}`)).json();
  try {
    await page.goto(`${P}/-/settings/repository`);
    // the patterns are folded while none is set
    await page.getByText(/^Advanced rules/).click();
    const field = page.getByLabel("Commit messages must match");
    await field.fill("(");
    await page.getByRole("button", { name: "Save push rules" }).click();
    await expect(page.locator(".alert, .error-box, [role=alert]").first()).toContainText("not a valid regular expression");
    await field.fill("^(feat|fix): ");
    await page.getByLabel("Largest file, in MB").fill("5");
    await page.getByRole("button", { name: "Save push rules" }).click();
    // "Saved" beside the button, which greys until the next change
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: "Save push rules" })).toBeDisabled();
    const saved = await (await page.request.get(`/api/v1/projects/${project.id}/push_rule`)).json();
    expect(saved.commit_message_regex).toBe("^(feat|fix): ");
    expect(saved.max_file_size_mb).toBe(5);
  } finally {
    await page.request.put(`/api/v1/projects/${project.id}/push_rule`, { headers: csrf, data: {} });
  }
});

test("a GPG key that is not one is refused in the profile", async ({ page }) => {
  await page.goto("/profile/gpg_keys");
  await page.getByRole("button", { name: "Add a GPG key" }).click();
  const window = page.getByRole("dialog", { name: "Add a GPG key" });
  await window.getByLabel("Public key").fill("hello");
  await window.getByRole("button", { name: "Add key" }).click();
  // refused in the window, which stays open with what was typed
  await expect(window.locator(".alert, .error-box, [role=alert]").first()).toContainText("not an armored OpenPGP public key");
  await expect(window.getByLabel("Public key")).toHaveValue("hello");
  await window.getByRole("button", { name: "Cancel" }).click();
  await expect(page.getByText("No GPG keys yet.")).toBeVisible();
});
