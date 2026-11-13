import { test as base, expect, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ADMIN, SEED } from "./global-setup";

export { expect, SEED, ADMIN };

/** Every test fails on an uncaught page error or a console error. */
export const test = base.extend<{ errors: string[] }>({
  errors: [
    async ({ page }, use) => {
      const errors: string[] = [];
      page.on("pageerror", (e) => errors.push(String(e)));
      // anything the page's CSP blocks is a failure too, not just a console line
      await page.addInitScript(() => {
        document.addEventListener("securitypolicyviolation", (e) => console.error(`CSP blocked ${e.blockedURI} (${e.violatedDirective})`));
      });
      page.on("console", (m) => {
        // 400/401/403/404: answers the page shows (validation, sign-in, access); 5xx and the rest fail
        if (m.type() === "error" && !/status of 40[0134]/.test(m.text())) errors.push(m.text());
      });
      await use(errors);
      expect(errors, "page errors").toEqual([]);
    },
    { auto: true },
  ],
});

export const P = `/${SEED.group}/${SEED.project}`;

export const isMobile = (page: Page) => (page.viewportSize()?.width ?? 1000) < 900;

/** Wait for an SPA route (no full navigation happens, so no waitForURL). */
export async function at(page: Page, path: string | RegExp) {
  await expect.poll(() => page.evaluate(() => location.pathname + location.hash)).toMatch(typeof path === "string" ? new RegExp(`^${path.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`) : path);
}

/** Headers that let `page.request` write to the API as the signed-in user. */
export async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const session = await (await page.request.get("/api/v1/session")).json();
  return { "x-csrf-token": session.csrf_token };
}

/** Calls `/api/v1` as the signed-in user and fails the test on an error answer:
 * `const call = await apiCall(page); await call("post", "/projects", {...})`. */
export async function apiCall(page: Page) {
  const csrf = await csrfHeaders(page);
  return async (method: "get" | "post" | "put" | "delete", url: string, data?: unknown) => {
    const r = await page.request[method](`/api/v1${url}`, { headers: csrf, data });
    expect(r.ok(), `${method} ${url}: ${await r.text()}`).toBeTruthy();
    return r.status() === 204 ? null : r.json();
  };
}

/** A project of its own (settings other specs must not see), with a first
 * commit on `main` pushed over git; `files` are its content. Delete it after. */
export async function freshProject(page: Page, prefix: string, files: Record<string, string>) {
  const csrf = await csrfHeaders(page);
  const call = await apiCall(page);
  const group = await call("get", `/resolve?path=${SEED.group}`);
  const n = Date.now() % 1_000_000;
  const path = `${prefix}-${n}`;
  const project = await call("post", "/projects", { namespace_id: group.id, name: `${prefix} ${n}`, path });
  // the commits API needs a branch to start from: the first commit goes over git
  const token = (await call("post", "/user/tokens", { name: `e2e-${path}`, scopes: ["write_repository"] })).token as string;
  const dir = mkdtempSync(join(tmpdir(), `forgust-e2e-${prefix}-`));
  const git = (...args: string[]) => execFileSync("git", args, { cwd: dir, stdio: "pipe" });
  git("init", "-q", "-b", "main");
  for (const [f, content] of Object.entries(files)) writeFileSync(join(dir, f), content);
  git("add", ".");
  git("-c", "user.name=E2E", "-c", "user.email=e2e@example.com", "commit", "-q", "-m", "init");
  const url = new URL(project.http_url);
  url.username = ADMIN;
  url.password = token;
  git("push", "-q", url.toString(), "main");
  return { project, web: `/${SEED.group}/${path}`, api: call, remove: () => page.request.delete(`/api/v1/projects/${project.id}`, { headers: csrf }) };
}
