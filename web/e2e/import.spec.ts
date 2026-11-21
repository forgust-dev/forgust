import { test, expect } from "./fixtures";

// The import form: one project by default, several on request; each source says
// the oldest self-hosted version it imports from.

test("the import form picks its source and mode", async ({ page }) => {
  await page.goto("/import");
  const sources = page.getByRole("radiogroup", { name: "Source" });
  for (const name of ["GitLab", "GitHub", "Gitea", "Forgejo"]) await expect(sources.getByRole("radio", { name: new RegExp(name) })).toBeVisible();
  await expect(sources.getByRole("radio", { name: /GitHub/ })).toContainText(/\d+\.\d+\+ \(Enterprise\)/);
  await expect(page.getByText(/Self-hosted: GitLab \d+\.\d+ or later/)).toBeVisible();

  // one project by default: a project URL on another forge moves the address and source there
  const address = page.getByRole("textbox", { name: /^Address/ });
  await expect(address).toHaveValue("https://gitlab.com");
  await page.getByRole("textbox", { name: /^Project/ }).fill("https://codeberg.org/forgejo/forgejo/src/branch/forgejo");
  await expect(address).toHaveValue("https://codeberg.org");
  await expect(sources.getByRole("radio", { name: /Forgejo/ })).toHaveAttribute("aria-checked", "true");
  await expect(page.getByRole("button", { name: "Find the project" })).toBeEnabled();

  // several projects: no project field, a connect step instead
  await page.getByRole("radiogroup", { name: "What to import" }).getByRole("radio", { name: "Several projects" }).click();
  await expect(page.getByRole("textbox", { name: /^Project/ })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Connect" })).toBeEnabled();

  // picking a source starts from its public instance
  await sources.getByRole("radio", { name: /Gitea/ }).click();
  await expect(address).toHaveValue("https://gitea.com");
});
