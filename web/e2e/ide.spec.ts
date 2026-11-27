import { test, expect, P, csrfHeaders, freshProject, isMobile } from "./fixtures";
import type { Page } from "@playwright/test";

/** How many changed files this browser keeps for the project's `main`. */
const kept = (page: Page) =>
  page.evaluate(
    () =>
      new Promise<number>((resolve) => {
        const open = indexedDB.open("forgust-ide");
        open.onerror = () => resolve(-1);
        open.onsuccess = () => {
          const db = open.result;
          if (!db.objectStoreNames.contains("drafts")) return resolve(0);
          const all = db.transaction("drafts").objectStore("drafts").getAll();
          all.onsuccess = () => resolve(all.result.reduce((n: number, s: { drafts: object }) => n + Object.keys(s.drafts).length, 0));
          all.onerror = () => resolve(-1);
        };
      }),
  );

/** The IDE's parts, wherever the screen's width puts them: on a narrow one the side panel and the editor take turns. */
function workbench(page: Page) {
  const toggle = page.getByRole("button", { name: "Files and changes" });
  const side = page.getByRole("complementary", { name: "Files and changes" });
  return {
    bar: page.locator(".ide-status-bar"),
    dialog: page.getByRole("alertdialog"),
    item: (name: string | RegExp) => page.getByRole("tree", { name: "Files" }).getByRole("treeitem", { name }),
    text: (path: string) => page.getByRole("textbox", { name: `Content of ${path}` }),
    changed: page.getByRole("list", { name: "Changed files" }).getByRole("listitem"),
    // After a commit or an update the workbench starts over (a loader, then the IDE again,
    // its side panel as it was): what is on screen is looked at again until it has settled.
    async panel(name: "Files" | "Changes") {
      await expect(async () => {
        await expect(page.locator(".ide")).toBeVisible();
        if (isMobile(page) && !(await side.isVisible())) await toggle.click({ timeout: 1000 });
        await side.getByRole("tab", { name: new RegExp(`^${name}`) }).click({ timeout: 1000 });
      }).toPass();
    },
    async editor() {
      await expect(async () => {
        await expect(page.locator(".ide")).toBeVisible();
        if (!isMobile(page)) return;
        if (await side.isVisible()) await toggle.click({ timeout: 1000 });
        await expect(side).toBeHidden({ timeout: 1000 });
      }).toPass();
    },
  };
}

const raw = (page: Page, projectId: number, ref: string, path: string) => page.request.get(`/api/v1/projects/${projectId}/repository/raw?ref=${ref}&path=${encodeURIComponent(path)}`);

test("the web IDE: files in tabs, edited, created, renamed and deleted, all still there after a reload", async ({ page }) => {
  const ide = workbench(page);
  const { item, bar, dialog } = ide;
  const files = () => ide.panel("Files");
  const editor = () => ide.editor();

  // from the sidebar's Code section
  await page.goto(P);
  if (isMobile(page)) await page.getByLabel("Open navigation").click();
  await page.locator(".sidebar").getByRole("link", { name: "Web IDE" }).click();
  await expect(page).toHaveURL(/\/-\/ide\/main$/);
  await expect(bar).toContainText("No changes");
  await expect(item("README.md")).toBeVisible();

  // a file in a folder, edited
  await item("src").click();
  await item("cart.rs").click();
  await expect(page).toHaveURL(/\/-\/ide\/main\/src\/cart\.rs$/);
  const cart = page.getByRole("textbox", { name: "Content of src/cart.rs" });
  await expect(cart).toContainText("pub struct Cart");
  await expect(page.locator(".ide-pane[data-active] .tok-keyword").first()).toBeVisible();
  await cart.press("ControlOrMeta+End");
  await page.keyboard.type("// edited");
  await expect(bar).toContainText("1 changed file");
  await files();
  await expect(item("cart.rs, modified")).toBeVisible();

  // a second file: its own tab, the first one keeps its text
  await item("long.txt").click();
  await expect(page.getByRole("tab", { name: "long.txt" })).toHaveAttribute("aria-selected", "true");
  await expect(page.getByRole("tablist", { name: "Open files" }).getByRole("tab")).toHaveCount(2);
  await page.getByRole("tab", { name: "cart.rs" }).click();
  await expect(cart).toContainText("// edited");

  // a new file in a new folder
  await files();
  await page.getByRole("button", { name: "New file", exact: true }).click();
  await dialog.getByLabel("File path").fill("notes/todo.md");
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(page).toHaveURL(/\/-\/ide\/main\/notes\/todo\.md$/);
  await page.getByRole("textbox", { name: "Content of notes/todo.md" }).fill("- one");
  // a name already taken is refused
  await files();
  await page.getByRole("button", { name: "New file", exact: true }).click();
  await dialog.getByLabel("File path").fill("README.md");
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(page.locator(".toast").last()).toContainText("README.md already exists");

  // renamed from the keyboard, its tab and the address follow
  await item("todo.md, new").press("F2");
  await dialog.getByLabel("File path").fill("notes/plan.md");
  await dialog.getByRole("button", { name: "Rename" }).click();
  await expect(page).toHaveURL(/\/-\/ide\/main\/notes\/plan\.md$/);
  await editor();
  await expect(page.getByRole("textbox", { name: "Content of notes/plan.md" })).toContainText("- one");

  // deleting a file with typed text in it asks first, as discarding does
  await files();
  await item("plan.md, new").press("Delete");
  await expect(dialog).toContainText("Delete plan.md?");
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(item("plan.md, new")).toBeVisible();

  // a file of the commit, deleted: struck out, to restore
  await files();
  await item("README.md").press("Delete");
  await expect(item("README.md, deleted")).toBeVisible();
  await expect(bar).toContainText("3 changed files");

  // all of it is kept in the browser
  await expect.poll(() => kept(page)).toBe(3);
  await page.reload();
  await expect(bar).toContainText("3 changed files");
  await expect(page.getByRole("textbox", { name: "Content of notes/plan.md" })).toContainText("- one");
  await files();
  await expect(item("README.md, deleted")).toBeVisible();
  await item("src").click();
  await item("cart.rs, modified").click();
  await expect(cart).toContainText("// edited");

  // and dropped on request
  await bar.getByRole("button", { name: "Discard all" }).click();
  await dialog.getByRole("button", { name: "Discard all" }).click();
  await expect(bar).toContainText("No changes");
  await expect(cart).not.toContainText("// edited");
  await expect.poll(() => kept(page)).toBe(0);
});

