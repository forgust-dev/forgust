import { request, type FullConfig } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export const ADMIN = process.env.FORGUST_E2E_ADMIN ?? "root";
export const PASSWORD = process.env.FORGUST_E2E_PASSWORD ?? "rootpassword";

/** Fixture data the specs rely on. Idempotent: reruns reuse what exists. */
export const SEED = { group: "e2e", project: "shop", other: "blog", feature: "feature/checkout", stacked: "feature/checkout-ui", recent: "fix/typo" };

function git(cwd: string, ...args: string[]) {
  execFileSync("git", args, { cwd, stdio: "pipe", env: { ...process.env, GIT_TERMINAL_PROMPT: "0" } });
}

export default async function setup(config: FullConfig) {
  const baseURL = config.projects[0].use.baseURL!;
  const ctx = await request.newContext({ baseURL });
  const login = await ctx.post("/api/v1/auth/login", { data: { login: ADMIN, password: PASSWORD } });
  if (!login.ok()) throw new Error(`login as ${ADMIN} failed: ${login.status()} — create it with \`forgust admin create-user --admin\``);
  const csrf = (await login.json()).session.csrf_token as string;
  const h = { "x-csrf-token": csrf };
  mkdirSync("e2e/.auth", { recursive: true });
  await ctx.storageState({ path: "e2e/.auth/admin.json" });

  const get = async (url: string) => {
    const r = await ctx.get(`/api/v1${url}`);
    return r.ok() ? r.json() : null;
  };
  const post = async (url: string, data: unknown) => {
    const r = await ctx.post(`/api/v1${url}`, { data, headers: h });
    if (!r.ok()) throw new Error(`POST ${url}: ${r.status()} ${await r.text()}`);
    return r.json();
  };

  const group = await get(`/resolve?path=${SEED.group}`);
  const groupId = group ? group.id : (await post("/groups", { name: "E2E", path: SEED.group })).id;
  // a second, empty project for cross-project navigation
  if (!(await get(`/resolve?path=${SEED.group}/${SEED.other}`))) await post("/projects", { namespace_id: groupId, name: "Blog", path: SEED.other });
  const existing = await get(`/resolve?path=${SEED.group}/${SEED.project}`);
  if (existing) return;

  const project = await post("/projects", { namespace_id: groupId, name: "Shop", path: SEED.project, description: "Fixture for the browser tests" });
  const token = (await post("/user/tokens", { name: `e2e-${Date.now()}`, scopes: ["api", "write_repository"] })).token as string;

  const dir = mkdtempSync(join(tmpdir(), "forgust-e2e-"));
  const w = (p: string, s: string) => {
    mkdirSync(join(dir, p, ".."), { recursive: true });
    writeFileSync(join(dir, p), s);
  };
  git(dir, "init", "-q", "-b", "main");
  git(dir, "config", "user.name", "E2E");
  git(dir, "config", "user.email", "e2e@example.com");
  w("README.md", "# Shop\n\nA fixture project with **Markdown**.\n");
  w("src/cart.rs", "pub struct Cart {\n    items: Vec<u32>,\n}\n\nimpl Cart {\n    pub fn total(&self) -> u32 {\n        self.items.iter().sum()\n    }\n}\n");
  w("src/lib.rs", "pub mod cart;\n");
  w("web/app.ts", "export function greet(name: string): string {\n  return `Hello ${name}`;\n}\n");
  for (let i = 0; i < 12; i++) w(`docs/page-${String(i).padStart(2, "0")}.md`, `# Page ${i}\n`);
  w("long.txt", Array.from({ length: 120 }, (_, i) => `line ${i + 1}`).join("\n") + "\n");
  w(".gitlab-ci.yml", "stages: [build, test]\nbuild:\n  stage: build\n  script: [cargo build]\ntest:\n  stage: test\n  script: [cargo test]\n");
  git(dir, "add", "-A");
  git(dir, "commit", "-q", "-m", "Initial commit");
  // a feature branch with several changed files
  git(dir, "checkout", "-q", "-b", SEED.feature);
  w("src/cart.rs", "pub struct Cart {\n    items: Vec<u32>,\n    discount: u32,\n}\n\nimpl Cart {\n    pub fn total(&self) -> u32 {\n        let sum: u32 = self.items.iter().sum();\n        sum.saturating_sub(self.discount)\n    }\n}\n");
  w("web/app.ts", "export function greet(name: string): string {\n  return `Welcome, ${name}!`;\n}\n\nexport const VERSION = 2;\n");
  for (let i = 0; i < 6; i++) w(`docs/page-${String(i).padStart(2, "0")}.md`, `# Page ${i}\n\nUpdated.\n`);
  w("src/checkout.rs", "pub fn checkout() -> bool {\n    true\n}\n");
  git(dir, "add", "-A");
  git(dir, "commit", "-q", "-m", "Add checkout and discounts");
  // a second layer on the feature branch, for the stack map (!2 on !1)
  git(dir, "checkout", "-q", "-b", SEED.stacked);
  w("web/checkout.ts", "export function pay(): boolean {\n  return true;\n}\n");
  git(dir, "add", "-A");
  git(dir, "commit", "-q", "-m", "Checkout page");
  git(dir, "checkout", "-q", "main");
  git(dir, "checkout", "-q", "-b", SEED.recent);
  w("README.md", "# Shop\n\nA fixture project with **Markdown** support.\n");
  git(dir, "commit", "-qam", "Fix typo");
  const url = new URL(project.http_url);
  url.username = ADMIN;
  url.password = token;
  git(dir, "push", "-q", url.toString(), "main", SEED.feature, SEED.stacked, SEED.recent);

  const mr = await post(`/projects/${project.id}/merge_requests`, {
    source_branch: SEED.feature,
    target_branch: "main",
    title: "Checkout with discounts",
    description: "Adds `checkout()` and a discount on the cart.",
  });
  // one unresolved line comment for n/p navigation
  const detail = await get(`/projects/${project.id}/merge_requests/${mr.iid}`);
  await post(`/projects/${project.id}/merge_requests/${mr.iid}/notes`, {
    body: "Should the discount be capped?",
    position: { version: detail.versions[0].version, old_path: "src/cart.rs", new_path: "src/cart.rs", new_line: 9 },
  });
  await post(`/projects/${project.id}/merge_requests`, { source_branch: SEED.stacked, title: "Checkout page", stack_on_iid: mr.iid });
  // a fake runner finishes the first job of main's pipeline with a real-looking log
  await runFirstJob(ctx, post, project.id);
  await ctx.dispose();
}

