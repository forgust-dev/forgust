import { createHmac } from "node:crypto";
import type { Browser, Page } from "@playwright/test";
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
    await me.getByRole("button", { name: "Sign in", exact: true }).click();
  };
  try {
    await signIn();
    await expect.poll(() => me.evaluate(() => location.pathname)).not.toBe("/login");
    await me.goto("/profile/security");
    const app = me.locator(".card", { hasText: "Authenticator app" });
    await app.getByLabel("Confirm with your password").fill(pw);
    await app.getByRole("button", { name: "Set up 2FA" }).click();
    const secret = (await app.locator("code").first().textContent())!.trim();
    await app.getByLabel("Code").fill(totp(secret));
    await app.getByRole("button", { name: "Enable" }).click();

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

/** A signed-in browser of a new account, and how to remove the account after. */
async function newAccount(page: Page, browser: Browser, prefix: string) {
  const admin = await apiCall(page);
  const username = `${prefix}${Date.now()}`;
  const pw = "a-long-test-password";
  const u = await admin("post", "/admin/users", { username, email: `${username}@example.com`, password: pw });
  const ctx = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const me = await ctx.newPage();
  const signIn = async () => {
    await me.goto("/login");
    await me.getByLabel("Username or email").fill(username);
    await me.getByLabel("Password").fill(pw);
    await me.getByRole("button", { name: "Sign in", exact: true }).click();
  };
  const done = async () => {
    await ctx.close();
    await admin("delete", `/admin/users/${u.id}`);
  };
  return { me, ctx, username, pw, signIn, done };
}

/** Chromium's virtual authenticator (WebAuthn over CDP), in this page. */
async function virtualAuthenticator(me: Page, passkey: boolean) {
  const cdp = await me.context().newCDPSession(me);
  await cdp.send("WebAuthn.enable");
  const { authenticatorId } = await cdp.send("WebAuthn.addVirtualAuthenticator", {
    options: { protocol: "ctap2", transport: passkey ? "internal" : "usb", hasResidentKey: passkey, hasUserVerification: true, isUserVerified: true, automaticPresenceSimulation: true },
  });
  return { cdp, authenticatorId };
}

const webauthnAvailable = async (page: Page) => (await (await page.request.get("/api/v1/setup")).json()).webauthn as boolean;

test("security key: a second factor after the password", async ({ page, browser }) => {
  test.skip(!(await webauthnAvailable(page)), "WebAuthn needs the server at https or localhost");
  const { me, pw, signIn, done } = await newAccount(page, browser, "key");
  try {
    await virtualAuthenticator(me, false);
    await signIn();
    await expect.poll(() => me.evaluate(() => location.pathname)).not.toBe("/login");
    await me.goto("/profile/security");
    const keys = me.locator(".card", { hasText: "Security keys" });
    await keys.getByLabel("Name").fill("Virtual key");
    await keys.getByLabel("Confirm with your password").fill(pw);
    await keys.getByRole("button", { name: "Add a security key" }).click();
    await expect(keys.getByText("Security key added.")).toBeVisible();
    await expect(keys.getByText("Virtual key")).toBeVisible();
    // the first second factor: recovery codes, this once
    await expect(me.getByText("Save your recovery codes")).toBeVisible();
    await me.getByRole("button", { name: "I saved them" }).click();
    await expect(me.getByRole("heading", { name: /^Two-factor authentication/ })).toContainText("Enabled");

    // the password, then the key
    await me.context().clearCookies();
    await signIn();
    await me.getByRole("button", { name: "Use your security key" }).click();
    await expect.poll(() => me.evaluate(() => location.pathname)).not.toBe("/login");

    // removed (the password again): two-factor is off
    await me.goto("/profile/security");
    await keys.getByRole("button", { name: "Remove…" }).click();
    await keys.getByLabel("Your password", { exact: true }).fill(pw);
    await keys.getByRole("button", { name: "Remove", exact: true }).click();
    await expect(keys.getByText("Virtual key removed.")).toBeVisible();
    await expect(me.getByRole("heading", { name: /^Two-factor authentication/ })).toContainText("Off");
  } finally {
    await done();
  }
});

test("passkey: sign in without a password", async ({ page, browser }) => {
  test.skip(!(await webauthnAvailable(page)), "WebAuthn needs the server at https or localhost");
  const { me, username, pw, signIn, done } = await newAccount(page, browser, "pk");
  try {
    await virtualAuthenticator(me, true);
    await signIn();
    await expect.poll(() => me.evaluate(() => location.pathname)).not.toBe("/login");
    await me.goto("/profile/security");
    const passkeys = me.locator(".card", { hasText: "Your passkeys" });
    await passkeys.getByLabel("Name").fill("This laptop");
    await passkeys.getByLabel("Confirm with your password").fill(pw);
    await passkeys.getByRole("button", { name: "Add a passkey" }).click();
    await expect(passkeys.getByText("Passkey added.")).toBeVisible();
    await expect(passkeys.getByText("never used")).toBeVisible();

    // signed out: no username, no password
    await me.context().clearCookies();
    await me.goto("/login");
    await me.getByRole("button", { name: "Sign in with a passkey" }).click();
    await expect.poll(() => me.evaluate(() => location.pathname)).not.toBe("/login");
    const who = await (await me.request.get("/api/v1/user")).json();
    expect(who.username).toBe(username);
    await me.goto("/profile/security");
    await expect(passkeys.getByText(/last used/)).toBeVisible();
  } finally {
    await done();
  }
});
