import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useQuery } from "@tanstack/react-query";
import { ArrowRight, CalendarDays, CircleDot, GitBranch, GitCommitHorizontal, GitMerge, Lock, ShieldAlert, ShieldCheck, Tag, Timer } from "lucide-react";
import { api, timeAgo, type S } from "../api/client";
import { fetchers, keys } from "../prefetch";
import { Avatar } from "./ui";
import { LabelList } from "./labels";
import { StatusIcon, duration, statusLabel } from "../pages/project/ci/Status";
import { SOURCE } from "../pages/project/ci/Pipelines";
import { SIGNATURE_TEXT } from "../pages/project/Signatures";

type Item = { kind: "mr" | "issue" | "pipeline" | "job"; path: string; iid: number };
type Target =
  | Item
  | { kind: "user"; username: string }
  | { kind: "commit"; path: string; sha: string }
  | { kind: "branch"; path: string; name: string }
  | { kind: "signature"; v: S["Verification"] };

const KINDS: Record<string, Item["kind"]> = { merge_requests: "mr", issues: "issue", pipelines: "pipeline", jobs: "job" };

/**
 * What a link points at: a merge request, issue, pipeline or job of a project
 * (`/<path>/-/<kind>/<n>`, its sub-pages too), or a person (links marked as
 * such: `/<name>` is also a group's or a project's path).
 */
function targetOf(a: HTMLAnchorElement): Target | null {
  const url = new URL(a.href, location.origin);
  if (url.origin !== location.origin) return null;
  // the page's own tabs and navigation say where they lead already
  if (a.closest(".tabs, nav, .breadcrumbs, .hover-card, [role=tablist]")) return null;
  const m = url.pathname.match(/^\/(.+?)\/-\/(merge_requests|issues|pipelines|jobs)\/(\d+)(\/[a-z_]*)?\/?$/);
  if (m) {
    const here = location.pathname.match(/^\/(.+?)\/-\/(merge_requests|issues|pipelines|jobs)\/(\d+)/);
    if (here && here[1] === m[1] && here[2] === m[2] && here[3] === m[3]) return null;
    return { kind: KINDS[m[2]], path: decodeURIComponent(m[1]), iid: Number(m[3]) };
  }
  const c = url.pathname.match(/^\/(.+?)\/-\/commit\/([0-9a-f]{7,40})\/?$/);
  if (c) {
    // a commit list already shows what the card would
    if (location.pathname.startsWith(`/${c[1]}/-/commit/${c[2]}`) || a.closest(".commit-row")) return null;
    return { kind: "commit", path: decodeURIComponent(c[1]), sha: c[2] };
  }
  // branch chips (merge requests, pipelines): their link is the ref alone, while
  // other tree links may also be a folder
  const b = a.classList.contains("ref-chip") ? url.pathname.match(/^\/(.+?)\/-\/tree\/(.+)$/) : null;
  if (b) return { kind: "branch", path: decodeURIComponent(b[1]), name: decodeURIComponent(b[2]) };
  if (a.classList.contains("user-mention") || a.classList.contains("user-link")) {
    const u = url.pathname.match(/^\/([^/]+)\/?$/);
    if (u) return { kind: "user", username: decodeURIComponent(u[1]) };
  }
  return null;
}

/** Links that may get a card (targetOf decides), and signature badges (their check in `data-signature`). */
const SELECTOR = "a[href], [data-signature]";

/** What an element shows a card for: a link's target, or a signature badge's check. */
function cardOf(el: HTMLElement): Target | null {
  if (el.dataset.signature) {
    try {
      return { kind: "signature", v: JSON.parse(el.dataset.signature) };
    } catch {
      return null;
    }
  }
  return el instanceof HTMLAnchorElement ? targetOf(el) : null;
}
const SHOW_MS = 350;
const HIDE_MS = 200;
const TIP_MS = 300;

