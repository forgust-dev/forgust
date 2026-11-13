// The README's screenshots, light and dark, from the seeded demo instance:
//   FORGUST_HOST=192.0.2.10 docker compose up -d --build
//   FORGUST_HOST=192.0.2.10 python3 deploy/seed.py
//   FORGUST_HOST=192.0.2.10 node deploy/screenshots.mjs [name]
// Writes .github/images/<name>-light.png and <name>-dark.png (all of them, or
// only the one named). Uses the web UI's Playwright: run `npm ci` in web/ first.
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const { chromium } = createRequire(join(root, "web/package.json"))("@playwright/test");

const host = process.env.FORGUST_HOST;
if (!host) throw new Error("set FORGUST_HOST to the address compose.yml was started with");
const base = `http://${host}:8080`;
const password = process.env.SEED_PASSWORD ?? "forgust-demo-pass";
const out = join(root, ".github/images");
const only = process.argv[2];
const P = "acme/shop";

const browser = await chromium.launch();
const contexts = {};
/** One signed-in context per user; each page picks its colour scheme. */
async function as(user) {
  if (!contexts[user]) {
    const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1.5 });
    const r = await ctx.request.post(`${base}/api/v1/auth/login`, { data: { login: user, password } });
    if (!r.ok()) throw new Error(`sign-in as ${user}: ${r.status()} (is the instance seeded?)`);
    contexts[user] = ctx;
  }
  return contexts[user];
}
async function api(path) {
  const r = await (await as("alice")).request.get(`${base}/api/v1${path}`);
  if (!r.ok()) throw new Error(`GET ${path}: ${r.status()}`);
  return r.json();
}
function need(value, what) {
  if (!value) throw new Error(`no ${what} in the demo: run deploy/seed.py first`);
  return value;
}

// the subjects, from what the seed made: looked up only for the shots that need them
const wanted = (...names) => !only || names.includes(only);
const project = await api(`/resolve?path=${encodeURIComponent(P)}`);
let mr, pipeline, job;
if (wanted("merge-request", "diff")) {
  const mrs = await api(`/projects/${project.id}/merge_requests?state=opened`);
  mr = need(mrs.find((m) => /discount/i.test(m.title)) ?? mrs[0], "open merge request");
}
if (wanted("pipeline", "job")) {
  // the main pipeline with the most jobs, once it has finished
  const mains = (await api(`/projects/${project.id}/pipelines`)).filter((p) => p.ref === "main");
  const details = await Promise.all(mains.map((p) => api(`/projects/${project.id}/pipelines/${p.iid}`)));
  pipeline = need(details.sort((a, b) => b.jobs.length - a.jobs.length)[0], "pipeline on main");
  for (let i = 0; ["running", "pending", "created"].includes(pipeline.status); i++) {
    if (i === 60) throw new Error(`pipeline #${pipeline.iid} still ${pipeline.status} after 10 minutes`);
    if (i === 0) console.log(`waiting for pipeline #${pipeline.iid} (${pipeline.status})…`);
    await new Promise((r) => setTimeout(r, 10_000));
    pipeline = await api(`/projects/${project.id}/pipelines/${pipeline.iid}`);
  }
  job = need(pipeline.jobs.find((j) => /^unit/.test(j.name)) ?? pipeline.jobs[0], "job");
}

const SHOTS = [
  // name, user, path (a function when it needs a subject above)
  ["code", "alice", P],
  ["file", "alice", `${P}/-/blob/main/shop/cart.py`],
  ["merge-request", "alice", () => `${P}/-/merge_requests/${mr.iid}`],
  ["diff", "alice", () => `${P}/-/merge_requests/${mr.iid}/diffs`],
  ["pipeline", "alice", () => `${P}/-/pipelines/${pipeline.iid}`],
  ["job", "alice", () => `${P}/-/jobs/${job.id}`],
  ["issues", "alice", `${P}/-/issues`],
  ["security", "alice", `${P}/-/security`],
  ["admin", "root", "admin"],
  ["admin-storage", "root", "admin/storage"],
  ["admin-health", "root", "admin/health"],
];

for (const [name, user, at] of SHOTS) {
  if (only && only !== name) continue;
  const path = typeof at === "function" ? at() : at;
  const ctx = await as(user);
  await Promise.all(
    ["light", "dark"].map(async (scheme) => {
      const page = await ctx.newPage();
      await page.emulateMedia({ colorScheme: scheme });
      await page.goto(`${base}/${path}`);
      await page.waitForLoadState("networkidle");
      await page.evaluate(() => document.fonts.ready);
      await page.screenshot({ path: join(out, `${name}-${scheme}.png`), animations: "disabled" });
      await page.close();
      console.log(`wrote .github/images/${name}-${scheme}.png from /${path}`);
    }),
  );
}
await browser.close();
