import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Container } from "lucide-react";
import { api, bytes, timeAgo, type S } from "../../api/client";
import { Badge, Empty, ErrorBox, Field, ListHead, Q, Section, useCopy } from "../../components/ui";
import type { Ctx } from "./Project";

/** The project's container images (ADR-0022): how to push, images, tags, cleanup. */
export function Registry({ ctx }: { ctx: Ctx }) {
  const q = useQuery({ queryKey: ["registry", ctx.p.id], queryFn: () => api.get<S["RegistryInfo"]>(`/projects/${ctx.p.id}/registry`) });
  return (
    <div className="stack">
      <ListHead title="Container registry" />
      <Q q={q}>
        {(info) =>
          !info.enabled ? (
            <Empty>The container registry is off on this server.</Empty>
          ) : (
            <>
              <HowTo info={info} />
              {info.repositories.length === 0 ? (
                <Empty>
                  <Container size={28} />
                  <p>No images yet. Push one with the commands above, or from CI with $CI_REGISTRY_IMAGE.</p>
                </Empty>
              ) : (
                <div className="stack">
                  {info.repositories.map((r) => (
                    <ImageRepo key={r.id} ctx={ctx} repo={r} />
                  ))}
                </div>
              )}
              {ctx.p.can_admin && <CleanupPolicy ctx={ctx} />}
            </>
          )
        }
      </Q>
    </div>
  );
}

function HowTo({ info }: { info: S["RegistryInfo"] }) {
  const lines = [`docker login ${info.host}`, `docker build -t ${info.image} .`, `docker push ${info.image}`];
  const [copied, copy] = useCopy();
  return (
    <details className="card registry-howto">
      <summary>Push an image</summary>
      <p className="small muted">
        Sign in with your username and a personal access token (or your password without two-factor authentication). CI jobs use
        $CI_REGISTRY_USER and $CI_REGISTRY_PASSWORD.
      </p>
      <pre className="mono small">{lines.join("\n")}</pre>
      <button className="btn small" onClick={() => copy(lines.join("\n"))}>
        {copied ? "Copied" : "Copy"}
      </button>
    </details>
  );
}

function ImageRepo({ ctx, repo }: { ctx: Ctx; repo: S["ImageRepository"] }) {
  const qc = useQueryClient();
  const [open, setOpen] = useState(false);
  const key = ["registry-tags", ctx.p.id, repo.id];
  const tags = useQuery({
    queryKey: key,
    queryFn: () => api.get<S["Tag"][]>(`/projects/${ctx.p.id}/registry/repositories/${repo.id}/tags`),
    enabled: open,
  });
  const [error, setError] = useState<unknown>(null);
  const refresh = () => {
    qc.invalidateQueries({ queryKey: key });
    qc.invalidateQueries({ queryKey: ["registry", ctx.p.id] });
  };
  const delTag = useMutation({
    mutationFn: (tag: string) => api.del(`/projects/${ctx.p.id}/registry/repositories/${repo.id}/tags/${encodeURIComponent(tag)}`),
    onSuccess: refresh,
    onError: setError,
  });
  const delRepo = useMutation({
    mutationFn: () => api.del(`/projects/${ctx.p.id}/registry/repositories/${repo.id}`),
    onSuccess: refresh,
    onError: setError,
  });
  return (
    <section className="card registry-repo" aria-label={`Image ${repo.location}`}>
      <div className="row wrap">
        <button className="link-btn grow ellipsis mono" onClick={() => setOpen(!open)} aria-expanded={open}>
          {repo.location}
        </button>
        <Badge>
          {repo.tags_count} tag{repo.tags_count === 1 ? "" : "s"}
        </Badge>
        <span className="small faint">{bytes(repo.size)}</span>
        {ctx.p.can_admin && (
          <button
            className="btn small danger"
            onClick={() => {
              if (window.confirm(`Delete ${repo.location} and all its tags?`)) delRepo.mutate();
            }}
          >
            Delete image
          </button>
        )}
      </div>
      <ErrorBox error={error} />
      {open && (
        <Q q={tags}>
          {(list) =>
            list.length === 0 ? (
              <p className="small muted">No tags.</p>
            ) : (
              <div className="list">
                {list.map((t) => (
                  <div className="item" key={t.name}>
                    <code className="grow">{t.name}</code>
                    <span className="small faint mono" title={t.digest}>
                      {t.digest.slice(7, 19)}
                    </span>
                    <span className="small faint">{bytes(t.total_size)}</span>
                    <span className="small faint">{timeAgo(t.updated_at)}</span>
                    {ctx.p.can_push && (
                      <button className="btn small danger" onClick={() => delTag.mutate(t.name)}>
                        Delete
                      </button>
                    )}
                  </div>
                ))}
              </div>
            )
          }
        </Q>
      )}
    </section>
  );
}

