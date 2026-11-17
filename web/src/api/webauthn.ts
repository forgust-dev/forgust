// WebAuthn in the browser: the server's options (JSON, binary fields in
// base64url) to what navigator.credentials expects, and its answer back to JSON.

type Json = Record<string, unknown>;

function fromB64url(s: string): ArrayBuffer {
  const b64 = s.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(s.length / 4) * 4, "=");
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out.buffer;
}

function toB64url(buf: ArrayBuffer | null): string | null {
  if (!buf) return null;
  let bin = "";
  for (const b of new Uint8Array(buf)) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

const withIds = (list: unknown) => ((list as Json[] | undefined) ?? []).map((c) => ({ ...c, id: fromB64url(c.id as string) }));

/** Whether this browser can do WebAuthn at all. */
export const webauthnSupported = () => typeof window !== "undefined" && "PublicKeyCredential" in window && !!navigator.credentials;

/** `navigator.credentials.create` with the server's options; the new credential as JSON. */
export async function createCredential(options: unknown): Promise<Json> {
  const o = (options as { publicKey: Json }).publicKey;
  const user = o.user as Json;
  const cred = (await navigator.credentials.create({
    publicKey: {
      ...(o as unknown as PublicKeyCredentialCreationOptions),
      challenge: fromB64url(o.challenge as string),
      user: { ...(user as unknown as PublicKeyCredentialUserEntity), id: fromB64url(user.id as string) },
      excludeCredentials: withIds(o.excludeCredentials) as PublicKeyCredentialDescriptor[],
    },
  })) as PublicKeyCredential | null;
  if (!cred) throw new Error("No credential was made.");
  const r = cred.response as AuthenticatorAttestationResponse;
  return {
    id: cred.id,
    rawId: toB64url(cred.rawId),
    type: cred.type,
    response: {
      attestationObject: toB64url(r.attestationObject),
      clientDataJSON: toB64url(r.clientDataJSON),
      transports: r.getTransports?.() ?? undefined,
    },
    extensions: cred.getClientExtensionResults(),
  };
}

/** `navigator.credentials.get` with the server's options; the answer as JSON. */
export async function getCredential(options: unknown): Promise<Json> {
  const o = (options as { publicKey: Json }).publicKey;
  const cred = (await navigator.credentials.get({
    publicKey: {
      ...(o as unknown as PublicKeyCredentialRequestOptions),
      challenge: fromB64url(o.challenge as string),
      allowCredentials: withIds(o.allowCredentials) as PublicKeyCredentialDescriptor[],
    },
  })) as PublicKeyCredential | null;
  if (!cred) throw new Error("No credential was chosen.");
  const r = cred.response as AuthenticatorAssertionResponse;
  return {
    id: cred.id,
    rawId: toB64url(cred.rawId),
    type: cred.type,
    response: {
      authenticatorData: toB64url(r.authenticatorData),
      clientDataJSON: toB64url(r.clientDataJSON),
      signature: toB64url(r.signature),
      userHandle: toB64url(r.userHandle),
    },
    extensions: cred.getClientExtensionResults(),
  };
}

/** The user closed the browser's prompt (or it timed out): nothing to report. */
export const cancelled = (e: unknown) => e instanceof DOMException && (e.name === "NotAllowedError" || e.name === "AbortError");