/** A plain tooltip, styled like the cards: for the `title` of anything that has no card. */
function Tip({ text, rect }: { text: string; rect: DOMRect }) {
  const above = rect.top > 44;
  const left = Math.max(8, Math.min(rect.left + rect.width / 2, window.innerWidth - 8));
  return createPortal(
    <div className={`tip ${above ? "above" : "below"}`} role="tooltip" style={{ left, top: above ? rect.top - 6 : rect.bottom + 6 }}>
      {text}
    </div>,
    document.body,
  );
}

/**
 * Plain tooltips: an element's `title` shows in a styled tip instead of the
 * browser's, after a short delay. The title moves to `data-title` (and to
 * `aria-label` when the element has no text of its own, so screen readers
 * keep it).
 */
function useTips() {
  const [tip, setTip] = useState<{ text: string; rect: DOMRect } | null>(null);
  const t = useRef<number | undefined>(undefined);
  useEffect(() => {
    const enter = (e: Event) => {
      const el = (e.target as Element | null)?.closest?.("[title], [data-title]") as HTMLElement | SVGElement | null;
      if (!el || el.closest(".hover-card")) return;
      if (el instanceof HTMLAnchorElement && targetOf(el)) return;
      const title = el.getAttribute("title");
      if (title) {
        el.setAttribute("data-title", title);
        el.removeAttribute("title");
        if (!el.getAttribute("aria-label") && !el.textContent?.trim()) el.setAttribute("aria-label", title);
      }
      const text = el.getAttribute("data-title");
      if (!text) return;
      clearTimeout(t.current);
      t.current = window.setTimeout(() => setTip({ text, rect: el.getBoundingClientRect() }), TIP_MS);
    };
    const leave = (e: Event) => {
      const el = (e.target as Element | null)?.closest?.("[data-title]");
      if (!el) return;
      // moving between the element's own parts keeps the tip
      const to = (e as MouseEvent).relatedTarget as Node | null;
      if (to && el.contains(to)) return;
      clearTimeout(t.current);
      setTip(null);
    };
    const close = () => {
      clearTimeout(t.current);
      setTip(null);
    };
    document.addEventListener("mouseover", enter);
    document.addEventListener("mouseout", leave);
    document.addEventListener("focusin", enter);
    document.addEventListener("focusout", leave);
    document.addEventListener("mousedown", close);
    window.addEventListener("scroll", close, true);
    window.addEventListener("popstate", close);
    return () => {
      document.removeEventListener("mouseover", enter);
      document.removeEventListener("mouseout", leave);
      document.removeEventListener("focusin", enter);
      document.removeEventListener("focusout", leave);
      document.removeEventListener("mousedown", close);
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("popstate", close);
    };
  }, []);
  return tip;
}

/**
 * Cards on hover (or keyboard focus) for merge requests, issues, pipelines, jobs,
 * commits, branches and people:
 * title, state, branches or author, instead of a bare tooltip. One listener for
 * the whole app, so links inside server-rendered Markdown get them too.
 */
