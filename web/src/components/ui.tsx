import { useEffect, useState, type FormEvent, type ReactNode } from "react";
import { Check, Copy, Search } from "lucide-react";
import { Link, NavLink } from "react-router-dom";
import { ApiError, LEVELS, bytes } from "../api/client";
import { Select } from "./controls";

export function Spinner({ label = "Loading…" }: { label?: string }) {
  return <div className="spinner">{label}</div>;
}

export function ErrorBox({ error }: { error: unknown }) {
  if (!error) return null;
  const msg = error instanceof ApiError ? error.message : String(error);
  return <div className="alert error">{msg}</div>;
}

/** Loading / error / content in one place for queries. */
export function Q<T>({ q, children }: { q: { isLoading: boolean; error: unknown; data: T | undefined }; children: (d: T) => ReactNode }) {
  if (q.isLoading) return <Spinner />;
  if (q.error) {
    if (q.error instanceof ApiError && q.error.status === 404) return <NotFound />;
    return <ErrorBox error={q.error} />;
  }
  if (q.data === undefined) return null;
  return <>{children(q.data)}</>;
}

export function NotFound() {
  return (
    <div className="empty">
      <h2>Not found</h2>
      <p>This page does not exist, or you do not have access to it.</p>
      <Link to="/">Back to your dashboard</Link>
    </div>
  );
}

export function Empty({ children }: { children: ReactNode }) {
  return <div className="empty">{children}</div>;
}

export function Badge({ children, tone }: { children: ReactNode; tone?: "accent" | "ok" | "warn" }) {
  return <span className={`badge ${tone ?? ""}`}>{children}</span>;
}

export function Field({ label, hint, children }: { label: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <label className="field">
      {label}
      {children}
      {hint && <span className="hint">{hint}</span>}
    </label>
  );
}

/** Like Field, for groups of controls (a label element would name them all). */
export function FieldGroup({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <div className="field" role="group" aria-label={label}>
      <span>{label}</span>
      {children}
      {hint && <span className="hint">{hint}</span>}
    </div>
  );
}

export function Card({ title, actions, children }: { title?: ReactNode; actions?: ReactNode; children: ReactNode }) {
  return (
    <div className="card">
      {title && (
        <div className="card-head">
          <strong className="grow">{title}</strong>
          {actions}
        </div>
      )}
      <div className="card-body">{children}</div>
    </div>
  );
}

export function Tabs({ items }: { items: { to: string; label: string; end?: boolean }[] }) {
  return (
    <nav className="tabs">
      {items.map((i) => (
        <NavLink key={i.to} to={i.to} end={i.end} className={({ isActive }) => (isActive ? "active" : "")}>
          {i.label}
        </NavLink>
      ))}
    </nav>
  );
}

/** `[copied, copy]`: copy text (or what a function yields) and say so for a moment. */
export function useCopy(ms = 1200): [boolean, (text: string | (() => Promise<string>)) => Promise<void>] {
  const [copied, setCopied] = useState(false);
  const copy = async (text: string | (() => Promise<string>)) => {
    await navigator.clipboard?.writeText(typeof text === "string" ? text : await text());
    setCopied(true);
    setTimeout(() => setCopied(false), ms);
  };
  return [copied, copy];
}

/** `v`, once it has stopped changing for `ms`. */
export function useDebounced<T>(v: T, ms: number) {
  const [d, setD] = useState(v);
  useEffect(() => {
    const t = setTimeout(() => setD(v), ms);
    return () => clearTimeout(t);
  }, [v, ms]);
  return d;
}

export function CopyField({ value }: { value: string }) {
  const [copied, copy] = useCopy();
  return (
    <div className="clone">
      <input readOnly value={value} onFocus={(e) => e.currentTarget.select()} aria-label="Copy value" />
      <button
        className="btn"
        type="button"
        onClick={() => copy(value)}
      >
        {copied ? "Copied" : "Copy"}
      </button>
    </div>
  );
}

export const VISIBILITY_OPTIONS = [
  { value: "private", label: "Private", hint: "Members only" },
  { value: "internal", label: "Internal", hint: "Any signed-in user" },
  { value: "public", label: "Public", hint: "Anyone" },
];

export function VisibilitySelect({ value, onChange, label }: { value: string; onChange: (v: string) => void; label?: string }) {
  return <Select value={value} onChange={onChange} options={VISIBILITY_OPTIONS} label={label} />;
}

/**
 * A settings form's submit: clears the messages, runs `f`, then says `done`
 * and calls `onDone` (its cache refresh), or shows the error.
 */