const ts = (s: number) => `2026-11-07T10:00:${String(s).padStart(2, "0")}.000000Z`;
export const SAMPLE_LOG = [
  `${ts(0)} 00O \x1b[0KRunning with gitlab-runner 19.5.3 (fake)\x1b[0;m`,
  `${ts(1)} 00O section_start:1794045601:prepare_script\r\x1b[0K`,
  `${ts(1)} 00O+\x1b[0K\x1b[36;1mPreparing environment\x1b[0;m`,
  `${ts(2)} 01O Running on fake-host`,
  `${ts(3)} 00O section_end:1794045603:prepare_script\r\x1b[0K`,
  `${ts(3)} 00O+section_start:1794045603:step_script\r\x1b[0K`,
  `${ts(3)} 00O+\x1b[0K\x1b[36;1mExecuting "step_script" stage of the job script\x1b[0;m`,
  `${ts(4)} 01O \x1b[32;1m$ cargo build\x1b[0;m`,
  `${ts(5)} 01E    Compiling shop v0.1.0`,
  `${ts(9)} 01O \x1b[32;1mFinished\x1b[0;m release target`,
  `${ts(9)} 00O section_end:1794045609:step_script\r\x1b[0K`,
  `${ts(9)} 00O \x1b[32;1mJob succeeded\x1b[0;m`,
].join("\n") + "\n";

async function runFirstJob(ctx: import("@playwright/test").APIRequestContext, post: (u: string, d: unknown) => Promise<any>, projectId: number) {
  const runner = await post(`/projects/${projectId}/runners`, { description: "e2e fake runner" });
  let job: any = null;
  for (let i = 0; i < 4 && !job; i++) {
    const r = await ctx.post("/api/v4/jobs/request", { data: { token: runner.token, system_id: "e2e", info: { version: "19.5.3", executor: "docker" } } });
    if (r.status() === 201) job = await r.json();
    else if (r.status() !== 204) throw new Error(`fake runner: ${r.status()}`);
  }
  if (!job) throw new Error("fake runner got no job");
  const body = Buffer.from(SAMPLE_LOG);
  const p = await ctx.patch(`/api/v4/jobs/${job.id}/trace`, { headers: { "JOB-TOKEN": job.token, "Content-Range": `0-${body.length - 1}` }, data: body });
  if (p.status() !== 202) throw new Error(`trace: ${p.status()}`);
  const done = await ctx.put(`/api/v4/jobs/${job.id}`, { data: { token: job.token, state: "success", exit_code: 0, output: { bytesize: body.length } } });
  if (done.status() !== 200) throw new Error(`finish: ${done.status()}`);
}
