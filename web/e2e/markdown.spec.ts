import { test, expect, freshProject, isMobile } from "./fixtures";

// Alerts, math and Mermaid diagrams in Markdown (here a README).

const README = `# Docs

> [!WARNING]
> Mind the gap.

Euler: $e^{i\\pi} + 1 = 0$

\`\`\`math
\\sum_{k=1}^{n} k = \\frac{n(n+1)}{2}
\`\`\`

\`\`\`mermaid
graph LR
  push --> pipeline --> deploy
\`\`\`

\`\`\`mermaid
this is not a diagram
\`\`\`
`;

test("alerts, math and diagrams in Markdown", async ({ page }) => {
  const { web, remove } = await freshProject(page, "md", { "README.md": README });
  try {
    await page.goto(web);
    const md = page.locator(".markdown").first();
    await expect(md.locator(".markdown-alert-warning")).toContainText("Mind the gap.");
    await expect(md.locator(".markdown-alert-title")).toHaveText("Warning");
    // typeset by KaTeX, inline and as a block
    await expect(md.locator(".math-inline .katex")).toBeVisible();
    await expect(md.locator(".math-display .katex-display")).toBeVisible();
    // drawn by Mermaid; a broken one stays as code, marked
    await expect(md.locator(".mermaid-diagram svg")).toBeVisible();
    await expect(md.locator(".mermaid-diagram")).toContainText("pipeline");
    await expect(md.locator("pre.mermaid-error")).toContainText("this is not a diagram");
  } finally {
    await remove();
  }
});

// A README written for GitHub: its HTML (a centred logo, a picture for each theme, a fold, keys),
// links to its sections, code coloured with a button to copy it, its outline, a wide table.
const filler = "A line to scroll past.\n\n".repeat(30);
const GITHUB = `<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="logo-dark.png" />
    <img src="logo-light.png" alt="Shop logo" width="120" />
  </picture>
</p>

<img src="dark.png#gh-dark-mode-only" alt="For dark"><img src="light.png#gh-light-mode-only" alt="For light">

Jump to [the options](#options).

<details><summary>Why another shop?</summary>

Because **ours** is faster.

</details>

Press <kbd>Ctrl</kbd>+<kbd>C</kbd>. H<sub>2</sub>O.

${filler}
## Install

\`\`\`sh
pip install shop
\`\`\`

${filler}
## Use it

\`\`\`python
def total(cents: int) -> int:
    return cents * 2
\`\`\`

${filler}
## Options

| Option | Default |
| --- | --- |
| \`--${"averyveryverylongoptionname".repeat(8)}\` | none |
`;

test("a README written for GitHub", async ({ page }) => {
  await page.context().grantPermissions(["clipboard-read", "clipboard-write"]);
  const { web, remove } = await freshProject(page, "gh", { "README.md": GITHUB });
  try {
    await page.goto(web);
    const md = page.locator(".readme .markdown");
    // its HTML, as GitHub keeps it
    await expect(md.locator('p[align="center"] picture img')).toHaveAttribute("src", `${web}/-/raw/main/logo-light.png`);
    await expect(md.locator("picture source")).toHaveAttribute("srcset", `${web}/-/raw/main/logo-dark.png`);
    await expect(md.locator("kbd")).toHaveText(["Ctrl", "C"]);
    await expect(md.getByText("Because ours is faster.")).toBeHidden();
    await md.locator("summary").click();
    await expect(md.getByText("Because ours is faster.")).toBeVisible();
    // an image for each theme: the system's (light here), then the one picked here
    await expect(md.getByAltText("For light")).toBeVisible();
    await expect(md.getByAltText("For dark")).toBeHidden();
    await page.evaluate(() => (document.documentElement.dataset.theme = "dark"));
    await expect(md.getByAltText("For dark")).toBeVisible();
    await expect(md.getByAltText("For light")).toBeHidden();
    await expect(md.locator("picture source")).toHaveAttribute("media", "all");
    await page.evaluate(() => delete document.documentElement.dataset.theme);
    await expect(md.locator("picture source")).toHaveAttribute("media", "(prefers-color-scheme: dark)");
    // code coloured as files are, with a button to copy it
    const def = md.locator("code.language-python span", { hasText: /^def$/ }).first();
    const [word, text] = await def.evaluate((s) => [getComputedStyle(s).color, getComputedStyle(s.closest("pre")!).color]);
    expect(word).not.toBe(text);
    const block = md.locator(".code-block", { hasText: "pip install shop" });
    await block.hover();
    await block.getByRole("button", { name: "Copy the code" }).click();
    await expect(block.getByRole("button", { name: "Copied" })).toBeVisible();
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe("pip install shop");
    // a link to a section finds it
    await md.getByRole("link", { name: "the options" }).click();
    await expect(page).toHaveURL(/#options$/);
    await expect(md.locator("#user-content-options")).toBeInViewport();
    // a wide table scrolls on its own, the page does not
    expect(await md.locator("table").evaluate((t) => t.scrollWidth > t.clientWidth)).toBe(true);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
    if (!isMobile(page)) {
      // a heading's own link, shown on hover: its address, copied
      const install = md.locator("#user-content-install");
      await install.hover();
      await install.getByRole("link", { name: "Link to heading 'Install'" }).click();
      await expect(page.getByText("Link to “Install” copied")).toBeVisible();
      expect(await page.evaluate(() => navigator.clipboard.readText())).toMatch(/#install$/);
    }
    // the outline goes to a section
    await page.getByRole("button", { name: "Outline" }).click();
    await page.getByRole("menuitem", { name: "Use it" }).click();
    await expect(page).toHaveURL(/#use-it$/);
    await expect(md.locator("#user-content-use-it")).toBeInViewport();
    // the README opens in the editor
    await expect(page.getByRole("link", { name: "Edit this file" })).toHaveAttribute("href", `${web}/-/edit/main/README.md`);
    // a page opened at a section shows it
    await page.goto("about:blank");
    await page.goto(`${web}#options`);
    await expect(md.locator("#user-content-options")).toBeInViewport();
  } finally {
    await remove();
  }
});
