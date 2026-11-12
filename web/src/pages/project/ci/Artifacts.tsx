import { Link, useSearchParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { ArrowUp, Download, File, Folder } from "lucide-react";
import { api, bytes, qs, type S } from "../../../api/client";
import { Empty, ListHead, Q } from "../../../components/ui";
import type { Ctx } from "../Project";

/** A job's artifacts archive, folder by folder; files open (text) or download. */
export function ArtifactBrowser({ ctx, jobId }: { ctx: Ctx; jobId: number }) {
  const [params] = useSearchParams();
  const path = params.get("path") ?? "";
  const base = `/projects/${ctx.p.id}/jobs/${jobId}/artifacts`;
  const q = useQuery({ queryKey: ["artifacts", ctx.p.id, jobId, path], queryFn: () => api.get<S["ArtifactListing"]>(`${base}/tree${qs({ path })}`) });
  const here = `${ctx.web}/-/jobs/${jobId}/artifacts`;
  const parts = path.split("/").filter(Boolean);
  const up = parts.length > 0 ? parts.slice(0, -1).join("/") : null;
  return (
    <div className="stack list-page">
      <ListHead
        title="Artifacts"
        sub={
          <span className="row wrap" style={{ gap: 4 }}>
            <Link to={`${ctx.web}/-/jobs/${jobId}`}>job #{jobId}</Link>
            <span className="faint">/</span>
            <Link to={here}>archive</Link>
            {parts.map((p, i) => (
              <span key={i} className="row" style={{ gap: 4 }}>
                <span className="faint">/</span>
                <Link to={`${here}${qs({ path: `${parts.slice(0, i + 1).join("/")}/` })}`}>{p}</Link>
              </span>
            ))}
          </span>
        }
      >
        <a className="btn small" href={`/api/v1${base}`}>
          <Download size={13} /> Download all
        </a>
      </ListHead>
      <Q q={q}>
        {(d) =>
          d.entries.length === 0 ? (
            <Empty>This archive is empty.</Empty>
          ) : (
            <div className="list rows" aria-label="Files">
              {up != null && (
                <Link className="item" to={`${here}${qs({ path: up ? `${up}/` : undefined })}`}>
                  <ArrowUp size={15} className="faint" /> ..
                </Link>
              )}
              {d.entries.map((e) =>
                e.dir ? (
                  <Link key={e.path} className="item" to={`${here}${qs({ path: e.path })}`}>
                    <Folder size={15} className="ic-folder" />
                    <span className="grow ellipsis">{e.name}</span>
                  </Link>
                ) : (
                  <a key={e.path} className="item" href={`/api/v1${base}/file${qs({ path: e.path })}`} target="_blank" rel="noreferrer">
                    <File size={15} className="faint" />
                    <span className="grow ellipsis">{e.name}</span>
                    <span className="faint small">{bytes(e.size)}</span>
                  </a>
                ),
              )}
              {d.truncated && <div className="item muted small">The archive has more files than can be listed; download it to see them all.</div>}
            </div>
          )
        }
      </Q>
    </div>
  );
}
