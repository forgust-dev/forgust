import { useMemo, useRef, useState, type FormEvent } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { BookOpen, ExternalLink, FileArchive, GitCommitHorizontal, Image, Package, Paperclip, Pencil, Plus, Tag, Trash2, Wrench, Clock } from "lucide-react";
import { api, bytes, enc, qs, refLink, timeAgo, type S } from "../../api/client";
import { AuthorLink, Empty, ErrorBox, Field, ListHead, Q, StateBadge } from "../../components/ui";
import { DecideCard, type DecideRow } from "../../components/decide";
import { MarkdownEditor } from "../../components/MarkdownEditor";
import { useToast } from "../../shell/toast";
import { useConfirm } from "../../shell/dialog";
import type { Ctx } from "./Project";
import { useTags } from "./Code";
import { Markdown } from "../../components/Markdown";

const LINK_TYPES = ["other", "package", "image", "runbook"] as const;

function LinkIcon({ type }: { type: string }) {
  const Icon = type === "package" ? Package : type === "image" ? Image : type === "runbook" ? BookOpen : ExternalLink;
  return <Icon size={14} className="faint" />;
}

/** Notes, assets and sources of one release. */
function ReleaseCard({ ctx, r, single }: { ctx: Ctx; r: S["ReleaseDto"]; single?: boolean }) {
  const nav = useNavigate();
  const qc = useQueryClient();
  const toast = useToast();
  const confirm = useConfirm();
  const page = `${ctx.web}/-/releases/${enc(r.tag_name)}`;
  const remove = async () => {
    if (!(await confirm({ title: `Delete the release ${r.name}?`, body: `The tag ${r.tag_name} stays.`, action: "Delete release", danger: true }))) return;
    try {
      await api.del(`/projects/${ctx.p.id}/releases/${enc(r.tag_name)}`);
      toast.ok(`Release ${r.name} deleted`);
      qc.invalidateQueries({ queryKey: ["releases", ctx.p.id] });
      qc.invalidateQueries({ queryKey: ["tags", ctx.p.id] });
      if (single) nav(`${ctx.web}/-/releases`);
    } catch (e) {
      toast.error(e);
    }
  };
  const date = new Date(r.released_at);
  return (
    <article className="card release" aria-label={`Release ${r.name}`}>
      <header className="release-head">
        <div className="grow" style={{ minWidth: 0 }}>
          <h2 className="release-title">{single ? r.name : <Link to={page}>{r.name}</Link>}</h2>
          <div className="row wrap small muted" style={{ gap: 10 }}>
            <Link className="ref-chip small" to={refLink(ctx.web, "tree", r.tag_name)} title="The tag">
              <Tag size={11} /> {r.tag_name}
            </Link>
            {r.commit && (
              <Link className="sha" to={`${ctx.web}/-/commit/${r.commit.id}`} title={r.commit.title}>
                {r.commit.short_id}
              </Link>
            )}
            {r.upcoming_release ? (
              <StateBadge tone="warn" icon={Clock}>
                Upcoming
              </StateBadge>
            ) : null}
            <span title={date.toLocaleString()}>
              {r.upcoming_release ? "Releases" : "Released"} {timeAgo(r.released_at)}
              {r.author_username && (
                <>
                  {" "}
                  by <AuthorLink x={{ author_username: r.author_username, author_name: r.author_name }} name />
                </>
              )}
            </span>
          </div>
        </div>
        {ctx.p.can_push && (
          <div className="row" style={{ gap: 4 }}>
            <Link className="icon-btn" to={`${page}/edit`} title="Edit this release" aria-label={`Edit release ${r.name}`}>
              <Pencil size={15} />
            </Link>
            <button className="icon-btn" title="Delete this release" aria-label={`Delete release ${r.name}`} onClick={remove}>
              <Trash2 size={15} />
            </button>
          </div>
        )}
      </header>
      {r.description ? <Markdown className="markdown" html={r.description_html} /> : null}
      <section className="release-assets" aria-label="Assets">
        <h3 className="small muted">Assets · {r.links.length + r.sources.length}</h3>
        <ul>
          {r.links.map((l) => (
            <li key={l.id}>
              <LinkIcon type={l.link_type} />
              <a href={l.direct_asset_url ?? l.href} rel="noopener noreferrer">
                {l.name}
              </a>
              {l.link_type !== "other" && <span className="faint small">{l.link_type}</span>}
            </li>
          ))}
          {r.sources.map((s) => (
            <li key={s.format}>
              <FileArchive size={14} className="faint" />
              <a href={s.url}>Source code ({s.format})</a>
            </li>
          ))}
        </ul>
      </section>
    </article>
  );
}

