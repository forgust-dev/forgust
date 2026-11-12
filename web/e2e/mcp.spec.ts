import { createHash, randomBytes } from "node:crypto";
import { test, expect, isMobile } from "./fixtures";

// An AI assistant signs in with OAuth: it registers, the person allows
// it on the consent page, and the code it gets back buys a token.

const b64url = (b: Buffer) => b.toString("base64url");

test("MCP sign-in: the consent page", async ({ page }) => {
  test.skip(isMobile(page), "one run is enough");
  const redirect = "http://127.0.0.1:9/callback";
  const reg = await page.request.post("/oauth/register", { data: { client_name: "E2E Assistant", redirect_uris: [redirect] } });
  expect(reg.status()).toBe(201);
  const { client_id } = await reg.json();
  const verifier = b64url(randomBytes(32));
  const challenge = b64url(createHash("sha256").update(verifier).digest());
  const q = new URLSearchParams({ client_id, redirect_uri: redirect, response_type: "code", code_challenge: challenge, code_challenge_method: "S256", scope: "mcp", state: "st8" });

  // the browser goes back to the client: catch it there
  let back = "";
  await page.route("http://127.0.0.1:9/**", (route) => {
    back = route.request().url();
    return route.fulfill({ status: 200, contentType: "text/plain", body: "ok" });
  });
  await page.goto(`/oauth/authorize?${q}`);
  await expect(page.getByRole("heading", { name: "Allow E2E Assistant?" })).toBeVisible();
  await expect(page.getByText("127.0.0.1", { exact: true })).toBeVisible();
  const write = page.getByLabel(/Also let it make changes/);
  await expect(write).not.toBeChecked();
  await page.getByRole("button", { name: "Allow" }).click();
  await expect.poll(() => back).toContain("code=");
  const answer = new URL(back);
  expect(answer.searchParams.get("state")).toBe("st8");

  const t = await page.request.post("/oauth/token", {
    form: { grant_type: "authorization_code", code: answer.searchParams.get("code")!, client_id, redirect_uri: redirect, code_verifier: verifier },
  });
  expect(t.status()).toBe(200);
  const token = await t.json();
  expect(token.scope).toBe("mcp");
  const r = await page.request.post("/api/v4/mcp", {
    headers: { authorization: `Bearer ${token.access_token}` },
    data: { jsonrpc: "2.0", id: 1, method: "tools/list" },
  });
  const tools = (await r.json()).result.tools.map((x: { name: string }) => x.name);
  expect(tools).toContain("get_merge_request");
  expect(tools).not.toContain("create_issue");

  // the person sees it with their tokens
  await page.goto("/profile/tokens");
  await expect(page.locator(".item", { hasText: "MCP: E2E Assistant" }).first()).toContainText("used by an AI assistant");
  // tidy: revoke it
  page.on("dialog", (d) => d.accept());
  await page.locator(".item", { hasText: "MCP: E2E Assistant" }).first().getByRole("button", { name: "Revoke" }).click();
});