export function HoverCards() {
  const tip = useTips();
  const [card, setCard] = useState<{ target: Target; rect: DOMRect; anchor: HTMLElement } | null>(null);
  const showT = useRef<number | undefined>(undefined);
  const hideT = useRef<number | undefined>(undefined);
  const over = useRef(false);

  useEffect(() => {
    const enter = (e: Event) => {
      const a = (e.target as Element | null)?.closest?.(SELECTOR) as HTMLElement | null;
      if (!a) return;
      const target = cardOf(a);
      if (!target) return;
      clearTimeout(hideT.current);
      clearTimeout(showT.current);
      // the native tooltip would cover the card
      if (a.title) {
        a.dataset.title = a.title;
        a.removeAttribute("title");
      }
      showT.current = window.setTimeout(() => setCard({ target, rect: a.getBoundingClientRect(), anchor: a }), SHOW_MS);
    };
    const leave = (e: Event) => {
      const a = (e.target as Element | null)?.closest?.(SELECTOR);
      if (!a) return;
      clearTimeout(showT.current);
      hideT.current = window.setTimeout(() => !over.current && setCard(null), HIDE_MS);
    };
    const close = () => {
      clearTimeout(showT.current);
      setCard(null);
    };
    // a scroll moves an open card off its link, so it goes; a card about to show
    // stays pending (scrolling a link under a still pointer, or into view first),
    // and reads the link's position when it shows
    const scrolled = () => setCard(null);
    const key = (e: KeyboardEvent) => e.key === "Escape" && close();
    document.addEventListener("mouseover", enter);
    document.addEventListener("mouseout", leave);
    document.addEventListener("focusin", enter);
    document.addEventListener("focusout", leave);
    document.addEventListener("keydown", key);
    window.addEventListener("scroll", scrolled, true);
    window.addEventListener("popstate", close);
    return () => {
      document.removeEventListener("mouseover", enter);
      document.removeEventListener("mouseout", leave);
      document.removeEventListener("focusin", enter);
      document.removeEventListener("focusout", leave);
      document.removeEventListener("keydown", key);
      window.removeEventListener("scroll", scrolled, true);
      window.removeEventListener("popstate", close);
    };
  }, []);

  // a click navigates: the card goes with it
  useEffect(() => {
    if (!card) return;
    const click = () => setCard(null);
    card.anchor.addEventListener("click", click);
    return () => card.anchor.removeEventListener("click", click);
  }, [card]);

  if (!card) return tip && <Tip text={tip.text} rect={tip.rect} />;
  const width = 340;
  const left = Math.max(8, Math.min(card.rect.left, window.innerWidth - width - 8));
  const below = card.rect.bottom + 8 + 180 < window.innerHeight;
  const style = below ? { left, top: card.rect.bottom + 6, width } : { left, bottom: window.innerHeight - card.rect.top + 6, width };
  return createPortal(
    <div
      className="hover-card"
      role="tooltip"
      style={style}
      onMouseEnter={() => {
        over.current = true;
        clearTimeout(hideT.current);
      }}
      onMouseLeave={() => {
        over.current = false;
        setCard(null);
      }}
    >
      {card.target.kind === "signature" ? (
        <SignatureCard v={card.target.v} />
      ) : card.target.kind === "user" ? (
        <UserCard username={card.target.username} />
      ) : card.target.kind === "commit" ? (
        <CommitCard path={card.target.path} sha={card.target.sha} />
      ) : card.target.kind === "branch" ? (
        <BranchCard path={card.target.path} name={card.target.name} />
      ) : card.target.kind === "pipeline" || card.target.kind === "job" ? (
        <CiCard t={card.target} />
      ) : (
        <ItemCard t={card.target as Item & { kind: "mr" | "issue" }} />
      )}
    </div>,
    document.body,
  );
}

function Loading() {
  return <div className="hover-card-body faint small">Loading…</div>;
}

const STATE: Record<string, { label: string; cls: string }> = {
  opened: { label: "Open", cls: "opened" },
  merged: { label: "Merged", cls: "merged" },
  closed: { label: "Closed", cls: "closed" },
};

