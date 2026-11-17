import { createHmac } from "node:crypto";
import { apiCall, expect, test } from "./fixtures";

/** The current TOTP code for a base32 secret (RFC 6238: SHA-1, 30 s, 6 digits). */
function totp(secret: string, ahead = 0): string {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  let bits = "";
  for (const c of secret.replace(/=+$/, "").toUpperCase()) bits += alphabet.indexOf(c).toString(2).padStart(5, "0");
  const key = Buffer.from(bits.match(/.{8}/g)!.map((b) => parseInt(b, 2)));
  const counter = Buffer.alloc(8);
  counter.writeBigUInt64BE(BigInt(Math.floor(Date.now() / 1000 / 30) + ahead));
  const h = createHmac("sha1", key).update(counter).digest();
  const o = h[h.length - 1] & 0xf;
  return String((h.readUInt32BE(o) & 0x7fffffff) % 1_000_000).padStart(6, "0");
}

test("two-factor: recovery codes are shown once and sign in once", async ({ page, browser }) => {
  const admin = await apiCall(page);
  const username = `tfa${Date.now()}`;
  const pw = "a-long-test-password";
  const u = await admin("post", "/admin/users", { username, email: `${username}@example.com`, password: pw });

  const ctx = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const me = await ctx.newPage();
  const signIn = async () => {
    await me.goto("/login");
    await me.getByLabel("Username or email").fill(username);
    await me.getByLabel("Password").fill(pw);
    await me.getByRole("button", { name: "Sign in" }).click();
  };
  try {
    await signIn();
    await expect.poll(() => me.evaluate(() => location.pathname)).not.toBe("/login");
    await me.goto("/profile/security");
    await me.getByRole("button", { name: "Set up 2FA" }).click();
    const secret = (await me.locator("code").first().textContent())!.trim();
    await me.getByLabel("Code").fill(totp(secret));
    await me.getByLabel("Confirm with your password").fill(pw);
    await me.getByRole("button", { name: "Enable" }).click();

    // the codes, this once
    await expect(me.getByText("Save your recovery codes")).toBeVisible();
    const codes = (await me.locator(".recovery-codes").textContent())!.trim().split("\n");
    expect(codes).toHaveLength(10);
    await expect(me.getByText("10 recovery codes left.")).toBeVisible();
    await me.getByRole("button", { name: "I saved them" }).click();
    await expect(me.locator(".recovery-codes")).toHaveCount(0);

    // a lost authenticator: a recovery code instead
    await ctx.clearCookies();
    await signIn();
    await me.getByRole("button", { name: /Use a recovery code/ }).click();
    await me.getByLabel("Recovery code").fill(codes[0]);
    await me.getByRole("button", { name: "Verify" }).click();
    await expect.poll(() => me.evaluate(() => location.pathname)).not.toBe("/login");
    await me.goto("/profile/security");
    await expect(me.getByText("9 recovery codes left.")).toBeVisible();

    // used up: refused the second time
    await ctx.clearCookies();
    await signIn();
    await me.getByRole("button", { name: /Use a recovery code/ }).click();
    await me.getByLabel("Recovery code").fill(codes[0]);
    await me.getByRole("button", { name: "Verify" }).click();
    await expect(me.locator(".alert, .error-box, [role=alert]").first()).toBeVisible();
    await expect.poll(() => me.evaluate(() => location.pathname)).toBe("/login");
  } finally {
    await ctx.close();
    await admin("delete", `/admin/users/${u.id}`);
  }
});
