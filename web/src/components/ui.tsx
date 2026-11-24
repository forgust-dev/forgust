import { Component, useEffect, useLayoutEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import { Check, Copy, EyeOff, Loader2, Lock, Search, type LucideIcon } from "lucide-react";
import { Link, NavLink } from "react-router-dom";
import { ApiError, LEVELS, bytes } from "../api/client";
import { Select } from "./controls";
import { useConfirm, type Ask } from "../shell/dialog";

/** How long a wait goes unmarked: a page that comes sooner never shows a loader. */
const SPINNER_DELAY = 400;
// One wait can pass through several loaders (the session, a page's code, then its data):
// they share when it began, so the next one goes on where the last one was.
let waitBegan = 0;
let loaders = 0;
let lastGone = -Infinity;

/** A loader that shows only once a wait has lasted SPINNER_DELAY (styles.css fades it in). */
export function Spinner({ label = "Loading…" }: { label?: string }) {
  const ref = useRef<HTMLDivElement>(null);
  // timed when it reaches the screen, not when it renders: React can hold a render back
  // (a page's code arrived just after its fallback showed), and a loader hidden by a
  // Suspense fallback comes back through here too
  useLayoutEffect(() => {
    const now = performance.now();
    // a new wait unless another loader is on screen or just left it
    if (loaders === 0 && now - lastGone > 100) waitBegan = now;
    loaders++;
    const waited = now - waitBegan;
    const el = ref.current;
    if (el) {
      el.style.animationDelay = `${SPINNER_DELAY - waited}ms`;
      // the arc goes on turning from where the last loader's was
      const icon = el.firstElementChild as SVGElement | null;
      if (icon) icon.style.animationDelay = `${-waited}ms`;
    }
    return () => {
      loaders--;
      lastGone = performance.now();
    };
  }, []);
  return (
    <div className="spinner" ref={ref}>
      <Loader2 size={16} className="spin" aria-hidden="true" />
      {label}
    </div>
  );
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

/**
 * A page that fails to render shows what happened, in the app around it,
 * instead of leaving the whole window blank; it tries again once `resetKey`
 * (the address) changes. Not a React key: the pages keep their state.
 */
export class PageErrorBoundary extends Component<{ resetKey: string; children: ReactNode }, { error: unknown }> {
  state: { error: unknown } = { error: null };

  static getDerivedStateFromError(error: unknown) {
    return { error };
  }

  componentDidCatch(error: unknown) {
    console.error(error);
  }

  componentDidUpdate(prev: { resetKey: string }) {
    if (this.state.error != null && prev.resetKey !== this.props.resetKey) this.setState({ error: null });
  }

  render() {
    if (this.state.error == null) return this.props.children;
    return (
      <div className="empty">
        <h2>This page could not be shown</h2>
        <p>{this.state.error instanceof Error ? this.state.error.message : String(this.state.error)}</p>
        <Link to="/">Back to your dashboard</Link>
      </div>
    );
  }
}

/** What a state means, one color each on every page (styles.css, "states"): fine, broke, waits,
 *  runs, merged, or nothing to say. */
export type Tone = "ok" | "danger" | "warn" | "info" | "merged" | "neutral";

/** A thing's state, as the UI convention draws it: a pill tinted with its tone, its icon and its
 *  word; a spinner (Loader2) turns. `className` adds what styles and tests key on (`mr-state opened`). */
export function StateBadge({ tone, icon: Icon, children, className, title }: { tone: Tone; icon?: LucideIcon; children: ReactNode; className?: string; title?: string }) {
  return (
    <span className={`state tone-${tone}${className ? ` ${className}` : ""}`} title={title}>
      {Icon && <Icon size={13} aria-hidden className={Icon === Loader2 ? "spin" : undefined} />}
      {children}
    </span>
  );
}

/** A property of a thing (a role, its visibility, a kind), as the UI convention draws it: a fine
 *  grey outline, never colored, so that it never reads as a state. */
export function Badge({ children }: { children: ReactNode }) {
  return <span className="badge">{children}</span>;
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
        <CopyLabel copied={copied} />
      </button>
    </div>
  );
}

/** "Copy", or "Copied" for a moment, in one cell: the button keeps its width, nothing beside it moves. */
export function CopyLabel({ copied }: { copied: boolean }) {
  return (
    <span className="copy-label" data-copied={copied || undefined}>
      <span>Copy</span>
      <span>Copied</span>
    </span>
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
 * and calls `onDone` (its cache refresh), or shows the error. With `ask`, it
 * asks first, in the app's dialog: cancelling changes nothing and says nothing.
 */
export function useFormRun(onDone: () => void = () => {}) {
  const [msg, setMsg] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const confirm = useConfirm();
  const run = (f: () => Promise<unknown>, done: string, ask?: Omit<Ask, "input">) => async (e?: FormEvent) => {
    e?.preventDefault();
    if (ask && !(await confirm(ask))) return;
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
export function SearchField({ value, onChange, placeholder, autoFocus }: { value: string; onChange: (v: string) => void; placeholder: string; autoFocus?: boolean }) {
  return (
    <label className="search-field">
      <Search size={14} aria-hidden />
      <input type="search" value={value} placeholder={placeholder} aria-label={placeholder} autoFocus={autoFocus} onChange={(e) => onChange(e.target.value)} />
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
        {copied ? <Check size={13} className="copied-icon" /> : <Copy size={13} />}
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

type Authored = { author_username?: string | null; author_name?: string | null; imported_author?: string | null; author_bot?: boolean };

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
  // bots have no profile page
  return x.author_username && !x.author_bot ? <UserLink username={x.author_username}>{label}</UserLink> : <>{label}</>;
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

/** A confidential issue: seen only by Reporters and up, its author and its assignees. */
export function ConfidentialMark() {
  return (
    <span className="pill confidential-mark" title="Confidential: only Reporters and up, the author and assignees can see it">
      <Lock size={11} aria-hidden /> Confidential
    </span>
  );
}

/** An internal comment: seen only by the project's Reporters and up. */
export function InternalMark() {
  return (
    <span className="pill internal-mark" title="Internal: only Reporters and up can see this comment">
      <EyeOff size={11} aria-hidden /> Internal
    </span>
  );
}

/** The "Internal comment" checkbox of a comment form; render it only when the viewer may comment internally. */
export function InternalToggle({ checked, onChange, grow }: { checked: boolean; onChange: (checked: boolean) => void; grow?: boolean }) {
  return (
    <label className={grow ? "check grow small" : "check small"} title="Only Reporters and up see it">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      Internal comment
    </label>
  );
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

