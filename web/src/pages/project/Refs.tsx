import { useState, type FormEvent } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowLeftRight, ArrowRight, Download, GitBranch, GitMerge, Package, Plus, Tag, Trash2, Shield, Star } from "lucide-react";
import { api, qs, timeAgo, type S, encPath, filesLink } from "../../api/client";
import { StatusIcon, statusLabel } from "./ci/Status";
import { Empty, ErrorBox, Field, ListHead, Q, SearchField, useDebounced } from "../../components/ui";
import { useToast } from "../../shell/toast";
import { PagedDiff } from "./Diff";
import { CommitList } from "./History";
import type { Ctx } from "./Project";
import { useBranches, useTags } from "./Code";
import { useConfirm } from "../../shell/dialog";
import { FormActions, FormDialog, useJustAdded } from "../../components/FormDialog";

/** Subject and age of a ref's tip, for list rows. */
function Tip({ ctx, c }: { ctx: Ctx; c?: S["CommitSummary"] | null }) {
  if (!c) return <span className="ref-updated" />;
  return (
    <>
      <span className="ref-updated ellipsis hide-narrow" title={c.subject}>
        updated {timeAgo(c.committed_at)} by {c.author_name}
      </span>
      <Link to={`${ctx.web}/-/commit/${c.id}`} className="sha hide-narrow" title={c.subject}>
        {c.id.slice(0, 8)}
      </Link>
    </>
  );
}

/** How far a branch is from the default one, in words: "2 ahead · 3 behind", "up to date". */
function distance(b: S["BranchDto"]) {
  if (b.ahead == null || b.behind == null) return "";
  if (b.ahead === 0 && b.behind === 0) return "up to date";
  return [b.ahead > 0 && `${b.ahead} ahead`, b.behind > 0 && `${b.behind} behind`].filter(Boolean).join(" · ");
}

/** Create a branch from a branch, tag or commit. */
function NewBranch({ ctx, onCreated, onCancel }: { ctx: Ctx; onCreated: (name: string) => void; onCancel: () => void }) {
  const [name, setName] = useState("");
  const [from, setFrom] = useState(ctx.p.default_branch ?? "main");
  const [error, setError] = useState<unknown>(null);
  const qc = useQueryClient();
  const toast = useToast();
  return (
    <form
      className="stack"
      onSubmit={async (e) => {
        e.preventDefault();
        setError(null);
        try {
          await api.post(`${ctx.base}/branches`, { branch: name.trim(), ref: from.trim() });
          toast.ok(`Branch ${name.trim()} created`);
          qc.invalidateQueries({ queryKey: ["branches", ctx.p.id] });
          onCreated(name.trim());
        } catch (err) {
          setError(err);
        }
      }}
    >
      <ErrorBox error={error} />
      <Field label="Branch name">
        <input value={name} onChange={(e) => setName(e.target.value)} placeholder="feature/my-change" spellCheck={false} />
      </Field>
      <Field label="From" hint="A branch, tag or commit">
        <input value={from} onChange={(e) => setFrom(e.target.value)} spellCheck={false} />
      </Field>
      <FormActions label="Create branch" disabled={!name.trim() || !from.trim()} onCancel={onCancel} />
    </form>
  );
}