/** A pipeline (status, title, ref, stages) or a job (status, stage, pipeline, runner). */
function CiCard({ t }: { t: Item }) {
  const { pid, error: projectError } = useProjectId(t.path);
  const pipeline = useQuery({
    queryKey: ["pipeline", pid, t.iid],
    queryFn: () => api.get<S["PipelineDetail"]>(`/projects/${pid}/pipelines/${t.iid}`),
    enabled: t.kind === "pipeline" && !!pid,
    staleTime: 15_000,
  });
  const job = useQuery({
    queryKey: ["job", pid, t.iid],
    queryFn: () => api.get<S["JobDetail"]>(`/projects/${pid}/jobs/${t.iid}`),
    enabled: t.kind === "job" && !!pid,
    staleTime: 15_000,
  });
  if (projectError || pipeline.error || job.error) return <div className="hover-card-body faint small">Not available.</div>;
  if (t.kind === "pipeline") {
    const p = pipeline.data;
    if (!p) return <Loading />;
    const live = p.jobs.filter((j) => !j.retried);
    return (
      <div className="hover-card-body">
        <div className="hover-card-top">
          <span className="hover-card-status">
            <StatusIcon status={p.status} size={15} /> {statusLabel(p.status)}
          </span>
          <span className="faint mono small">
            {t.path} pipeline #{t.iid}
          </span>
        </div>
        {p.commit_title && (
          <div className="hover-card-title">
            <span>{p.commit_title}</span>
          </div>
        )}
        <div className="hover-card-refs">
          {p.tag ? <Tag size={12} className="faint" /> : <GitBranch size={12} className="faint" />}
          <span className="ref-chip small">{p.ref}</span>
          <code className="small faint">{p.sha.slice(0, 8)}</code>
        </div>
        <div className="hover-card-stages">
          {p.stages.map((s) => {
            const jobs = live.filter((j) => j.stage === s);
            const failed = jobs.some((j) => j.status === "failed" && !j.allow_failure);
            const st = failed ? "failed" : jobs.every((j) => j.status === "success" || j.status === "skipped") ? "success" : (jobs[0]?.status ?? "created");
            return (
              <span key={s} className="small">
                <StatusIcon status={st} size={12} /> {s}
              </span>
            );
          })}
        </div>
        <div className="hover-card-meta faint small">
          {SOURCE[p.source] ?? p.source} by {p.username ?? "someone"} {timeAgo(p.created_at)} · {live.length} job{live.length === 1 ? "" : "s"}
          {p.duration != null && (
            <>
              {" "}
              · <Timer size={11} /> {duration(p.duration)}
            </>
          )}
        </div>
      </div>
    );
  }
  const j = job.data;
  if (!j) return <Loading />;
  return (
    <div className="hover-card-body">
      <div className="hover-card-top">
        <span className="hover-card-status">
          <StatusIcon status={j.status} size={15} allowFailure={j.allow_failure} /> {statusLabel(j.status)}
        </span>
        <span className="faint mono small">
          {t.path} job #{t.iid}
        </span>
      </div>
      <div className="hover-card-title">
        <span>{j.name}</span>
      </div>
      <div className="hover-card-refs">
        <span className="small faint">stage {j.stage} · pipeline #{j.pipeline_iid}</span>
        <span className="ref-chip small">{j.ref}</span>
      </div>
      <div className="hover-card-meta faint small">
        {j.duration != null ? `ran ${duration(j.duration)}` : j.started_at ? `started ${timeAgo(j.started_at)}` : `created ${timeAgo(j.created_at)}`}
        {j.runner_description && ` · on ${j.runner_description}`}
        {j.failure_reason && ` · ${j.failure_reason.replace(/_/g, " ")}`}
      </div>
    </div>
  );
}

