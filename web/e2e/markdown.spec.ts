import { test, expect, freshProject } from "./fixtures";

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