export function Branches({ ctx }: { ctx: Ctx }) {
  const [search, setSearch] = useState("");
  const [creating, setCreating] = useState(false);
  const fresh = useJustAdded();
  const qc = useQueryClient();
  const toast = useToast();
  const confirm = useConfirm();
  const term = useDebounced(search, 250);
  const q = useQuery({
    queryKey: ["branches", ctx.p.id, term, "details"],
    queryFn: () => api.get<S["BranchDto"][]>(`${ctx.base}/branches${qs({ search: term, per_page: 100, details: true })}`),
  });
  const remove = async (name: string) => {
    const ok = await confirm({
      title: `Delete the branch ${name}?`,
      body: "Its commits stay reachable only from other branches and merge requests.",
      action: "Delete branch",
      danger: true,
    });
    if (!ok) return;
    try {
      await api.del(`${ctx.base}/branches/${encPath(name)}`);
      toast.ok(`Branch ${name} deleted`);
      qc.invalidateQueries({ queryKey: ["branches", ctx.p.id] });
    } catch (e) {
      toast.error(e);
    }
  };
  return (
    <div className="stack list-page">
      <ListHead title="Branches">
        {ctx.p.can_write_repository && (
          <button className="btn primary" onClick={() => setCreating(true)}>
            <Plus size={14} /> Create a branch
          </button>
        )}
      </ListHead>
      <FormDialog open={creating} onClose={() => setCreating(false)} title="Create a branch">
        {creating && (
          <NewBranch
            ctx={ctx}
            onCancel={() => setCreating(false)}
            onCreated={(name) => {
              fresh.mark(name);
              setCreating(false);
            }}
          />
        )}
      </FormDialog>
      <div className="list-toolbar plain">
        <SearchField value={search} onChange={setSearch} placeholder="Filter branches" />
      </div>
      <Q q={q}>
        {(list) =>
          list.length === 0 ? (
            <Empty>{search ? <>No branch matches “{search}”.</> : "No branches yet."}</Empty>
          ) : (
            <div className="list rows">
              {list.map((b) => (
                <div className={"item ref-row" + fresh.fresh(b.name)} key={b.name}>
                  <span className="row-icon faint">
                    <GitBranch size={16} />
                  </span>
                  <div className="ref-main">
                    <Link to={filesLink(ctx.web, b.name, ctx.p.default_branch)} className="ref-name ellipsis">
                      {b.name}
                    </Link>
                    {b.default && (
                      <span className="pill">
                        <Star size={11} aria-hidden /> default
                      </span>
                    )}
                    {b.protected && (
                      <span className="pill">
                        <Shield size={11} aria-hidden /> protected
                      </span>
                    )}
                  </div>
                  <span className="ref-cells">
                  <Tip ctx={ctx} c={b.commit} />
                  <span className="ref-distance" title={b.default ? undefined : `Compared with ${ctx.p.default_branch}`}>
                    {distance(b)}
                  </span>
                  <span className="ref-ci">
                    {b.pipeline && (
                      <Link to={`${ctx.web}/-/pipelines/${b.pipeline.iid}`} title={`Pipeline #${b.pipeline.iid}: ${statusLabel(b.pipeline.status)}`} aria-label={`Pipeline #${b.pipeline.iid}: ${statusLabel(b.pipeline.status)}`}>
                        <StatusIcon status={b.pipeline.status} />
                      </Link>
                    )}
                  </span>
                  <span className="ref-mr">
                    {b.merge_request ? (
                      <Link className="ref-mr-link" to={`${ctx.web}/-/merge_requests/${b.merge_request.iid}`} title={b.merge_request.title}>
                        <GitMerge size={13} aria-hidden /> !{b.merge_request.iid}
                      </Link>
                    ) : (
                      !b.default &&
                      ctx.p.can_push && (
                        <Link className="btn small" to={`${ctx.web}/-/merge_requests/new?source=${encodeURIComponent(b.name)}`}>
                          <GitMerge size={13} /> Merge request
                        </Link>
                      )
                    )}
                  </span>
                  <span className="ref-slot hide-narrow">
                    {!b.default && ctx.p.default_branch && (
                      <Link className="icon-btn" to={`${ctx.web}/-/compare/${ctx.p.default_branch}...${b.name}`} title={`Compare with ${ctx.p.default_branch}`} aria-label={`Compare ${b.name} with ${ctx.p.default_branch}`}>
                        <ArrowLeftRight size={15} />
                      </Link>
                    )}
                  </span>
                  <span className="ref-slot">
                    {!b.default && ctx.p.can_write_repository && (
                      <button className="icon-btn" title={`Delete ${b.name}`} aria-label={`Delete branch ${b.name}`} onClick={() => remove(b.name)}>
                        <Trash2 size={15} />
                      </button>
                    )}
                  </span>
                  </span>
                </div>
              ))}
            </div>
          )
        }
      </Q>
    </div>
  );
}

