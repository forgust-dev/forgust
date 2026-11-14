import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, timeAgo, type S } from "../api/client";
import { Empty, Pager, Q } from "./ui";

/**
 * The audit events at `path`, newest first, `perPage` at a time (what the endpoint
 * returns per call; it pages with `before=<id>`). `target` adds what each event is about.
 */
export function AuditEvents({ path, perPage, target = false }: { path: string; perPage: number; target?: boolean }) {
  // the `before` cursor of each page shown so far, the first page having none
  const [cursors, setCursors] = useState<(number | undefined)[]>([undefined]);
  const before = cursors[cursors.length - 1];
  const q = useQuery({
    queryKey: ["audit", path, before],
    queryFn: () => api.get<S["AuditEvent"][]>(before == null ? path : `${path}?before=${before}`),
  });
  const page = cursors.length;
  const older = () => {
    const last = q.data?.at(-1);
    if (last) setCursors((c) => [...c, last.id]);
  };
  return (
    <div className="stack">
      <Q q={q}>
        {(events) =>
          events.length === 0 ? (
            <Empty>{page === 1 ? "No events." : "No older events."}</Empty>
          ) : (
            <div className="list">
              {events.map((e) => (
                <div className="item small" key={e.id}>
                  <code>{e.action}</code>
                  {target && (
                    <span className="muted">
                      {e.target_type} {e.target_id}
                    </span>
                  )}
                  <span className="grow muted ellipsis">{JSON.stringify(e.details)}</span>
                  <span>{e.principal_name ?? e.principal_kind}</span>
                  <span className="faint">{timeAgo(e.created_at)}</span>
                </div>
              ))}
            </div>
          )
        }
      </Q>
      <Pager page={page} count={q.data?.length ?? 0} perPage={perPage} onPage={(n) => (n < page ? setCursors((c) => c.slice(0, n)) : older())} labels={["Newer", "Older"]} />
    </div>
  );
}