/**
 * The page's emphasized card, for those who release: what is not released yet.
 * Tags newer than the latest release that have none, and what the default branch
 * gained since it. None when everything is released, or when nothing ever was.
 */
function Unreleased({ ctx, latest }: { ctx: Ctx; latest?: S["ReleaseDto"] }) {
  const tags = useTags(ctx, ctx.p.can_push);
  const ref = ctx.p.default_branch ?? "main";
  const since = useQuery({
    queryKey: ["compare", ctx.p.id, latest?.tag_name, ref, "commits"],
    // the commits only: the diff stops at one file
    queryFn: () => api.get<S["CompareResponse"]>(`${ctx.base}/compare${qs({ from: latest!.tag_name, to: ref, limit: 1 })}`),
    enabled: ctx.p.can_push && !!latest,
    staleTime: 60_000,
  });
  if (!ctx.p.can_push || !tags.data || (latest && !since.data)) return null;
  const released = latest ? (tags.data.find((t) => t.name === latest.tag_name)?.commit?.committed_at ?? 0) : 0;
  const fresh = tags.data.filter((t) => !t.release && (!latest || (t.commit?.committed_at ?? 0) > released));
  const commits = since.data?.commits ?? [];
  if (fresh.length === 0 && commits.length === 0) return null;
  // the commit list stops at 250
  const count = commits.length >= 250 ? "250+" : String(commits.length);
  const tagRows = fresh.slice(0, 2);
  const commitRows = commits.slice(0, 4 - tagRows.length);
  const hidden = commits.length - commitRows.length;
  const rows: DecideRow[] = [
    ...tagRows.map((t) => ({
      key: `tag:${t.name}`,
      icon: <Tag size={15} className="row-ic" />,
      text: (
        <>
          <code>{t.name}</code> {t.commit?.subject}
        </>
      ),
      aside: <Link to={`${ctx.web}/-/releases/new?tag=${enc(t.name)}`}>Release it</Link>,
    })),
    ...commitRows.map((c) => ({
      key: c.id,
      icon: <GitCommitHorizontal size={15} className="row-ic" />,
      text: <Link to={`${ctx.web}/-/commit/${c.id}`}>{c.subject}</Link>,
      aside: c.id.slice(0, 8),
    })),
  ];
  const first = fresh[0];
  return (
    <DecideCard
      label="Not released yet"
      eyebrow={
        <>
          <Package size={14} aria-hidden /> Not released yet
        </>
      }
      // the title says what the button does: release the tag, else what the branch gained
      title={
        first
          ? fresh.length === 1
            ? `${first.name} has no release`
            : `${fresh.length} tags have no release`
          : `${count} commit${commits.length === 1 ? "" : "s"} on ${ref} since ${latest!.tag_name}`
      }
      sub={
        latest
          ? first && commits.length > 0
            ? `${count} commit${commits.length === 1 ? "" : "s"} on ${ref} since ${latest.tag_name}, released ${timeAgo(latest.released_at)}.`
            : `${latest.name} was released ${timeAgo(latest.released_at)}.`
          : "Give a tag its notes and files."
      }
      actions={
        <>
          <Link className="btn run" to={`${ctx.web}/-/releases/new${first ? `?tag=${enc(first.name)}` : ""}`}>
            {first ? `Release ${first.name}` : "Create release"}
          </Link>
          {latest && commits.length > 0 && (
            <Link to={`${ctx.web}/-/compare/${latest.tag_name}...${ref}`}>
              Compare {latest.tag_name}…{ref}
            </Link>
          )}
        </>
      }
      rowsTitle={tagRows.length === 0 && commitRows.length > 0 ? `Latest on ${ref}` : undefined}
      rows={rows}
      foot={hidden > 0 ? `And ${hidden} more commit${hidden === 1 ? "" : "s"}.` : undefined}
    />
  );
}

