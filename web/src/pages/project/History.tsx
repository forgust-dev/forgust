import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { ChevronDown, ChevronUp, FolderCode, GitCommitHorizontal } from "lucide-react";
import { api, qs, timeAgo, type S } from "../../api/client";
import { Avatar, Empty, GitPerson, ListHead, Q, ShaCopy, Pager } from "../../components/ui";
import { PagedDiff } from "./Diff";
import { PathCrumbs, RefPicker, useResolved } from "./Code";
import type { Ctx } from "./Project";
import { useCiLive } from "../../shell/live";
import { StatusIcon } from "./ci/Status";
import { SignatureBadge, useSignatures } from "./Signatures";

/** Latest pipeline status per commit, for lists. */
export function useCommitStatuses(ctx: Ctx, shas: string[]) {
  useCiLive(ctx.p.id);
  const q = useQuery({
    queryKey: ["commit-statuses", ctx.p.id, shas.join(",")],
    queryFn: () => api.get<S["CommitStatus"][]>(`/projects/${ctx.p.id}/commit_statuses${qs({ shas: shas.join(",") })}`),
    enabled: shas.length > 0 && ctx.p.can_read_code,
    staleTime: 10_000,
  });
  return new Map((q.data ?? []).map((s) => [s.sha, s]));
}

export function CommitRow({ ctx, c, status, signature }: { ctx: Ctx; c: S["CommitInfo"]; status?: S["CommitStatus"]; signature?: S["Verification"] }) {
  const to = `${ctx.web}/-/commit/${c.id}`;
  const coAuthored = c.committer.name !== c.author.name;
  return (
    <div className="item commit-row">
      <Avatar name={c.author.name} username={c.author.username} />
      <div className="grow" style={{ minWidth: 0 }}>
        <Link to={to} className="row-title">
          <span className="ellipsis">{c.subject}</span>
        </Link>
        <div className="row-meta">
          <span>
            <strong>
              <GitPerson p={c.author} />
            </strong>{" "}
            {coAuthored ? (
              <>
                authored, <GitPerson p={c.committer} /> committed
              </>
            ) : (
              "committed"
            )}{" "}
            {timeAgo(c.committer.time)}
          </span>
        </div>
      </div>
      <SignatureBadge v={signature} />
      {status && (
        <Link to={`${ctx.web}/-/pipelines/${status.pipeline_iid}`} aria-label={`Pipeline ${status.status}`} title={`Pipeline ${status.status}`}>
          <StatusIcon status={status.status} size={18} />
        </Link>
      )}
      <ShaCopy sha={c.id} to={to} />
      <Link to={`${ctx.web}/-/tree/${c.id}`} className="icon-btn" title="Browse the files at this commit" aria-label="Browse the files at this commit">
        <FolderCode size={16} />
      </Link>
    </div>
  );
}

const day = (t: number) => new Date(t * 1000).toLocaleDateString(undefined, { weekday: "short", year: "numeric", month: "short", day: "numeric" });

/** Commits grouped by the day they were committed. */
export function CommitList({ ctx, list }: { ctx: Ctx; list: S["CommitInfo"][] }) {
  const statuses = useCommitStatuses(ctx, list.map((c) => c.id));
  const signatures = useSignatures(ctx, list.map((c) => c.id));
  const groups = useMemo(() => {
    const out: { day: string; commits: S["CommitInfo"][] }[] = [];
    for (const c of list) {
      const d = day(c.committer.time);
      if (out.length && out[out.length - 1].day === d) out[out.length - 1].commits.push(c);
      else out.push({ day: d, commits: [c] });
    }
    return out;
  }, [list]);
  return (
    <div className="commit-days">
      {groups.map((g) => (
        <section key={g.day} className="commit-day">
          <h2 className="commit-day-head">
            <GitCommitHorizontal size={15} /> Commits on {g.day}
          </h2>
          <div className="list rows">
            {g.commits.map((c) => (
              <CommitRow key={c.id} ctx={ctx} c={c} status={statuses.get(c.id)} signature={signatures[c.id]} />
            ))}
          </div>
        </section>
      ))}
    </div>
  );
}

const PER_PAGE = 40;