test("the web IDE commits several files to a new branch, each a click from what changed in it", async ({ page }) => {
  const project = await (await page.request.get(`/api/v1/resolve?path=${P.slice(1)}`)).json();
  const csrf = await csrfHeaders(page);
  const branch = `e2e-ide-${Date.now() % 100000}`;
  const ide = workbench(page);
  try {
    await page.goto(`${P}/-/ide/main/src/lib.rs`);
    await expect(ide.text("src/lib.rs")).toContainText("pub mod cart;");
    await ide.text("src/lib.rs").press("ControlOrMeta+End");
    await page.keyboard.type("pub mod extra;");
    await ide.panel("Files");
    await page.getByRole("button", { name: "New file", exact: true }).click();
    await ide.dialog.getByLabel("File path").fill("src/extra.rs");
    await ide.dialog.getByRole("button", { name: "Create" }).click();
    await ide.text("src/extra.rs").fill("pub fn extra() {}");

    // the changes, and what one of them is
    await ide.panel("Changes");
    await expect(ide.changed).toHaveCount(2);
    await ide.changed.filter({ hasText: "lib.rs" }).getByRole("button").first().click();
    await expect(page.getByRole("button", { name: "Show changes" })).toHaveAttribute("aria-pressed", "true");
    await expect(page.locator(".ide-pane[data-active] .cm-changedLine").first()).toContainText("pub mod extra;");

    // committed together, on a new branch: on to its merge request
    await ide.panel("Changes");
    await page.getByLabel("Commit message").fill("Add the extra module");
    await page.getByRole("radio", { name: /new branch and open a merge request/ }).check();
    await page.getByRole("textbox", { name: "New branch name" }).fill(branch);
    await page.getByRole("button", { name: "Commit 2 files" }).click();
    await expect(page.getByRole("heading", { name: "New merge request" })).toBeVisible();
    await expect(page).toHaveURL(new RegExp(`source=${branch}`));
    expect(await (await raw(page, project.id, branch, "src/lib.rs")).text()).toContain("pub mod extra;");
    expect(await (await raw(page, project.id, branch, "src/extra.rs")).text()).toBe("pub fn extra() {}");
    expect((await raw(page, project.id, "main", "src/extra.rs")).status()).toBe(404);
    await expect.poll(() => kept(page)).toBe(0);
  } finally {
    await page.request.delete(`/api/v1/projects/${project.id}/repository/branches/${branch}`, { headers: csrf });
  }
});