/** Create a tag, annotated when it has a message. */
function NewTag({ ctx, onCreated, onCancel }: { ctx: Ctx; onCreated: (name: string) => void; onCancel: () => void }) {
  const [name, setName] = useState("");
  const [from, setFrom] = useState(ctx.p.default_branch ?? "main");
  const [message, setMessage] = useState("");
  const [error, setError] = useState<unknown>(null);
  const qc = useQueryClient();
  const toast = useToast();
  return (
    <form
      className="stack"
      onSubmit={async (e) => {
        e.preventDefault();
        setError(null);
        try {
          await api.post(`${ctx.base}/tags`, { tag_name: name.trim(), ref: from.trim(), message: message.trim() || undefined });
          toast.ok(`Tag ${name.trim()} created`);
          qc.invalidateQueries({ queryKey: ["tags", ctx.p.id] });
          onCreated(name.trim());
        } catch (err) {
          setError(err);
        }
      }}
    >
      <ErrorBox error={error} />
      <Field label="Tag name">
        <input value={name} onChange={(e) => setName(e.target.value)} placeholder="v1.0.0" spellCheck={false} />
      </Field>
      <Field label="From" hint="A branch, tag or commit">
        <input value={from} onChange={(e) => setFrom(e.target.value)} spellCheck={false} />
      </Field>
      <Field label="Message" hint="Optional: makes an annotated tag">
        <textarea rows={2} value={message} onChange={(e) => setMessage(e.target.value)} />
      </Field>
      <FormActions label="Create tag" disabled={!name.trim() || !from.trim()} onCancel={onCancel} />
    </form>
  );
}

export function Tags({ ctx }: { ctx: Ctx }) {
  const [search, setSearch] = useState("");
  const [creating, setCreating] = useState(false);
  const fresh = useJustAdded();
  const qc = useQueryClient();
  const toast = useToast();
  const confirm = useConfirm();
  const q = useTags(ctx);
  const remove = async (t: S["TagDto"]) => {
    const ok = await confirm({
      title: `Delete the tag ${t.name}?`,
      body: t.release ? `Its release “${t.release}” is deleted too.` : undefined,
      action: "Delete tag",
      danger: true,
    });
    if (!ok) return;
    try {
      await api.del(`${ctx.base}/tags/${encPath(t.name)}`);
      toast.ok(`Tag ${t.name} deleted`);
      qc.invalidateQueries({ queryKey: ["tags", ctx.p.id] });
      qc.invalidateQueries({ queryKey: ["releases", ctx.p.id] });
    } catch (e) {
      toast.error(e);
    }
  };
  return (
    <div className="stack list-page">
      <ListHead title="Tags">
        {ctx.p.can_write_repository && (
          <button className="btn primary" onClick={() => setCreating(true)}>
            <Plus size={14} /> Create a tag
          </button>
        )}
      </ListHead>
      <FormDialog open={creating} onClose={() => setCreating(false)} title="Create a tag">
        {creating && (
          <NewTag
            ctx={ctx}
            onCancel={() => setCreating(false)}
            onCreated={(name) => {
              fresh.mark(name);
              setCreating(false);
            }}
          />
        )}
      </FormDialog>
      <div className="list-toolbar plain">
        <SearchField value={search} onChange={setSearch} placeholder="Filter tags" />
      </div>
      <Q q={q}>
        {(all) => {
          const list = all.filter((t) => t.name.toLowerCase().includes(search.toLowerCase()));
          return list.length === 0 ? (
            <Empty>{search ? <>No tag matches “{search}”.</> : "No tags yet."}</Empty>
          ) : (
            <div className="list rows">
              {list.map((t) => (
                <div className={"item ref-row" + fresh.fresh(t.name)} key={t.name}>
                  <span className="row-icon faint">
                    <Tag size={16} />
                  </span>
                  <div className="ref-main">
                    <Link to={filesLink(ctx.web, t.name, ctx.p.default_branch)} className="ref-name ellipsis">
                      {t.name}
                    </Link>
                    {t.annotated && <span className="pill">annotated</span>}
                    {t.protected && (
                      <span className="pill">
                        <Shield size={11} aria-hidden /> protected
                      </span>
                    )}
                    {t.release && (
                      <Link className="pill" to={`${ctx.web}/-/releases/${encodeURIComponent(t.name)}`} title="This tag's release">
                        <Package size={11} /> {t.release}
                      </Link>
                    )}
                  </div>
                  <span className="ref-cells">
                  <Tip ctx={ctx} c={t.commit} />
                  <span className="ref-release">
                    {!t.release && ctx.p.can_push && (
                      <Link className="btn small" to={`${ctx.web}/-/releases/new?tag=${encodeURIComponent(t.name)}`} aria-label="Create release" title="Create a release on this tag">
                        <Package size={13} /> <span className="hide-narrow">Create release</span>
                      </Link>
                    )}
                  </span>
                  <div className="btn-group">
                    <a className="btn small" href={`/api/v1${ctx.base}/archive${qs({ ref: t.name, format: "zip" })}`} title="Download a zip of this tag">
                      <Download size={13} /> zip
                    </a>
                    <a className="btn small" href={`/api/v1${ctx.base}/archive${qs({ ref: t.name, format: "tar.gz" })}`} title="Download a tar.gz of this tag">
                      tar.gz
                    </a>
                  </div>
                  <span className="ref-slot">
                    {ctx.p.can_write_repository && (t.protected ? ctx.p.can_admin : ctx.p.can_push) && (
                      <button className="icon-btn" title={`Delete ${t.name}`} aria-label={`Delete tag ${t.name}`} onClick={() => remove(t)}>
                        <Trash2 size={15} />
                      </button>
                    )}
                  </span>
                  </span>
                </div>
              ))}
            </div>
          );
        }}
      </Q>
    </div>
  );
}

