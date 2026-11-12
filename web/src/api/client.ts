import type { components } from "./schema";

export type S = components["schemas"];

export class ApiError extends Error {
  status: number;
  code: string;
  constructor(status: number, code: string, message: string) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

let csrf: string | null = null;
export function setCsrf(token: string | null) {
  csrf = token;
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = {};
  // a file goes as it is (avatars), a form as multipart (uploads); anything else as JSON
  const raw = body instanceof Blob || body instanceof FormData;
  if (body !== undefined && !(body instanceof FormData)) headers["content-type"] = body instanceof Blob ? body.type || "application/octet-stream" : "application/json";
  if (method !== "GET" && csrf) headers["x-csrf-token"] = csrf;
  const res = await fetch(`/api/v1${path}`, {
    method,
    headers,
    body: body === undefined ? undefined : raw ? (body as Blob | FormData) : JSON.stringify(body),
    credentials: "same-origin",
  });
  const text = await res.text();
  const data = text ? JSON.parse(text) : null;
  if (!res.ok) {
    throw new ApiError(res.status, data?.error ?? "error", data?.message ?? res.statusText);
  }
  return data as T;
}

export const api = {
  get: <T>(path: string) => request<T>("GET", path),
  post: <T>(path: string, body: unknown = {}) => request<T>("POST", path, body),
  put: <T>(path: string, body: unknown) => request<T>("PUT", path, body),
  del: <T>(path: string) => request<T>("DELETE", path),
  /** A file uploaded to a project: to show or link in Markdown, or attach to a release. */
  upload: (projectId: number, file: File) => {
    const form = new FormData();
    form.append("file", file, file.name);
    return request<S["Uploaded"]>("POST", `/projects/${projectId}/uploads`, form);
  },
};

/** Encode a full path (`acme/platform/api`) for `{id}` URL segments. */
export const enc = (p: string | number) => encodeURIComponent(String(p));
/** A path whose slashes stay slashes (branch names, file paths), each segment encoded. */
export const encPath = (p: string) => p.split("/").map(encodeURIComponent).join("/");

export function qs(params: Record<string, string | number | boolean | undefined | null>): string {
  const u = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v !== undefined && v !== null && v !== "") u.set(k, String(v));
  }
  const s = u.toString();
  return s ? `?${s}` : "";
}

export const LEVELS = ["guest", "reporter", "developer", "maintainer", "owner"] as const;

export function timeAgo(ts: number | string): string {
  const t = typeof ts === "number" ? ts * 1000 : Date.parse(ts);
  const s = Math.max(0, (Date.now() - t) / 1000);
  if (s < 60) return "just now";
  const units: [number, string][] = [
    [60, "minute"],
    [3600, "hour"],
    [86400, "day"],
    [86400 * 30, "month"],
    [86400 * 365, "year"],
  ];
  let unit = units[0];
  for (const u of units) if (s >= u[0]) unit = u;
  const n = Math.floor(s / unit[0]);
  return `${n} ${unit[1]}${n === 1 ? "" : "s"} ago`;
}

export function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const u = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v < 10 ? 1 : 0)} ${u[i]}`;
}