export function Commits({ ctx, refAndPath }: { ctx: Ctx; refAndPath: string }) {
  const r = useResolved(ctx, refAndPath);
  const [page, setPage] = useState(1);
  const q = useQuery({
    queryKey: ["commits", ctx.p.id, r.data?.commit, r.data?.path, page],
    queryFn: () => api.get<S["CommitInfo"][]>(`${ctx.base}/commits${qs({ ref: r.data!.commit, path: r.data!.path, page, per_page: PER_PAGE })}`),
    enabled: !!r.data,
  });
  return (
    <Q q={r}>
      {(res) => (
        <div className="stack list-page">
          <ListHead title="Commits" />
          <div className="repo-toolbar">
            <RefPicker ctx={ctx} current={res.ref} path={res.path} kind="commits" />
            {res.path && <PathCrumbs ctx={ctx} gitRef={res.ref} path={res.path} />}
          </div>
          <Q q={q}>{(list) => (list.length === 0 ? <Empty>No commits.</Empty> : <CommitList ctx={ctx} list={list} />)}</Q>
          <Pager page={page} count={q.data?.length ?? 0} perPage={PER_PAGE} onPage={setPage} labels={["Newer", "Older"]} />
        </div>
      )}
    </Q>
  );
}

/** A commit message body: the first lines, the rest on demand. */
function CommitBody({ body }: { body: string }) {
  const lines = body.replace(/\s+$/, "").split("\n");
  const long = lines.length > 8;
  const [open, setOpen] = useState(false);
  return (
    <div className="commit-body">
      <pre>{long && !open ? lines.slice(0, 6).join("\n") : lines.join("\n")}</pre>
      {long && (
        <button className="link-btn small" onClick={() => setOpen(!open)}>
          {open ? (
            <>
              <ChevronUp size={14} /> Show less
            </>
          ) : (
            <>
              <ChevronDown size={14} /> Show {lines.length - 6} more lines
            </>
          )}
        </button>
      )}
    </div>
  );
}

export function Commit({ ctx, sha }: { ctx: Ctx; sha: string }) {
  const c = useQuery({ queryKey: ["commit", ctx.p.id, sha], queryFn: () => api.get<S["CommitInfo"]>(`${ctx.base}/commits/${sha}`) });
  const statuses = useCommitStatuses(ctx, c.data ? [c.data.id] : []);
  const signatures = useSignatures(ctx, c.data ? [c.data.id] : []);
  return (
    <Q q={c}>
      {(commit) => {
        const status = statuses.get(commit.id);
        const other = commit.committer.name !== commit.author.name;
        return (
          <div className="stack">
            <header className="mr-head commit-head">
              <div className="mr-status">
                <span className="mr-state commit">Commit</span>
                <span>{commit.id.slice(0, 8)}</span>
                <span aria-hidden="true">·</span>
                <span>
                  <GitPerson p={commit.author} /> authored {timeAgo(commit.author.time)}
                </span>
                {other && (
                  <>
                    <span aria-hidden="true">·</span>
                    <span>
                      <GitPerson p={commit.committer} /> committed {timeAgo(commit.committer.time)}
                    </span>
                  </>
                )}
              </div>
              <h1 className="mr-title">{commit.subject}</h1>
              {commit.body && <CommitBody body={commit.body} />}
              <div className="commit-facts">
                <Avatar name={commit.author.name} username={commit.author.username} />
                <ShaCopy sha={commit.id} />
                <SignatureBadge v={signatures[commit.id]} />
                {commit.parents.length > 0 && (
                  <span className="commit-parents">
                    {commit.parents.length === 1 ? "parent" : `${commit.parents.length} parents`}
                    {commit.parents.map((p) => (
                      <Link key={p} to={`${ctx.web}/-/commit/${p}`} className="sha">
                        {p.slice(0, 8)}
                      </Link>
                    ))}
                  </span>
                )}
                {status && (
                  <Link to={`${ctx.web}/-/pipelines/${status.pipeline_iid}`} className="commit-pipeline">
                    <StatusIcon status={status.status} size={16} /> Pipeline #{status.pipeline_iid}
                  </Link>
                )}
                <span className="grow" />
                <Link to={`${ctx.web}/-/tree/${commit.id}`} className="btn">
                  <FolderCode size={15} /> Browse files
                </Link>
              </div>
            </header>
            <PagedDiff url={(offset) => `${ctx.base}/commits/${commit.id}/diff${qs({ offset })}`} />
          </div>
        );
      }}
    </Q>
  );
}
