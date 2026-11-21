import { test, expect, at, freshProject } from "./fixtures";

// Code search: text inside the files of a project's default branch, indexed in
// the background after a push.

test("code is found from the repository page, the search page and the palette", async ({ page }) => {
  const word = `needle${Date.now() % 1_000_000}`;
  const { web, remove } = await freshProject(page, "code", {
    "README.md": "# Code\n",
    "app.py": `def run():\n    return "${word}"\n`,
    "Notes.txt": `first line\n${word.toUpperCase()} in capitals\n`,
  });
  try {
    // the push is indexed a moment later
    await expect
      .poll(async () => (await (await page.request.get(`/api/v1/search/code?q=${word}`)).json()).files.length, { timeout: 30_000 })
      .toBe(2);

    // from the repository page: this project's code
    await page.goto(web);
    await page.getByRole("searchbox", { name: "Search this project's code" }).fill(word);
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/scope=code/);
    const python = page.getByRole("region", { name: /app\.py/ });
    const notes = page.getByRole("region", { name: /Notes\.txt/ });
    await expect(python.locator("mark")).toHaveText(word);
    await expect(python.getByText("Python")).toBeVisible();
    await expect(notes.locator("mark")).toHaveText(word.toUpperCase());
    await expect(page.getByText(/^in e2e\/code-/)).toBeVisible();

    // exact case, then a regular expression, then a language
    await page.getByRole("button", { name: "Match case" }).click();
    await expect(notes).toHaveCount(0);
    await expect(python).toBeVisible();
    await page.getByRole("button", { name: "Match case" }).click();
    await expect(notes).toBeVisible();
    await page.getByRole("button", { name: "Regular expression" }).click();
    await page.getByRole("searchbox", { name: "Search…" }).fill(`return "${word}"$`);
    await expect(notes).toHaveCount(0);
    // a match that spans syntax colours is marked piece by piece
    await expect.poll(async () => (await python.locator("mark").allTextContents()).join("")).toBe(`return "${word}"`);
    // inside one project, a pattern with no plain text to look up still runs
    await page.getByRole("searchbox", { name: "Search…" }).fill("^# \\w+$");
    await expect.poll(async () => (await page.getByRole("region", { name: /README\.md/ }).locator("mark").allTextContents()).join("")).toBe("# Code");
    await expect(python).toHaveCount(0);
    await page.getByRole("button", { name: "Regular expression" }).click();
    await page.getByRole("searchbox", { name: "Search…" }).fill(word);
    // by file name
    await page.getByRole("searchbox", { name: "File name" }).fill("notes.txt");
    await expect(python).toHaveCount(0);
    await expect(notes).toBeVisible();
    await page.getByRole("searchbox", { name: "File name" }).fill("");
    await expect(python).toBeVisible();
    // the language list has a filter: typing narrows it, Enter takes the first
    await page.getByRole("combobox", { name: "Language", exact: true }).click();
    await page.getByRole("combobox", { name: "Filter: Language" }).fill("typ");
    await expect(page.getByRole("option")).toHaveText(["TypeScript"]);
    await page.getByRole("combobox", { name: "Filter: Language" }).fill("pyt");
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/lang=python/);
    await expect(page.getByRole("combobox", { name: "Language", exact: true })).toHaveText("Python");
    await expect(notes).toHaveCount(0);
    await expect(python).toBeVisible();

    // a line number opens the file at that line
    await python.getByRole("link", { name: "2", exact: true }).click();
    await at(page, `${web}/-/blob/main/app.py#L2`);
    await expect(page.locator(".code-line.target")).toContainText(word);

    // from the palette: every project
    await page.keyboard.press("Control+k");
    const dialog = page.getByRole("dialog", { name: "Search" });
    await dialog.locator("input").fill(word);
    await dialog.getByRole("option", { name: /Search all code for/ }).click();
    await expect(page).toHaveURL(/scope=code/);
    await expect(page.getByRole("region", { name: /Notes\.txt/ })).toBeVisible();
    // the other tabs still search titles
    await page.getByRole("button", { name: /^Projects/ }).click();
    await expect(page.getByText(`Nothing has “${word}” in its title.`)).toBeVisible();
  } finally {
    await remove();
  }
});
