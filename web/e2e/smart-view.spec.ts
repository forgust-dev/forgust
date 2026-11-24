import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { Locator } from "@playwright/test";
import { test, expect, freshProject, isMobile } from "./fixtures";

// The Smart View: a project's code as blocks, what each needs, and who to ask. A job of
// Forgust's maps the code with the assay image; here a runner of the test's takes it and
// uploads what assay writes for these files (`assay report`, kept beside this file). The
// instance's Smart View runner tags are changed for the run, then put back.
const REPORT = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), "smart-view-report.json"), "utf8"));

test("the Smart View maps the code into blocks and tells what each needs", async ({ page }) => {
  const tag = `e2e-map-${Date.now() % 100000}`;
  let before: any = null;
  let runnerId: number | null = null;
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

    // off until a Maintainer turns it on
    await page.goto(`${web}/-/smart_view`);
    await expect(page.getByRole("heading", { name: "Smart View" })).toBeVisible();
    await expect(page.getByText("The Smart View is off for this project.")).toBeVisible();
    await expect(page.getByRole("link", { name: "Turn it on in the settings" })).toHaveAttribute("href", `${web}/-/settings/automation#smart-view`);

    // turned on, a job maps the default branch: an instance runner of this test takes it
    before = (await api("get", "/admin/smart_view")).settings;
    await api("put", "/admin/smart_view", { ...before, enabled: true, tags: [tag] });
    const runner = await api("post", "/admin/runners", { description: tag, tags: [tag], run_untagged: false });
    runnerId = runner.id;
    await api("put", `/projects/${project.id}/smart_view`, { enabled: true });
    await page.reload();
    await expect(page.getByText("The code is being mapped by")).toBeVisible();
    let job: any = null;
    for (let i = 0; i < 40 && !job; i++) {
      const r = await page.request.post("/api/v4/jobs/request", { data: { token: runner.token, system_id: tag, info: { version: "19.5.3", executor: "docker" } } });
      if (r.status() === 201) job = await r.json();
      else await page.waitForTimeout(250);
    }
    expect(job, "the map job").toBeTruthy();
    expect(JSON.stringify(job.steps)).toContain("assay report");
    // the default branch's: with what each commit changed to the map, the first time all of them
    expect(JSON.stringify(job.steps)).toContain("--history");
    const sha = job.git_info.sha;
    const commits = await api("get", `/projects/${project.id}/repository/commits?ref_name=main`);
    const first = commits[commits.length - 1].id;
    // two commits: the workspace, its image and services, then the crates
    const crates = ["api", "core"];
    const ofCrates = (l: any) => crates.includes(l.from) || crates.includes(l.to);
    const now = Math.floor(Date.now() / 1000);
    const chapter = (commit: string, title: string, at: number, blocks: any[], links: any[]) => ({
      commit,
      author: "E2E",
      email: "e2e@example.com",
      at,
      title,
      changes: { blocks_added: blocks.map((b) => ({ id: b.id, name: b.name, kind: b.kind, ...(b.role ? { role: b.role } : {}) })), links_added: links },
    });
    const history = {
      chapters: [
        chapter(first, "Start the map", now - 3600, REPORT.map.blocks.filter((b: any) => !crates.includes(b.id)), REPORT.map.links.filter((l: any) => !ofCrates(l))),
        chapter(sha, "Add the crates", now, REPORT.map.blocks.filter((b: any) => crates.includes(b.id)), REPORT.map.links.filter(ofCrates)),
      ],
    };
    const report = { ...REPORT, commit: sha, map: { ...REPORT.map, commit: sha }, history };
    const up = await page.request.post(`/api/v4/jobs/${job.id}/artifacts?artifact_type=smart_view&artifact_format=raw`, {
      headers: { "JOB-TOKEN": job.token },
      multipart: { file: { name: "assay-report.json", mimeType: "application/json", buffer: Buffer.from(JSON.stringify(report)) } },
    });
    expect(up.status()).toBe(201);
    expect((await page.request.put(`/api/v4/jobs/${job.id}`, { data: { token: job.token, state: "success", exit_code: 0 } })).status()).toBe(200);

    // the page takes the map in when the job is done
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

    // a block chosen: what it needs and what needs it, each with the link and the file that says so
    await card("api").click();
    await expect(page).toHaveURL(/[?&]block=api\b/);
    const panel = page.getByRole("complementary", { name: "api, details" });
    await expect(panel.getByRole("heading", { name: "api", exact: true })).toBeVisible();
    const row = (details: Locator, name: string) => details.locator("li").filter({ has: page.getByRole("button", { name, exact: true }) });
    await expect(row(panel, "core")).toContainText("uses");
    await expect(row(panel, "core").getByRole("link", { name: "From api/Cargo.toml" })).toHaveAttribute("href", `${web}/-/blob/main/api/Cargo.toml`);
    await expect(row(panel, "image")).toContainText("builds");
    await expect(row(panel, "image").getByRole("link", { name: "From Dockerfile" })).toBeVisible();
    // where it is, and the manifest it was found in
    await expect(panel.getByRole("link", { name: "Cargo.toml", exact: true })).toHaveAttribute("href", `${web}/-/blob/main/api/Cargo.toml`);
    // since when each link is there, and what the history tells of the block
    await expect(row(panel, "core")).toContainText(`since ${sha.slice(0, 7)}`);
    const told = panel.locator("section").filter({ has: page.getByRole("heading", { name: /^History/ }) });
    await expect(told).toContainText("Added · Now uses core");
    await expect(told.getByRole("link", { name: "Add the crates" })).toHaveAttribute("href", `${web}/-/commit/${sha}`);

    // from one block to the next: the image, and who made it
    await panel.getByRole("button", { name: "image", exact: true }).click();
    const image = page.getByRole("complementary", { name: "image, details" });
    await expect(row(image, "app")).toContainText("runs");
    await expect(row(image, "app").getByRole("link", { name: "From compose.yml" })).toBeVisible();
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

    // the map through its history, a commit at a time, then back to now
    await page.goto(`${web}/-/smart_view`);
    await page.getByRole("button", { name: /^2 changes since / }).click();
    await expect(page).toHaveURL(new RegExp(`[?&]history=${sha}`));
    const chapterNow = page.locator(".smart-history-chapter");
    await expect(chapterNow).toContainText("Add the crates");
    await expect(chapterNow).toContainText("Adds 2 blocks: api and core");
    await page.getByRole("button", { name: "The change before" }).click();
    await expect(page).toHaveURL(new RegExp(`[?&]history=${first}`));
    await expect(chapterNow).toContainText("The first map: 3 blocks and 2 links.");
    await page.getByRole("button", { name: "Back to the map of now" }).click();
    await expect(page).not.toHaveURL(/history=/);

    // what a change reaches, through others too; lit up when asked
    await page.goto(`${web}/-/smart_view?block=api`);
    const api2 = page.getByRole("complementary", { name: "api, details" });
    await expect(api2.getByText("It reaches 2 blocks: those that use it, and these:")).toBeVisible();
    await expect(row(api2, "app")).toContainText("through image");
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
    if (before) await api("put", "/admin/smart_view", before);
    if (runnerId) await api("delete", `/runners/${runnerId}`);
    await remove();
  }
});