function ItemCard({ t }: { t: { kind: "mr" | "issue"; path: string; iid: number } }) {
  const { pid, error: projectError } = useProjectId(t.path);
  const mr = useQuery({ queryKey: pid ? keys.mr(pid, t.iid) : ["none"], queryFn: () => fetchers.mr(pid!, t.iid), enabled: t.kind === "mr" && !!pid, staleTime: 30_000 });
  const issue = useQuery({
    queryKey: ["issue", pid, t.iid],
    queryFn: () => api.get<S["IssueDetail"]>(`/projects/${pid}/issues/${t.iid}`),
    enabled: t.kind === "issue" && !!pid,
    staleTime: 30_000,
  });
  if (projectError || mr.error || issue.error) return <div className="hover-card-body faint small">Not available.</div>;
  if (t.kind === "mr") {
    const m = mr.data;
    if (!m) return <Loading />;
    const st = m.draft && m.state === "opened" ? { label: "Draft", cls: "draft" } : (STATE[m.state] ?? STATE.opened);
    return (
      <div className="hover-card-body">
        <div className="hover-card-top">
          <span className={`mr-state ${st.cls}`}>{st.label}</span>
          <span className="faint mono small">
            {t.path}!{t.iid}
          </span>
        </div>
        <div className="hover-card-title">
          <GitMerge size={15} className={m.state === "merged" ? "ic-accent" : m.state === "closed" ? "ic-danger" : "ic-ok"} />
          <span>{m.title}</span>
        </div>
        <div className="hover-card-refs">
          <span className="ref-chip small">{m.source_branch}</span>
          <ArrowRight size={12} className="faint" />
          <span className="ref-chip small">{m.target_branch}</span>
        </div>
        {m.labels.length > 0 && <LabelList labels={m.labels} />}
        <div className="hover-card-meta faint small">
          {m.author_username ?? m.imported_author ?? "ghost"} opened {timeAgo(m.created_at)}
          {m.versions[0] && ` · ${m.versions[0].files_count} file${m.versions[0].files_count === 1 ? "" : "s"}, +${m.versions[0].additions} −${m.versions[0].deletions}`}
          {m.approvals.length > 0 && ` · ${m.approvals.length} approval${m.approvals.length === 1 ? "" : "s"}`}
        </div>
      </div>
    );
  }
  const i = issue.data;
  if (!i) return <Loading />;
  const st = STATE[i.state] ?? STATE.opened;
  return (
    <div className="hover-card-body">
      <div className="hover-card-top">
        <span className={`mr-state ${st.cls}`}>{st.label}</span>
        <span className="faint mono small">
          {t.path}#{t.iid}
        </span>
      </div>
      <div className="hover-card-title">
        <CircleDot size={15} className={i.state === "closed" ? "faint" : "ic-ok"} />
        <span>{i.title}</span>
      </div>
      {i.labels.length > 0 && <LabelList labels={i.labels} />}
      <div className="hover-card-meta faint small">
        {i.author_username ?? i.imported_author ?? "ghost"} opened {timeAgo(i.created_at)}
        {i.notes_count > 0 && ` · ${i.notes_count} comment${i.notes_count === 1 ? "" : "s"}`}
        {i.assignees.length > 0 && ` · assigned to ${i.assignees.map((a) => a.username).join(", ")}`}
      </div>
    </div>
  );
}

/** A commit's signature: verified or not and why, whose key, and which. */
function SignatureCard({ v }: { v: S["Verification"] }) {
  const ok = v.verification_status === "verified";
  return (
    <div className="hover-card-body">
      <div className="hover-card-top">
        <span className="hover-card-status">
          {ok ? <ShieldCheck size={15} className="ic-ok" /> : <ShieldAlert size={15} className="ic-warn" />} {ok ? "Verified" : "Unverified"}
        </span>
        <span className="faint small">{v.signature_type} signature</span>
      </div>
      <div className="small">{SIGNATURE_TEXT[v.verification_status]}</div>
      {v.signer && (
        <div className="hover-card-refs">
          <Avatar name={v.signer} username={v.signer} />
          <span className="small">Key of @{v.signer}</span>
        </div>
      )}
      {v.key_fingerprint && <div className="hover-card-meta faint small mono hover-card-key">{v.key_fingerprint}</div>}
    </div>
  );
}

function UserCard({ username }: { username: string }) {
  const q = useQuery({ queryKey: ["profile", username], queryFn: () => api.get<S["Profile"]>(`/users/${encodeURIComponent(username)}`), staleTime: 60_000 });
  if (q.error) return <div className="hover-card-body faint small">Not available.</div>;
  const p = q.data;
  if (!p) return <Loading />;
  return (
    <div className="hover-card-body hover-card-user">
      <Avatar name={p.name || p.username} username={p.username} />
      <div style={{ minWidth: 0 }}>
        <div className="hover-card-title">
          <span>{p.name || p.username}</span>
        </div>
        <div className="faint small">@{p.username}</div>
        <div className="hover-card-meta faint small">
          <CalendarDays size={12} /> Member since {new Date(p.member_since).toLocaleDateString(undefined, { month: "long", year: "numeric" })}
        </div>
      </div>
    </div>
  );
}

function useProjectId(path: string) {
  const project = useQuery({ queryKey: keys.pathResolve(path), queryFn: () => fetchers.pathResolve(path), staleTime: 60_000 });
  return { pid: project.data?.kind === "project" ? project.data.id : undefined, error: project.error };
}

