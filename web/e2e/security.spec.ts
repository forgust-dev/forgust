import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test, expect, freshProject, isMobile } from "./fixtures";

// Findings from the security reports jobs upload: the Security tab,
// dismissing one, and a merge request's new and fixed findings.

const FIXTURES = join(dirname(fileURLToPath(import.meta.url)), "../../crates/forgust-server/tests/fixtures/security");
const fixture = (name: string) => readFileSync(join(FIXTURES, name));

test("security findings: the tab, dismissing, and a merge request", async ({ page }) => {
  test.skip(isMobile(page), "one run is enough: the API work is the same");
  const tag = `e2e-sec-${Date.now() % 100000}`;
  const ci = `scan:\n  tags: [${tag}]\n  script: [scan]\n`;
  const { project, web, api, remove } = await freshProject(page, "sec", { ".gitlab-ci.yml": ci });
  try {
    const runner = await api("post", `/projects/${project.id}/runners`, { description: tag, tags: [tag], run_untagged: false });
    const runJob = async (reports: [string, Buffer][]) => {
      let job: any = null;
      for (let i = 0; i < 40 && !job; i++) {
        const r = await page.request.post("/api/v4/jobs/request", { data: { token: runner.token, system_id: tag, info: { version: "19.5.3", executor: "docker" } } });
        if (r.status() === 201) job = await r.json();
        else await page.waitForTimeout(250);
      }
      expect(job, "the job").toBeTruthy();
      for (const [type, buffer] of reports) {
        const r = await page.request.post(`/api/v4/jobs/${job.id}/artifacts?artifact_type=${type}&artifact_format=raw`, {
          headers: { "JOB-TOKEN": job.token },
          multipart: { file: { name: "gl-report.json", mimeType: "application/json", buffer } },
        });
        expect(r.status()).toBe(201);
      }
      expect((await page.request.put(`/api/v4/jobs/${job.id}`, { data: { token: job.token, state: "success", exit_code: 0 } })).status()).toBe(200);
    };

    // the default branch's scan
    await runJob([
      ["sast", fixture("semgrep-sast.json")],
      ["dependency_scanning", fixture("gemnasium-dependency.json")],
    ]);
    await expect(async () => {
      await page.goto(`${web}/-/security`);
      await expect(page.locator(".finding-row").first()).toBeVisible({ timeout: 1500 });
    }).toPass({ timeout: 20_000 });
    await expect(page.locator(".finding-row")).toHaveCount(3);
    await expect(page.locator(".finding-row").first()).toContainText("Prototype Pollution in lodash");
    await expect(page.locator(".finding-row").first()).toContainText("fixed in 4.17.21");

    // dismiss the MD5 one
    await page.locator(".finding-row", { hasText: "broken or risky cryptographic" }).locator(".finding-toggle").click();
    await page.getByLabel("Reason").click();
    await page.getByRole("option", { name: "Used in tests" }).click();
    await page.getByLabel("Comment").fill("a cache key");
    await page.locator(".finding-detail").getByRole("button", { name: "Dismiss" }).click();
    await expect(page.locator(".toast.ok").last()).toContainText("Dismissed");
    await expect(page.locator(".finding-row")).toHaveCount(2);
    await page.getByRole("button", { name: /^Dismissed/ }).click();
    await expect(page.locator(".finding-row")).toHaveCount(1);
    await expect(page.locator(".finding-row")).toContainText("dismissed by");

    // a merge request whose pipeline fixes the SQL injection and adds CodeQL's
    const branch = "fix";
    await api("post", `/projects/${project.id}/repository/commits`, {
      branch,
      start_branch: "main",
      commit_message: "fix",
      actions: [{ action: "create", file_path: "fix.txt", content: "x\n" }],
    });
    const sast = JSON.parse(fixture("semgrep-sast.json").toString());
    sast.vulnerabilities.shift();
    await runJob([["sast", Buffer.concat([Buffer.from(JSON.stringify(sast)), Buffer.from("\n"), fixture("codeql.sarif")])]]);
    const mr = await api("post", `/projects/${project.id}/merge_requests`, { source_branch: branch, title: "Fix" });
    const panel = page.getByRole("region", { name: "Security scanning" });
    await expect(async () => {
      await page.goto(`${web}/-/merge_requests/${mr.iid}`);
      await expect(panel).toContainText("2 new, 1 fixed", { timeout: 1500 });
    }).toPass({ timeout: 20_000 });
    await expect(panel).toContainText("Database query built from user-controlled sources");
    await expect(panel.locator(".finding-title.struck")).toContainText("SQL Injection");
  } finally {
    await remove();
  }
});