test("the web IDE commits to the branch, and brings its changes onto commits made meanwhile", async ({ page }) => {
  const { project, web, api, remove } = await freshProject(page, "ide", { "a.txt": "alpha\n", "b.txt": "beta\n", "c.txt": "gamma\n" });
  const ide = workbench(page);
  try {
    // an edit, a move and a deletion, in one commit
    await page.goto(`${web}/-/ide/main/a.txt`);
    await expect(ide.text("a.txt")).toContainText("alpha");
    await ide.text("a.txt").press("ControlOrMeta+End");
    await page.keyboard.type("A2");
    await ide.panel("Files");
    await ide.item("b.txt").press("F2");
    await ide.dialog.getByLabel("File path").fill("docs/b.txt");
    await ide.dialog.getByRole("button", { name: "Rename" }).click();
    await ide.item("c.txt").press("Delete");
    await ide.panel("Changes");
    await expect(page.getByLabel("Commit message")).toHaveValue("Update 3 files");
    await page.getByRole("button", { name: "Commit 3 files" }).click();
    await expect(page.locator(".toast.ok").last()).toContainText("Committed to main");
    await expect(ide.bar).toContainText("No changes");
    expect(await (await raw(page, project.id, "main", "a.txt")).text()).toBe("alpha\nA2");
    expect(await (await raw(page, project.id, "main", "docs/b.txt")).text()).toBe("beta\n");
    expect((await raw(page, project.id, "main", "b.txt")).status()).toBe(404);
    expect((await raw(page, project.id, "main", "c.txt")).status()).toBe(404);

    // someone else commits to a.txt while two files are edited here
    await ide.editor();
    await ide.text("a.txt").press("ControlOrMeta+End");
    await page.keyboard.type(" mine");
    await ide.panel("Files");
    await ide.item("docs").click();
    await ide.item("b.txt").click();
    await ide.text("docs/b.txt").press("ControlOrMeta+End");
    await page.keyboard.type("B2");
    await api("post", `/projects/${project.id}/repository/commits`, { branch: "main", commit_message: "Elsewhere", actions: [{ action: "update", file_path: "a.txt", content: "upstream\n" }] });

    // the commit is refused, nothing is overwritten, and the way out is offered
    await ide.panel("Changes");
    await page.getByRole("button", { name: "Commit 2 files" }).click();
    await expect(page.locator(".ide-commit .alert.error")).toContainText("changed since you opened it");
    await expect(page.locator(".ide-commit .banner")).toContainText("has new commits");
    await expect(page.getByRole("radio", { name: /Commit directly/ })).toBeDisabled();
    await page.getByRole("button", { name: /^Update to/ }).click();
    // a.txt changed on both sides: its change cannot follow
    await expect(ide.dialog).toContainText("a.txt");
    await expect(ide.dialog).not.toContainText("docs/b.txt");
    await ide.dialog.getByRole("button", { name: "Drop it and update" }).click();
    await expect(page.locator(".toast.ok").last()).toContainText("Updated to");

    // what is left lands on the new head
    await ide.panel("Changes");
    await expect(ide.changed).toHaveCount(1);
    await page.getByRole("button", { name: "Commit 1 file" }).click();
    await expect(page.locator(".toast.ok").last()).toContainText("Committed to main");
    await expect(ide.bar).toContainText("No changes");
    expect(await (await raw(page, project.id, "main", "a.txt")).text()).toBe("upstream\n");
    expect(await (await raw(page, project.id, "main", "docs/b.txt")).text()).toBe("beta\nB2");

    // the editor's own keys stay its own: deleting a line does not open the search
    await ide.editor();
    await ide.text("docs/b.txt").press("ControlOrMeta+Shift+k");
    await expect(ide.bar).toContainText("1 changed file");
    await expect(page.getByRole("dialog", { name: "Search" })).toHaveCount(0);
  } finally {
    await remove();
  }
});

test("the web IDE finds a file by name, previews Markdown, and takes Vim's keys on request", async ({ page }) => {
  const ide = workbench(page);
  await page.goto(`${P}/-/ide/main/README.md`);
  const readme = ide.text("README.md");
  await expect(readme).toContainText("A fixture project");

  // Markdown as it will read, the text still there behind it
  await readme.press("ControlOrMeta+End");
  await page.keyboard.type("## Added here");
  await page.getByRole("button", { name: "Preview" }).click();
  await expect(page.locator(".ide-preview").getByRole("heading", { name: "Added here" })).toBeVisible();
  await expect(readme).toBeHidden();
  await page.getByRole("button", { name: "Preview" }).click();
  await expect(readme).toContainText("## Added here");

  // a file found by its name, from the editor: it opens here, not in the repository's pages
  await readme.press("ControlOrMeta+p");
  const finder = page.getByRole("dialog", { name: "Search" });
  await finder.locator("input").fill("app.ts");
  await expect(finder.getByRole("option").first()).toContainText("web/app.ts");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\/-\/ide\/main\/web\/app\.ts$/);
  const app = ide.text("web/app.ts");
  await expect(app).toContainText("export function greet");
  // the preview is Markdown's alone
  await expect(page.getByRole("button", { name: "Preview" })).toHaveCount(0);

  // Vim: keys are commands until a text is being inserted
  await ide.bar.getByRole("button", { name: "Vim" }).click();
  await expect(page.locator(".ide-pane[data-active] .cm-vim-panel")).toBeVisible();
  await app.press("g");
  await page.keyboard.type("gdd");
  await expect(app).not.toContainText("export function greet");
  await page.keyboard.type("O// by vim");
  await page.keyboard.press("Escape");
  await expect(app).toContainText("// by vim");
  // and off again: a letter is a letter
  await ide.bar.getByRole("button", { name: "Vim" }).click();
  await expect(page.locator(".ide-pane[data-active] .cm-vim-panel")).toHaveCount(0);
  await app.press("ControlOrMeta+End");
  await page.keyboard.type("dd");
  await expect(app).toContainText("dd");
  await expect(ide.bar).toContainText("2 changed files");
});