/** The latest pipeline of a commit, if it has one. */
function CommitPipeline({ pid, sha }: { pid: number; sha: string }) {
  const q = useQuery({
    queryKey: ["commit-status", pid, sha],
    queryFn: () => api.get<S["CommitStatus"][]>(`/projects/${pid}/commit_statuses?shas=${sha}`),
    enabled: sha.length === 40,
    staleTime: 15_000,
  });
  const st = q.data?.[0];
  if (!st) return null;
  return (
    <span className="small">
      <StatusIcon status={st.status} size={12} /> pipeline #{st.pipeline_iid} {statusLabel(st.status).toLowerCase()}
    </span>
  );
}

/** A commit: subject, the start of its message, author, when, its pipeline. */
function CommitCard({ path, sha }: { path: string; sha: string }) {
  const { pid, error } = useProjectId(path);
  const q = useQuery({
    queryKey: ["commit", pid, sha],
    queryFn: () => api.get<S["CommitInfo"]>(`/projects/${pid}/repository/commits/${sha}`),
    enabled: !!pid,
    staleTime: 300_000,
  });
  if (error || q.error) return <div className="hover-card-body faint small">Not available.</div>;
  const c = q.data;
  if (!c) return <Loading />;
  const body = c.body.trim().split("\n").slice(0, 3).join(" ");
  return (
    <div className="hover-card-body">
      <div className="hover-card-top">
        <span className="faint small">
          <GitCommitHorizontal size={13} /> Commit
        </span>
        <span className="faint mono small">
          {path}@{c.id.slice(0, 8)}
        </span>
      </div>
      <div className="hover-card-title">
        <span>{c.subject}</span>
      </div>
      {body && <div className="small muted hover-card-excerpt">{body.length > 180 ? `${body.slice(0, 180)}…` : body}</div>}
      <div className="hover-card-refs">{pid && <CommitPipeline pid={pid} sha={c.id} />}</div>
      <div className="hover-card-meta faint small">
        {c.author.username ?? c.author.name} authored {timeAgo(c.author.time)}
        {c.parents.length > 1 && " · merge commit"}
      </div>
    </div>
  );
}

/** A branch: its last commit and pipeline, default or protected. */
function BranchCard({ path, name }: { path: string; name: string }) {
  const { pid, error } = useProjectId(path);
  const q = useQuery({
    queryKey: ["branch", pid, name],
    queryFn: () => api.get<S["BranchDto"][]>(`/projects/${pid}/repository/branches?search=${encodeURIComponent(name)}&per_page=50`),
    enabled: !!pid,
    staleTime: 15_000,
  });
  if (error || q.error) return <div className="hover-card-body faint small">Not available.</div>;
  if (!q.data) return <Loading />;
  const b = q.data.find((x) => x.name === name);
  if (!b) {
    return (
      <div className="hover-card-body">
        <div className="hover-card-title">
          <GitBranch size={14} className="faint" /> <span>{name}</span>
        </div>
        <div className="hover-card-meta faint small">No longer exists (deleted or merged).</div>
      </div>
    );
  }
  return (
    <div className="hover-card-body">
      <div className="hover-card-top">
        <span className="faint small">
          <GitBranch size={13} /> Branch{b.default && " · default"}
          {b.protected && (
            <>
              {" "}
              · <Lock size={11} /> protected
            </>
          )}
        </span>
        <span className="faint mono small">{path}</span>
      </div>
      <div className="hover-card-title">
        <span className="mono">{b.name}</span>
      </div>
      {b.commit && (
        <>
          <div className="hover-card-refs">
            <code className="small faint">{b.commit.id.slice(0, 8)}</code>
            <span className="small ellipsis">{b.commit.subject}</span>
          </div>
          <div className="hover-card-refs">{pid && <CommitPipeline pid={pid} sha={b.commit.id} />}</div>
          <div className="hover-card-meta faint small">
            {b.commit.author_name} committed {timeAgo(b.commit.committed_at)}
          </div>
        </>
      )}
    </div>
  );
}