export function useFormRun(onDone: () => void = () => {}) {
  const [msg, setMsg] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const run = (f: () => Promise<unknown>, done: string) => async (e?: FormEvent) => {
    e?.preventDefault();
    setError(null);
    setMsg(null);
    try {
      await f();
      setMsg(done);
      onDone();
    } catch (err) {
      setError(err);
    }
  };
  return { run, msg, error };
}

export const levelName = (l: string) => l[0].toUpperCase() + l.slice(1).replace("_", " ");

export function LevelSelect({ value, onChange, levels, label }: { value: string; onChange: (v: string) => void; levels?: readonly string[]; label?: string }) {
  const all = levels ?? LEVELS;
  return <Select value={value} onChange={onChange} options={all.map((l) => ({ value: l, label: levelName(l) }))} label={label} />;
}

/** One-time secret display (tokens are shown once). */
export function NewSecret({ token, username }: { token: string; username?: string | null }) {
  return (
    <div className="token-box stack" style={{ gap: 6 }}>
      <strong>Copy this token now. It will not be shown again.</strong>
      {username && (
        <span>
          Username: <code>{username}</code>
        </span>
      )}
      <CopyField value={token} />
    </div>
  );
}

/** The top of a list page: a title, an optional line under it, actions on the right. */
export function ListHead({ title, sub, children }: { title: ReactNode; sub?: ReactNode; children?: ReactNode }) {
  return (
    <header className="list-head">
      <div className="grow" style={{ minWidth: 0 }}>
        <h1>{title}</h1>
        {sub && <div className="list-sub">{sub}</div>}
      </div>
      {children && <div className="list-actions">{children}</div>}
    </header>
  );
}

/** Underlined tabs that filter a list (state, status…), with optional counts. */
export function FilterTabs<T extends string>({ value, onChange, options, label }: { value: T; onChange: (v: T) => void; options: { value: T; label: ReactNode; count?: number }[]; label: string }) {
  return (
    <nav className="tabs filter-tabs" aria-label={label}>
      {options.map((o) => (
        <button key={o.value} type="button" className={o.value === value ? "active" : ""} aria-pressed={o.value === value} onClick={() => onChange(o.value)}>
          {o.label}
          {o.count != null && <span className="tab-count">{o.count}</span>}
        </button>
      ))}
    </nav>
  );
}

/** A filter input with a search icon. */
export function SearchField({ value, onChange, placeholder }: { value: string; onChange: (v: string) => void; placeholder: string }) {
  return (
    <label className="search-field">
      <Search size={14} aria-hidden />
      <input type="search" value={value} placeholder={placeholder} aria-label={placeholder} onChange={(e) => onChange(e.target.value)} />
    </label>
  );
}

/** A short sha that copies the full one. */
export function ShaCopy({ sha, to }: { sha: string; to?: string }) {
  const [copied, copy] = useCopy();
  return (
    <span className="sha-copy">
      {to ? (
        <Link to={to} className="sha">
          {sha.slice(0, 8)}
        </Link>
      ) : (
        <span className="sha">{sha.slice(0, 8)}</span>
      )}
      <button
        type="button"
        className="icon-btn"
        title="Copy the full SHA"
        aria-label="Copy the full SHA"
        onClick={() => copy(sha)}
      >
        {copied ? <Check size={13} /> : <Copy size={13} />}
      </button>
    </span>
  );
}

/**
 * A block of a settings page: what it is about on the left (title and a
 * sentence), its controls on the right; stacked on small screens.
 */
export function Section({ title, description, actions, danger, id, children }: { title: ReactNode; description?: ReactNode; actions?: ReactNode; danger?: boolean; id?: string; children: ReactNode }) {
  return (
    <section id={id} className={`settings-section ${danger ? "danger" : ""}`}>
      <div className="settings-intro">
        <h2>{title}</h2>
        {description && <p>{description}</p>}
        {actions && <div className="settings-actions">{actions}</div>}
      </div>
      <div className="settings-body">{children}</div>
    </section>
  );
}

/** A stable hue per name, so people keep their avatar colour. */
export function hue(name: string) {
  let h = 0;
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) % 360;
  return h;
}

/** Avatar URLs that answered 404 in this page's life: not asked again. */
const missing = new Set<string>();

/** An image laid over a fallback (initials) once it has loaded; never a broken icon. */
function Photo({ src }: { src?: string | null }) {
  const [shown, setShown] = useState(false);
  const [gone, setGone] = useState(() => !src || missing.has(src));
  if (!src || gone) return null;
  return (
    <img
      src={src}
      alt=""
      loading="lazy"
      decoding="async"
      className={shown ? "shown" : undefined}
      onLoad={() => setShown(true)}
      onError={() => {
        missing.add(src);
        setGone(true);
      }}
    />
  );
}

