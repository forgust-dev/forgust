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
        { action: "create", file_path: "api/Cargo.toml", content: crate("api", '\n[dependencies]\naxum = "0.8"\ncore = { path = "../core" }\n') },
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
    await expect(card("db")).toContainText("postgres:17");
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
  } finally {
    await remove();
  }
});