/** Branch and tag names, offered while typing a ref. */
function RefOptions({ ctx, id }: { ctx: Ctx; id: string }) {
  const branches = useBranches(ctx);
  const tags = useTags(ctx);
  return (
    <datalist id={id}>
      {(branches.data ?? []).map((b) => (
        <option key={`b${b.name}`} value={b.name} />
      ))}
      {(tags.data ?? []).map((t) => (
        <option key={`t${t.name}`} value={t.name} />
      ))}
    </datalist>
  );
}

/** `/-/compare/<from>...<to>` */
export function Compare({ ctx, spec }: { ctx: Ctx; spec: string }) {
  const nav = useNavigate();
  const [params] = useSearchParams();
  const [from0, to0] = spec.includes("...") ? spec.split("...") : [params.get("from") ?? ctx.p.default_branch ?? "", params.get("to") ?? ""];
  const [from, setFrom] = useState(from0);
  const [to, setTo] = useState(to0);
  const ready = !!from0 && !!to0;
  const q = useQuery({
    queryKey: ["compare", ctx.p.id, from0, to0],
    queryFn: () => api.get<S["CompareResponse"]>(`${ctx.base}/compare${qs({ from: from0, to: to0, limit: 1 })}`),
    enabled: ready,
  });
  const branches = useBranches(ctx);
  const toIsBranch = (branches.data ?? []).some((b) => b.name === to0);
  const submit = (e: FormEvent) => {
    e.preventDefault();
    nav(`${ctx.web}/-/compare/${from}...${to}`);
  };
  return (
    <div className="stack list-page">
      <ListHead title="Compare" sub="The changes in one branch, tag or commit since it left another." />
      <form className="compare-form" onSubmit={submit}>
        <RefOptions ctx={ctx} id="compare-refs" />
        <label className="compare-ref">
          <span>base</span>
          <input list="compare-refs" placeholder="main" value={from} onChange={(e) => setFrom(e.target.value)} />
        </label>
        <ArrowRight size={16} className="faint" aria-hidden />
        <label className="compare-ref">
          <span>compare</span>
          <input list="compare-refs" placeholder="branch, tag or commit" value={to} onChange={(e) => setTo(e.target.value)} />
        </label>
        <button className="btn primary" disabled={!from || !to}>
          Compare
        </button>
        <button type="button" className="icon-btn" title="Swap" aria-label="Swap base and compare" onClick={() => (setFrom(to), setTo(from))}>
          <ArrowLeftRight size={16} />
        </button>
      </form>
      {ready && (
        <Q q={q}>
          {(c) => (
            <div className="stack">
              <div className="compare-summary">
                <span>
                  <strong>{c.commits.length}</strong> commit{c.commits.length === 1 ? "" : "s"} in <code>{to0}</code> not in <code>{from0}</code>
                  {c.merge_base ? ", diffed from their merge base." : "."}
                </span>
                <span className="grow" />
                {toIsBranch && to0 !== from0 && ctx.p.can_push && (
                  <Link className="btn" to={`${ctx.web}/-/merge_requests/new?source=${encodeURIComponent(to0)}&target=${encodeURIComponent(from0)}`}>
                    <GitMerge size={15} /> Open a merge request
                  </Link>
                )}
              </div>
              {c.commits.length > 0 && <CommitList ctx={ctx} list={c.commits} />}
              <PagedDiff url={(offset) => `${ctx.base}/compare${qs({ from: from0, to: to0, offset })}`} pick={(v) => (v as S["CompareResponse"]).diff} />
            </div>
          )}
        </Q>
      )}
    </div>
  );
}