/**
 * A person's picture, or their initial on their colour. With `username`, the
 * picture is theirs when they set one (`src` overrides, e.g. a versioned URL).
 */
export function Avatar({ name, ok, title, username, src, className }: { name: string; ok?: boolean; title?: string; username?: string | null; src?: string | null; className?: string }) {
  const url = src ?? (username ? `/api/v1/users/${encodeURIComponent(username)}/avatar` : null);
  return (
    <span className={`avatar ${ok ? "ok" : ""} ${className ?? ""}`} title={title} style={{ "--h": hue(name) } as React.CSSProperties} aria-hidden={title ? undefined : true}>
      {(name[0] ?? "?").toUpperCase()}
      <Photo key={url ?? ""} src={url} />
    </span>
  );
}

/** A project's or group's picture, or its initial on a tile. */
export function Tile({ name, src, group, big }: { name: string; src?: string | null; group?: boolean; big?: boolean }) {
  return (
    <span className={`tile${group ? " group" : ""}${big ? " big" : ""}`}>
      {name[0]?.toUpperCase()}
      <Photo key={src ?? ""} src={src} />
    </span>
  );
}

type Authored = { author_username?: string | null; author_name?: string | null; imported_author?: string | null };

/** A person's name linking to their profile at `/<username>`. */
export function UserLink({ username, children, className }: { username: string; children?: ReactNode; className?: string }) {
  return (
    <Link to={`/${username}`} className={`user-link${className ? ` ${className}` : ""}`} onClick={(e) => e.stopPropagation()}>
      {children ?? username}
    </Link>
  );
}

/** A commit's author or committer: a link to their profile when the server
 *  matched the email to an account, else the name git recorded. */
export function GitPerson({ p }: { p: { name: string; email: string; username?: string | null } }) {
  return p.username ? (
    <UserLink username={p.username} className="git-person">
      {p.name || p.username}
    </UserLink>
  ) : (
    <span title={p.email}>{p.name}</span>
  );
}

/**
 * Who wrote something, linked to their profile; plain text for authors who
 * only exist on the forge it was imported from. `name` shows the display name.
 */
export function AuthorLink({ x, name }: { x: Authored; name?: boolean }) {
  const label = name ? noteAuthor(x) : authorOf(x);
  return x.author_username ? <UserLink username={x.author_username}>{label}</UserLink> : <>{label}</>;
}

/** Who wrote something: their username here, or their name on the forge it was imported from. */
export function authorOf(x: Authored, fallback = "ghost") {
  return x.author_username ?? x.imported_author ?? fallback;
}

/** Display name for comments: name, username, or the imported name. */
export function noteAuthor(x: Authored) {
  return x.author_name || x.author_username || x.imported_author || "ghost";
}

/** Marks content whose author has no account here (it came from an import). */
export function ImportedMark({ x }: { x: Authored }) {
  return !x.author_username && x.imported_author ? (
    <span className="pill" title="Imported: this person has no account here">
      imported
    </span>
  ) : null;
}

/**
 * Previous / next for lists paged without a total: shown once there is more than
 * one page (`count` items on this page of `perPage`).
 */
export function Pager({ page, count, perPage, onPage, labels = ["Previous", "Next"] }: { page: number; count: number; perPage: number; onPage: (p: number) => void; labels?: [string, string] }) {
  if (page === 1 && count < perPage) return null;
  return (
    <div className="pager">
      <button className="btn" disabled={page === 1} onClick={() => onPage(page - 1)}>
        {labels[0]}
      </button>
      <span className="faint small">Page {page}</span>
      <button className="btn" disabled={count < perPage} onClick={() => onPage(page + 1)}>
        {labels[1]}
      </button>
    </div>
  );
}

/** What a storage quota holds: a bar, then "1.2 GB of 2 GB (60 %)". */
export function QuotaBar({ used, quota, label }: { used: number; quota: number; label: string }) {
  const pct = quota > 0 ? Math.min(100, Math.round((used / quota) * 100)) : 0;
  const tone = pct >= 100 ? "bad" : pct >= 80 ? "warn" : "ok";
  return (
    <div className="quota">
      <div className="quota-head small">
        <span>{label}</span>
        <span className="muted">
          {bytes(used)} of {bytes(quota)} ({pct} %)
        </span>
      </div>
      <div className="quota-bar" role="meter" aria-label={label} aria-valuemin={0} aria-valuemax={quota} aria-valuenow={used}>
        <span className={tone} style={{ width: `${pct}%` }} />
      </div>
    </div>
  );
}