/** An older release, in one line: the latest one gets the full card. */
function ReleaseRow({ ctx, r }: { ctx: Ctx; r: S["ReleaseDto"] }) {
  const assets = r.links.length + r.sources.length;
  return (
    <div className="item">
      <span className="row-icon faint">
        <Package size={16} />
      </span>
      <div className="grow" style={{ minWidth: 0 }}>
        <div className="row-title">
          <Link to={`${ctx.web}/-/releases/${enc(r.tag_name)}`} className="ellipsis">
            {r.name}
          </Link>
          {r.upcoming_release && (
            <StateBadge tone="warn" icon={Clock}>
              Upcoming
            </StateBadge>
          )}
        </div>
        <div className="row-meta">
          <span>{r.tag_name}</span>
          {r.commit && <span>{r.commit.short_id}</span>}
          {r.author_username && <span>by {r.author_username}</span>}
          <span>
            {assets} asset{assets === 1 ? "" : "s"}
          </span>
        </div>
      </div>
      <span className="row-time" title={new Date(r.released_at).toLocaleString()}>
        {timeAgo(r.released_at)}
      </span>
    </div>
  );
}

export function Releases({ ctx }: { ctx: Ctx }) {
  const q = useQuery({ queryKey: ["releases", ctx.p.id], queryFn: () => api.get<S["ReleaseDto"][]>(`/projects/${ctx.p.id}/releases?per_page=50`) });
  return (
    <div className="stack list-page">
      <ListHead title="Releases" sub="Versions of this project: notes, files and the source at each tag.">
        {ctx.p.can_push && (
          <Link className="btn primary" to={`${ctx.web}/-/releases/new`}>
            <Plus size={14} /> New release
          </Link>
        )}
      </ListHead>
      <Q q={q}>
        {(list) => {
          // newest first; an upcoming release is not the latest one out
          const latest = list.find((r) => !r.upcoming_release) ?? list[0];
          const rest = list.filter((r) => r !== latest);
          return (
            <>
              <Unreleased ctx={ctx} latest={latest} />
              {list.length === 0 ? (
                <Empty>No releases yet.{ctx.p.can_push && " Create one from a tag, or let a pipeline make it."}</Empty>
              ) : (
                <>
                  <ReleaseCard ctx={ctx} r={latest} />
                  {rest.length > 0 && (
                    <section className="stack" style={{ gap: 8, marginTop: 10 }} aria-label="Other releases">
                      <h2 className="section-title">
                        Other releases<span className="section-count"> {rest.length}</span>
                      </h2>
                      <div className="list rows">
                        {rest.map((r) => (
                          <ReleaseRow key={r.id} ctx={ctx} r={r} />
                        ))}
                      </div>
                    </section>
                  )}
                </>
              )}
            </>
          );
        }}
      </Q>
    </div>
  );
}

export function ReleasePage({ ctx, tag }: { ctx: Ctx; tag: string }) {
  const q = useQuery({ queryKey: ["releases", ctx.p.id, tag], queryFn: () => api.get<S["ReleaseDto"]>(`/projects/${ctx.p.id}/releases/${enc(tag)}`) });
  return (
    <div className="stack list-page">
      <Q q={q}>{(r) => <ReleaseCard ctx={ctx} r={r} single />}</Q>
    </div>
  );
}

type PendingLink = { key: string; id?: number; name: string; url: string; direct_asset_path?: string | null; link_type: string; size?: number };

/** New release, or `tag`'s release edited. */
export function ReleaseForm({ ctx, tag }: { ctx: Ctx; tag?: string }) {
  const existing = useQuery({
    queryKey: ["releases", ctx.p.id, tag],
    queryFn: () => api.get<S["ReleaseDto"]>(`/projects/${ctx.p.id}/releases/${enc(tag!)}`),
    enabled: !!tag,
  });
  if (tag) return <Q q={existing}>{(r) => <Form ctx={ctx} r={r} />}</Q>;
  return <Form ctx={ctx} />;
}

