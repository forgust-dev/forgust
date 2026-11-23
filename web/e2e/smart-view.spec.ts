import { test, expect, freshProject, isMobile } from "./fixtures";

// The Smart View: a project's code as blocks, what each needs, and who to ask.

test("the Smart View maps the code into blocks and tells what each needs", async ({ page }) => {
  const { project, web, api, remove } = await freshProject(page, "map", {
    "Cargo.toml": '[workspace]\nmembers = ["api", "core"]\n',
    Dockerfile: "FROM rust\nCOPY . .\nRUN cargo build --release -p api\n",
    "compose.yml": "services:\n  app:\n    build: .\n    depends_on: [db]\n  db:\n    image: postgres:17\n",
    "README.md": "# Map\n",
  });
  try {
    // the crates, in folders: by the commits API
    const crate = (name: string, deps = "") => `[package]\nname = "${name}"\nversion = "0.1.0"\n${deps}`;
    await api("post", `/projects/${project.id}/repository/commits`, {
      branch: "main",
      commit_message: "Add the crates",
      actions: [
        { action: "create", file_path: "api/Cargo.toml", content: crate("api", 'description = "Serves the shop"\n\n[dependencies]\naxum = "0.8"\ncore = { path = "../core" }\n') },
        { action: "create", file_path: "api/src/main.rs", content: "fn main() {}\n" },
        { action: "create", file_path: "core/Cargo.toml", content: crate("core") },
        { action: "create", file_path: "core/src/lib.rs", content: "pub fn f() {}\n" },
      ],
    });

    await page.goto(`${web}/-/smart_view`);
    await expect(page.getByRole("heading", { name: "Smart View" })).toBeVisible();
    // compose.yml declares the services but holds no block's files
    await expect(page.getByText("5 blocks · 4 links · 3 files in no block")).toBeVisible();
    const card = (id: string) => page.locator(`.smart-card[data-id="${id}"]`);
    for (const id of ["app", "db", "image", "api", "core"]) await expect(card(id)).toBeVisible();
    await expect(card("api")).toContainText("Rust crate · API");
    await expect(card("api")).toContainText("Serves the shop");
    await expect(card("db")).toContainText("postgres:17");
    // what each is at a glance: its technology's logo, named on hover
    await expect(card("api").locator(".smart-mark title")).toHaveText("Rust");
    await expect(card("db").locator(".smart-mark title")).toHaveText("PostgreSQL");
    if (!isMobile(page)) await expect(page.getByRole("link", { name: "Smart View" }).first()).toBeVisible();

    // a block chosen: what it needs and what needs it, with the file that says so
    await card("api").click();
    await expect(page).toHaveURL(/[?&]block=api\b/);
    const panel = page.getByRole("complementary", { name: "api, details" });
    await expect(panel.getByRole("heading", { name: "api" })).toBeVisible();
    await expect(panel.locator("li", { hasText: "uses core" })).toContainText("Cargo.toml");
    await expect(panel.locator("li", { hasText: "image builds it" })).toContainText("Dockerfile");
    await expect(panel.getByRole("link", { name: "api/Cargo.toml" })).toHaveAttribute("href", `${web}/-/blob/main/api/Cargo.toml`);

    // from one block to the next: the image, and who made it
    await panel.getByRole("button", { name: "image" }).click();
    const image = page.getByRole("complementary", { name: "image, details" });
    await expect(image.locator("li", { hasText: "app runs it" })).toContainText("compose.yml");
    await expect(image.getByText("E2E", { exact: true })).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.locator(".smart-panel")).toHaveCount(0);
    await expect(page).not.toHaveURL(/block=/);

    // a block found by its name: Enter chooses it
    const find = page.getByRole("searchbox", { name: "Find a block" });
    await find.fill("core");
    await expect(page.locator(".smart-find-count")).toHaveText("1 block");
    await find.press("Enter");
    await expect(page).toHaveURL(/[?&]block=core\b/);

    // what a change reaches, through others too; lit up when asked
    await page.goto(`${web}/-/smart_view?block=api`);
    const api2 = page.getByRole("complementary", { name: "api, details" });
    await expect(api2.getByText("2 blocks depend on it, 1 of them through others.")).toBeVisible();
    await expect(api2.locator("li", { hasText: "through image" })).toContainText("app");
    await api2.getByRole("button", { name: "Light them up" }).click();
    await expect(page).toHaveURL(/[?&]impact=1\b/);
    await expect(card("app")).not.toHaveClass(/\bdim\b/);
    await expect(card("core")).toHaveClass(/\bdim\b/);

    // a correction, written into a new assay.toml in the editor
    await api2.getByRole("button", { name: "Rename or describe" }).click();
    await api2.getByLabel("Name", { exact: true }).fill("API");
    await api2.getByRole("button", { name: "Write it in assay.toml" }).click();
    await expect(page).toHaveURL(new RegExp(`${web}/-/new/main`));
    await expect(page.getByLabel("File path")).toHaveValue("assay.toml");
    await expect(page.getByLabel("File content")).toHaveValue(/\[blocks\.api\]\nname = "API"\n/);

    // a file's page names its block and who to ask
    await page.goto(`${web}/-/blob/main/api/Cargo.toml`);
    await expect(page.locator(".block-chip")).toContainText("api");

    // a merge request lists the blocks it changes, lit up in the Smart View
    if (!isMobile(page)) {
      await api("post", `/projects/${project.id}/repository/commits`, {
        branch: "more-core",
        start_branch: "main",
        commit_message: "More core",
        actions: [{ action: "create", file_path: "core/src/more.rs", content: "pub fn g() {}\n" }],
      });
      const mr = await api("post", `/projects/${project.id}/merge_requests`, { source_branch: "more-core", title: "More core" });
      await page.goto(`${web}/-/merge_requests/${mr.iid}`);
      const blocks = page.locator(".fact").filter({ has: page.locator(".fact-label", { hasText: /^Blocks$/ }) });
      await expect(blocks.getByRole("link", { name: "core", exact: true })).toBeVisible();
      await expect(blocks).toContainText("1 file");
      await blocks.getByRole("link", { name: "See them in the Smart View" }).click();
      await expect(page.locator(".smart-mr")).toContainText("changes 1 block: core");
    }
  } finally {
    await remove();
  }
});
