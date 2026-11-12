import { createHash } from "node:crypto";
import { test, expect, P, isMobile } from "./fixtures";

// An image pushed over /v2/ (as docker does) shows on the project's
// Container registry page, where its tag is deleted.

const sha = (b: Buffer | string) => `sha256:${createHash("sha256").update(b).digest("hex")}`;

test("an image pushed over the registry API shows and its tag can be deleted", async ({ page, request }) => {
  const name = P.slice(1);
  const tag = `e2e-${Date.now() % 1_000_000}`;
  const auth = `Basic ${Buffer.from("root:rootpassword").toString("base64")}`;
  const tok = await (await request.get(`/jwt/auth?service=container_registry&scope=repository:${name}:pull,push`, { headers: { authorization: auth } })).json();
  const bearer = { authorization: `Bearer ${tok.token}` };
  const config = Buffer.from(`{"architecture":"amd64","os":"linux","tag":"${tag}"}`);
  const layer = Buffer.from(`layer of ${tag}`);
  for (const b of [config, layer]) {
    const r = await request.post(`/v2/${name}/blobs/uploads/?digest=${sha(b)}`, { headers: bearer, data: b });
    expect(r.status()).toBe(201);
  }
  const manifest = JSON.stringify({
    schemaVersion: 2,
    mediaType: "application/vnd.oci.image.manifest.v1+json",
    config: { mediaType: "application/vnd.oci.image.config.v1+json", digest: sha(config), size: config.length },
    layers: [{ mediaType: "application/vnd.oci.image.layer.v1.tar+gzip", digest: sha(layer), size: layer.length }],
  });
  const put = await request.put(`/v2/${name}/manifests/${tag}`, {
    headers: { ...bearer, "content-type": "application/vnd.oci.image.manifest.v1+json" },
    data: manifest,
  });
  expect(put.status()).toBe(201);

  await page.goto(`${P}/-/container_registry`);
  await expect(page.getByText("Push an image")).toBeVisible();
  const image = page.getByRole("region", { name: new RegExp(`Image .*/${name}$`) });
  await image.getByRole("button", { name: new RegExp(`/${name}$`) }).click();
  const row = image.locator(".item", { hasText: tag });
  await expect(row).toBeVisible();
  await row.getByRole("button", { name: "Delete" }).click();
  await expect(image.locator(".item", { hasText: tag })).toHaveCount(0);
  const gone = await request.get(`/v2/${name}/manifests/${tag}`, { headers: bearer });
  expect(gone.status()).toBe(404);
  if (!isMobile(page)) await expect(page.getByRole("link", { name: "Container registry" })).toBeVisible();
});