function toLocal(iso: string) {
  const d = new Date(iso);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function Form({ ctx, r }: { ctx: Ctx; r?: S["ReleaseDto"] }) {
  const nav = useNavigate();
  const qc = useQueryClient();
  const toast = useToast();
  const [params] = useSearchParams();
  // editing a release keeps its tag: no picker
  const tags = useTags(ctx, !r);
  const [tag, setTag] = useState(r?.tag_name ?? params.get("tag") ?? "");
  const [from, setFrom] = useState(ctx.p.default_branch ?? "main");
  const [tagMessage, setTagMessage] = useState("");
  const [name, setName] = useState(r?.name ?? "");
  const [notes, setNotes] = useState(r?.description ?? "");
  const [date, setDate] = useState(r ? toLocal(r.released_at) : "");
  const [links, setLinks] = useState<PendingLink[]>(() => (r?.links ?? []).map((l) => ({ key: `l${l.id}`, id: l.id, name: l.name, url: l.url, direct_asset_path: l.direct_asset_path, link_type: l.link_type })));
  const [adding, setAdding] = useState<{ name: string; url: string; link_type: string } | null>(null);
  const [uploading, setUploading] = useState(0);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);

  const known = tags.data ?? [];
  const tagExists = !!r || known.some((t) => t.name === tag.trim());
  const taken = r ? undefined : known.find((t) => t.name === tag.trim())?.release;

  const files = async (list: File[]) => {
    for (const f of list) {
      setUploading((n) => n + 1);
      try {
        const u = await api.upload(ctx.p.id, f);
        setLinks((ls) => [...ls, { key: `u${u.id}`, name: u.alt, url: u.full_path, direct_asset_path: `/${u.alt}`, link_type: "other", size: u.size }]);
      } catch (e) {
        setError(e);
      } finally {
        setUploading((n) => n - 1);
      }
    }
  };

  const linkBody = (l: PendingLink) => ({ name: l.name, url: l.url, direct_asset_path: l.direct_asset_path || undefined, link_type: l.link_type });
  const released_at = useMemo(() => (date ? new Date(date).toISOString() : undefined), [date]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    setBusy(true);
    const t = tag.trim();
    const base = `/projects/${ctx.p.id}/releases`;
    try {
      if (!r) {
        await api.post(base, {
          tag_name: t,
          name: name.trim() || undefined,
          description: notes,
          ref: tagExists ? undefined : from.trim(),
          tag_message: tagExists ? undefined : tagMessage.trim() || undefined,
          released_at,
          links: links.map(linkBody),
        });
      } else {
        const path = `${base}/${enc(t)}`;
        await api.put(path, { name: name.trim() || undefined, description: notes, released_at });
        for (const old of r.links) if (!links.some((l) => l.id === old.id)) await api.del(`${path}/links/${old.id}`);
        for (const l of links) if (l.id == null) await api.post(`${path}/links`, linkBody(l));
      }
      qc.invalidateQueries({ queryKey: ["releases", ctx.p.id] });
      qc.invalidateQueries({ queryKey: ["tags", ctx.p.id] });
      toast.ok(r ? "Release updated" : `Release ${name.trim() || t} created`);
      nav(`${ctx.web}/-/releases/${enc(t)}`);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="stack list-page">
      <ListHead title={r ? `Edit ${r.name}` : "New release"} sub={r ? <>on tag <code>{r.tag_name}</code></> : "Name a version and attach what people need to use it."} />
      <form className="stack" style={{ maxWidth: 760 }} onSubmit={submit}>
        <ErrorBox error={error} />
        {!r && (
          <>
            <Field label="Tag" hint={tag.trim() && !tagExists ? "A new tag: it is created with the release" : "An existing tag, or a new one"}>
              <input value={tag} onChange={(e) => setTag(e.target.value)} list="release-tags" placeholder="v1.0.0" spellCheck={false} autoFocus />
            </Field>
            <datalist id="release-tags">
              {known.filter((t) => !t.release).map((t) => (
                <option key={t.name} value={t.name} />
              ))}
            </datalist>
            {taken && <p className="error-text small">This tag already has a release: {taken}.</p>}
            {tag.trim() && !tagExists && (
              <div className="row wrap" style={{ gap: 10 }}>
                <Field label="Create the tag from" hint="A branch, tag or commit">
                  <input value={from} onChange={(e) => setFrom(e.target.value)} spellCheck={false} />
                </Field>
                <Field label="Tag message" hint="Optional: an annotated tag">
                  <input value={tagMessage} onChange={(e) => setTagMessage(e.target.value)} />
                </Field>
              </div>
            )}
          </>
        )}
        <Field label="Title" hint="The tag name when empty">
          <input value={name} onChange={(e) => setName(e.target.value)} placeholder={tag.trim() || "v1.0.0"} />
        </Field>
        <Field label="Release notes" hint="Markdown">
          <MarkdownEditor projectId={ctx.p.id} rows={10} value={notes} onChange={setNotes} label="Release notes" />
        </Field>
        <Field label="Release date" hint="Now when empty; a future date marks an upcoming release">
          <input type="datetime-local" value={date} onChange={(e) => setDate(e.target.value)} />
        </Field>
        <fieldset className="release-files">
          <legend className="small muted">Assets</legend>
          {links.length > 0 && (
            <ul className="stack" style={{ gap: 6 }}>
              {links.map((l) => (
                <li key={l.key} className="row" style={{ gap: 8 }}>
                  <LinkIcon type={l.link_type} />
                  <span className="grow ellipsis">
                    {l.name} {l.size != null && <span className="faint small">{bytes(l.size)}</span>}
                    {!l.url.startsWith("/-/") && <span className="faint small"> · {l.url}</span>}
                  </span>
                  <button type="button" className="icon-btn" aria-label={`Remove ${l.name}`} title="Remove" onClick={() => setLinks((ls) => ls.filter((x) => x.key !== l.key))}>
                    <Trash2 size={14} />
                  </button>
                </li>
              ))}
            </ul>
          )}
          {uploading > 0 && <p className="muted small">Uploading {uploading} file{uploading > 1 ? "s" : ""}…</p>}
          {adding ? (
            <div className="row wrap" style={{ gap: 8, alignItems: "flex-end" }}>
              <Field label="Link name">
                <input autoFocus value={adding.name} onChange={(e) => setAdding({ ...adding, name: e.target.value })} />
              </Field>
              <Field label="URL">
                <input value={adding.url} onChange={(e) => setAdding({ ...adding, url: e.target.value })} placeholder="https://" />
              </Field>
              <Field label="Type">
                <select value={adding.link_type} onChange={(e) => setAdding({ ...adding, link_type: e.target.value })}>
                  {LINK_TYPES.map((t) => (
                    <option key={t}>{t}</option>
                  ))}
                </select>
              </Field>
              <button
                type="button"
                className="btn"
                disabled={!adding.name.trim() || !adding.url.trim()}
                onClick={() => {
                  setLinks((ls) => [...ls, { key: `n${Date.now()}`, name: adding.name.trim(), url: adding.url.trim(), link_type: adding.link_type }]);
                  setAdding(null);
                }}
              >
                Add link
              </button>
              <button type="button" className="btn" onClick={() => setAdding(null)}>
                Cancel
              </button>
            </div>
          ) : (
            <div className="row" style={{ gap: 8 }}>
              <button type="button" className="btn small" onClick={() => fileInput.current?.click()}>
                <Paperclip size={13} /> Upload files
              </button>
              <button type="button" className="btn small" onClick={() => setAdding({ name: "", url: "", link_type: "other" })}>
                <Wrench size={13} /> Add a link
              </button>
            </div>
          )}
          <input
            ref={fileInput}
            type="file"
            multiple
            hidden
            aria-label="Release files"
            onChange={(e) => {
              files(Array.from(e.target.files ?? []));
              e.target.value = "";
            }}
          />
        </fieldset>
        <div className="row">
          <button className="btn primary large" aria-busy={busy || uploading > 0} disabled={busy || uploading > 0 || !tag.trim() || !!taken || (!tagExists && !from.trim())}>
            {busy ? "Saving…" : r ? "Save changes" : "Create release"}
          </button>
          <button type="button" className="btn large" onClick={() => nav(-1)}>
            Cancel
          </button>
        </div>
      </form>
    </div>
  );
}