function CleanupPolicy({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const key = ["registry-cleanup", ctx.p.id];
  const q = useQuery({ queryKey: key, queryFn: () => api.get<S["CleanupPolicy"]>(`/projects/${ctx.p.id}/registry/cleanup_policy`) });
  return (
    <Section title="Cleanup policy" description="Runs every day: removes old tags you no longer need. The latest tag is always kept.">
      <Q q={q}>{(p) => <CleanupForm ctx={ctx} initial={p} onSaved={() => qc.invalidateQueries({ queryKey: key })} />}</Q>
    </Section>
  );
}

function CleanupForm({ ctx, initial, onSaved }: { ctx: Ctx; initial: S["CleanupPolicy"]; onSaved: () => void }) {
  const qc = useQueryClient();
  const [p, setP] = useState(initial);
  const [error, setError] = useState<unknown>(null);
  const [note, setNote] = useState<string | null>(null);
  const save = useMutation({
    mutationFn: () => api.put(`/projects/${ctx.p.id}/registry/cleanup_policy`, p),
    onSuccess: () => {
      setError(null);
      setNote("Saved.");
      onSaved();
    },
    onError: setError,
  });
  const run = useMutation({
    mutationFn: () => api.post<{ removed: number }>(`/projects/${ctx.p.id}/registry/cleanup`),
    onSuccess: (r) => {
      setNote(`${r.removed} tag${r.removed === 1 ? "" : "s"} removed.`);
      qc.invalidateQueries({ queryKey: ["registry", ctx.p.id] });
    },
    onError: setError,
  });
  const num = (v: string) => (v === "" ? null : Math.max(0, Number(v) || 0));
  return (
    <form
      className="stack"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <ErrorBox error={error} />
      <label className="check">
        <input type="checkbox" checked={p.enabled} onChange={(e) => setP({ ...p, enabled: e.target.checked })} />
        Clean up tags every day
      </label>
      <div className="grid2">
        <Field label="Keep the newest" hint="tags of each image (empty: no minimum)">
          <input type="number" min={0} value={p.keep_n ?? ""} onChange={(e) => setP({ ...p, keep_n: num(e.target.value) })} />
        </Field>
        <Field label="Remove tags older than, in days" hint="empty: any age">
          <input type="number" min={0} value={p.older_than_days ?? ""} onChange={(e) => setP({ ...p, older_than_days: num(e.target.value) })} />
        </Field>
        <Field label="Tags that may be removed" hint="a regular expression over the whole name, e.g. .* or v.+">
          <input className="mono" value={p.name_regex} onChange={(e) => setP({ ...p, name_regex: e.target.value })} />
        </Field>
        <Field label="Tags always kept" hint="e.g. main|stable|v\d+\.\d+\.\d+">
          <input className="mono" value={p.name_regex_keep} onChange={(e) => setP({ ...p, name_regex_keep: e.target.value })} />
        </Field>
      </div>
      <div className="row wrap">
        <button className="btn primary">Save policy</button>
        <button type="button" className="btn" onClick={() => run.mutate()} disabled={run.isPending}>
          Run now
        </button>
        {note && <span className="small muted">{note}</span>}
      </div>
    </form>
  );
}
