import zlib from "node:zlib";
import { test, expect, P, csrfHeaders } from "./fixtures";

/** A zip with stored and deflated entries, as the runner's archives have. */
function zip(files: { name: string; data: Buffer; deflate?: boolean }[]): Buffer {
  const parts: Buffer[] = [];
  const central: Buffer[] = [];
  let offset = 0;
  for (const f of files) {
    const body = f.deflate ? zlib.deflateRawSync(f.data) : f.data;
    const name = Buffer.from(f.name);
    const crc = zlib.crc32(f.data);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(f.deflate ? 8 : 0, 8);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(body.length, 18);
    local.writeUInt32LE(f.data.length, 22);
    local.writeUInt16LE(name.length, 26);
    parts.push(local, name, body);
    const c = Buffer.alloc(46);
    c.writeUInt32LE(0x02014b50, 0);
    c.writeUInt16LE(20, 4);
    c.writeUInt16LE(20, 6);
    c.writeUInt16LE(f.deflate ? 8 : 0, 10);
    c.writeUInt32LE(crc, 16);
    c.writeUInt32LE(body.length, 20);
    c.writeUInt32LE(f.data.length, 24);
    c.writeUInt16LE(name.length, 28);
    c.writeUInt32LE(offset, 42);
    central.push(c, name);
    offset += 30 + name.length + body.length;
  }
  const cd = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(files.length, 8);
  end.writeUInt16LE(files.length, 10);
  end.writeUInt32LE(cd.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...parts, cd, end]);
}

const JUNIT = `<?xml version="1.0"?><testsuite name="unit"><testcase classname="cart" name="adds"/><testcase classname="cart" name="removes"><failure message="removes broke">expected 1, got 2</failure></testcase></testsuite>`;

test("a job's test report and artifacts, on the pipeline, the merge request and the job", async ({ page }) => {
  const csrf = await csrfHeaders(page);
  const project = await (await page.request.get(`/api/v1/resolve?path=${P.slice(1)}`)).json();
  const api = `/api/v1/projects/${project.id}`;
  const branch = `e2e-reports-${Date.now() % 100000}`;
  let mrIid: number | null = null;
  try {
    // a pipeline on a new branch whose job only a tagged runner takes
    const ci = "reports:\n  tags: [e2e-reports]\n  script: [make test]\n  artifacts:\n    paths: [out/]\n    reports:\n      junit: report.xml\n";
    const c = await page.request.post(`${api}/repository/commits`, {
      headers: csrf,
      data: { branch, start_branch: "main", commit_message: "reports", actions: [{ action: "update", file_path: ".gitlab-ci.yml", content: ci }] },
    });
    expect(c.ok(), await c.text()).toBe(true);
    const runner = await (await page.request.post(`${api}/runners`, { headers: csrf, data: { description: "e2e reports", tags: ["e2e-reports"], run_untagged: false } })).json();
    let job: any = null;
    for (let i = 0; i < 40 && !job; i++) {
      const r = await page.request.post("/api/v4/jobs/request", { data: { token: runner.token, system_id: "e2e-reports", info: { version: "19.5.3", executor: "docker" } } });
      if (r.status() === 201) job = await r.json();
      else await page.waitForTimeout(250);
    }
    expect(job, "the tagged runner got the job").toBeTruthy();
    const upload = (type: string, format: string, name: string, buffer: Buffer) =>
      page.request.post(`/api/v4/jobs/${job.id}/artifacts?artifact_type=${type}&artifact_format=${format}`, {
        headers: { "JOB-TOKEN": job.token },
        multipart: { file: { name, mimeType: "application/octet-stream", buffer } },
      });
    expect((await upload("junit", "gzip", "junit.xml.gz", zlib.gzipSync(JUNIT))).status()).toBe(201);
    const archive = zip([
      { name: "out/r.txt", data: Buffer.from("built\n".repeat(50)), deflate: true },
      { name: "out/logo.png", data: Buffer.from("png") },
    ]);
    expect((await upload("archive", "zip", "artifacts.zip", archive)).status()).toBe(201);
    const done = await page.request.put(`/api/v4/jobs/${job.id}`, { data: { token: job.token, state: "failed", exit_code: 1, output: { bytesize: 0 } } });
    expect(done.status()).toBe(200);
    const mr = await (await page.request.post(`${api}/merge_requests`, { headers: csrf, data: { source_branch: branch, title: `Reports ${branch}` } })).json();
    mrIid = mr.iid;
    const pipelines = await (await page.request.get(`${api}/pipelines?ref=${branch}`)).json();
    const iid = pipelines[0].iid;

    // the pipeline's Tests section (the report is read in the background)
    await expect(async () => {
      await page.goto(`${P}/-/pipelines/${iid}`);
      await expect(page.getByRole("region", { name: "Tests" })).toContainText("2 tests · 1 failed · 1 passed", { timeout: 1500 });
    }).toPass({ timeout: 20_000 });
    const tests = page.getByRole("region", { name: "Tests" });
    await tests.getByRole("button", { name: /removes/ }).click();
    await expect(tests.locator(".test-output")).toContainText("expected 1, got 2");

    // the merge request's test summary (nothing to compare with on main: all new)
    await page.goto(`${P}/-/merge_requests/${mrIid}`);
    const summary = page.getByRole("region", { name: "Test summary" });
    await expect(summary).toContainText("2 tests · 1 failed");
    await expect(summary).toContainText("New failures");

    // the job's artifacts, browsed
    await page.goto(`${P}/-/jobs/${job.id}`);
    await page.getByRole("link", { name: "Browse" }).click();
    const files = page.locator(".list.rows[aria-label=\"Files\"]");
    await files.getByRole("link", { name: "out" }).click();
    const txt = files.getByRole("link", { name: /r\.txt/ });
    await expect(txt).toBeVisible();
    await expect(files.getByRole("link", { name: /logo\.png/ })).toBeVisible();
    const href = await txt.getAttribute("href");
    const file = await page.request.get(href!);
    expect(file.headers()["content-type"]).toBe("text/plain; charset=utf-8");
    expect(await file.text()).toBe("built\n".repeat(50));
  } finally {
    if (mrIid) await page.request.put(`${api}/merge_requests/${mrIid}`, { headers: csrf, data: { state_event: "close" } });
    await page.request.delete(`${api}/repository/branches/${branch}`, { headers: csrf });
  }
});
