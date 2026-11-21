import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useSearchParams } from "react-router-dom";
import { AlertTriangle, X } from "lucide-react";
import { api, qs, refLink, timeAgo, type S } from "../../api/client";
import { Avatar, Empty, ListHead, Q, UserLink } from "../../components/ui";
import { RefPicker } from "./Code";
import { StatusIcon, statusLabel } from "./ci/Status";
import type { Ctx } from "./Project";

type Block = S["MapBlock"];
type Job = S["JobDto"];
/** A link, or a block's parent holding it (`part`). */
type Edge = { from: string; to: string; kind: S["MapLinkKind"] | "part"; inferred: boolean; via: string };
type Box = { x: number; y: number };

// a card, the gaps between columns and rows, the margin
const W = 204;
const H = 92;
const GX = 52;
const GY = 14;
const PAD = 18;
/** The canvas shrinks to fit the board down to this scale; below, it scrolls. */
const MIN_SCALE = 0.75;
/** Narrower than this (a phone), the blocks are a list: a graph would be mostly out of sight. */
const LIST_BELOW = 560;

const KIND: Record<S["MapKind"], [label: string, color: string]> = {
  rust: ["Rust crate", "#c0581f"],
  node: ["npm package", "#4f9a3a"],
  python: ["Python project", "#3d7ab8"],
  go: ["Go module", "#1a9bb5"],
  migrations: ["Schema", "#8a5cd0"],
  docker: ["Image", "#2f6fe0"],
  service: ["Service", "#6b7a86"],
  ci: ["CI", "#b8901a"],
  docs: ["Docs", "#8a8178"],
  other: ["Block", "#8a8178"],
};
const VERB: Record<S["MapLinkKind"], string> = { uses: "uses", builds: "builds", runs: "runs", depends_on: "depends on", connects: "connects to", calls: "calls" };

/** A block's title: its name, but its id when the name is only its path (an image, a schema). */
const title = (b: Block) => (b.name === b.roots[0]?.path ? b.id : b.name);
const place = (b: Block) =>
  b.roots.length ? b.roots[0].path + (b.roots.length > 1 ? ` +${b.roots.length - 1}` : "") : (b.image?.replace(/@sha256:[0-9a-f]+$/, "") ?? b.manifests.map((m) => m.path).join(", "));
const plural = (n: number, one: string, many = `${one}s`) => `${n.toLocaleString()} ${n === 1 ? one : many}`;

/** The project's code as blocks (what is built, run or deployed apart), what each needs, and
 *  the people to ask; `gitRef`: the branch, tag or commit shown (the default branch if none). */
export function SmartView({ ctx, gitRef }: { ctx: Ctx; gitRef: string }) {
  const ref = gitRef || ctx.p.default_branch || "main";
  const q = useQuery({ queryKey: ["map", ctx.p.id, ref], queryFn: () => api.get<S["RepoMap"]>(`${ctx.base}/map${qs({ ref })}`), staleTime: 30_000 });
  return (
    <div className="stack smart-view">
      <ListHead title="Smart View" sub="The parts of this code, what each needs, and who to ask about them.">
        <RefPicker ctx={ctx} current={ref} path="" kind="smart_view" />
      </ListHead>
      <Q q={q}>{(map) => <MapView ctx={ctx} gitRef={ref} map={map} />}</Q>
    </div>
  );
}

function MapView({ ctx, gitRef, map }: { ctx: Ctx; gitRef: string; map: S["RepoMap"] }) {
  const [params, setParams] = useSearchParams();
  const byId = useMemo(() => new Map(map.blocks.map((b) => [b.id, b])), [map]);
  const lay = useMemo(() => layout(map), [map]);
  const jobs = useJobs(ctx, map.commit);
  const [main, scale, room] = useScale(lay.width);
  const list = room > 0 && room < LIST_BELOW;
  const asked = params.get("block");
  const chosen = asked && byId.has(asked) ? asked : null;
  const select = (id: string | null) =>
    setParams(
      (now) => {
        const next = new URLSearchParams(now);
        if (id) next.set("block", id);
        else next.delete("block");
        return next;
      },
      { replace: true },
    );
  useEffect(() => {
    if (!chosen) return;
    const close = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !e.defaultPrevented) select(null);
    };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  });
  // what is near the chosen block stays bright; the rest steps back
  const near = useMemo(() => {
    const s = new Set<string>();
    if (!chosen) return s;
    s.add(chosen);
    for (const e of lay.edges) {
      if (e.from === chosen) s.add(e.to);
      if (e.to === chosen) s.add(e.from);
    }
    return s;
  }, [chosen, lay]);

  if (map.config_error) {
    return (
      <div className="banner danger" role="alert">
        <AlertTriangle size={16} />
        <span className="grow small">
          <Link to={refLink(ctx.web, "blob", gitRef, "assay.toml")}>assay.toml</Link> could not be read, so nothing is mapped until it is fixed: <code>{map.config_error}</code>
        </span>
      </div>
    );
  }
  if (!map.blocks.length) {
    return (
      <Empty>
        No blocks found. They come from manifests: <code>Cargo.toml</code>, <code>package.json</code>, <code>pyproject.toml</code>, <code>go.mod</code>, a <code>Dockerfile</code>, a compose file or a CI configuration. An <code>assay.toml</code> at the top
        of the repository can declare others.
      </Empty>
    );
  }
  const card = (b: Block, style?: CSSProperties) => (
    <BlockCard key={b.id} b={b} style={style} status={blockStatus(b, jobs)} chosen={chosen === b.id} dim={!!chosen && !near.has(b.id)} onSelect={() => select(b.id)} />
  );
  return (
    <>
      <div className="smart-bar">
        <span>
          {plural(map.blocks.length, "block")} · {plural(map.links.length, "link")} · {plural(map.unassigned, "file")} in no block
        </span>
        {!list && <Legend />}
      </div>
      <div className={`smart-layout${chosen ? " open" : ""}`}>
        <div className="smart-main" ref={main}>
          {lay.graph.length > 0 &&
            (list ? (
              <div className="smart-grid list">{lay.order.map((id) => card(byId.get(id)!))}</div>
            ) : (
              <Board lay={lay} scale={scale} chosen={chosen} onClear={() => select(null)} render={(id, style) => card(byId.get(id)!, style)} />
            ))}
          {lay.loose.length > 0 && (
            <section className="smart-loose" aria-label="Not linked to other blocks">
              <h2>Not linked to other blocks</h2>
              <div className={`smart-grid${list ? " list" : scale < 1 ? " scaled" : ""}`} style={{ "--scale": scale } as CSSProperties}>
                {lay.loose.map((id) => card(byId.get(id)!))}
              </div>
            </section>
          )}
          <Notes ctx={ctx} gitRef={gitRef} map={map} />
        </div>
        {chosen && <Details ctx={ctx} gitRef={gitRef} map={map} lay={lay} b={byId.get(chosen)!} jobs={jobs} select={select} />}
      </div>
    </>
  );
}

/** The scale that fits a graph `width` wide in the element `ref` is put on (not below
 *  MIN_SCALE: the board scrolls instead), followed as it resizes. */
function useScale(width: number): [(el: HTMLDivElement | null) => void, number, number] {
  const [el, setEl] = useState<HTMLDivElement | null>(null);
  const [room, setRoom] = useState(0);
  useLayoutEffect(() => {
    if (!el) return;
    const measure = () => setRoom(el.clientWidth);
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [el]);
  // the board's border takes 2px
  return [setEl, room && width ? Math.max(MIN_SCALE, Math.min(1, (room - 2) / width)) : 1, room];
}

/** The graph: the linked blocks in columns, from what needs to what is needed. */
function Board({ lay, scale, chosen, onClear, render }: { lay: Layout; scale: number; chosen: string | null; onClear: () => void; render: (id: string, style: CSSProperties) => ReactNode }) {
  const board = useRef<HTMLDivElement>(null);
  const arrow = useId();
  useEffect(() => {
    if (chosen) board.current?.querySelector(`[data-id="${CSS.escape(chosen)}"]`)?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [chosen]);
  return (
    <div className="smart-board" ref={board} onClick={onClear}>
      <div className="smart-stage" style={{ width: lay.width * scale, height: lay.height * scale }}>
        <div className="smart-canvas" style={{ width: lay.width, height: lay.height, transform: scale < 1 ? `scale(${scale})` : undefined }}>
          <svg className="smart-edges" width={lay.width} height={lay.height} aria-hidden>
            <defs>
              {["", "-hl"].map((v) => (
                <marker key={v} id={`${arrow}${v}`} viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
                  <path d="M0,1 L9,5 L0,9 z" className={`smart-arrow${v}`} />
                </marker>
              ))}
            </defs>
            {lay.edges.map((e) => {
              const mine = !!chosen && (e.from === chosen || e.to === chosen);
              const cls = [e.kind === "part" ? "part" : e.inferred ? "inferred" : "declared", mine ? "hl" : chosen ? "dim" : ""].join(" ");
              return <path key={`${e.from}>${e.to}:${e.kind}`} d={path(e, lay)} className={cls} markerEnd={e.kind === "part" ? undefined : `url(#${arrow}${mine ? "-hl" : ""})`} />;
            })}
          </svg>
          {lay.graph.map((id) => {
            const p = lay.box.get(id)!;
            return render(id, { left: p.x, top: p.y });
          })}
        </div>
      </div>
    </div>
  );
}

function BlockCard({ b, style, status, chosen, dim, onSelect }: { b: Block; style?: CSSProperties; status: string | null; chosen: boolean; dim: boolean; onSelect: () => void }) {
  const [kind, color] = KIND[b.kind];
  const lang = b.languages[0];
  return (
    <button
      type="button"
      className={`smart-card${chosen ? " selected" : ""}${dim ? " dim" : ""}`}
      style={{ ...style, "--k": color } as CSSProperties}
      data-id={b.id}
      aria-pressed={chosen}
      onClick={(e) => {
        e.stopPropagation();
        onSelect();
      }}
    >
      <span className="smart-kind">
        <i className="dot" />
        <span>{`${kind}${b.role ? ` · ${b.role}` : ""}`}</span>
        {(b.bus_factor === 1 || status) && (
          <span className="smart-flags">
            {b.bus_factor === 1 && (
              <span className="smart-solo" title="One person made most of its recent changes">
                1 person
              </span>
            )}
            {status && (
              <span title={`CI: ${statusLabel(status)}`}>
                <StatusIcon status={status} size={13} />
              </span>
            )}
          </span>
        )}
      </span>
      <span className="smart-name">{title(b)}</span>
      <span className="smart-place">{place(b)}</span>
      <span className="smart-foot">
        {b.files > 0 && <span className="smart-files">{`${plural(b.files, "file")}${lang ? ` · ${lang.name}` : ""}`}</span>}
        <span className="smart-people">
          {b.people.slice(0, 3).map((p) => (
            <Avatar key={p.name} name={p.name} username={p.username} title={p.name} />
          ))}
        </span>
      </span>
    </button>
  );
}

function Details({ ctx, gitRef, map, lay, b, jobs, select }: { ctx: Ctx; gitRef: string; map: S["RepoMap"]; lay: Layout; b: Block; jobs: Map<string, Job>; select: (id: string | null) => void }) {
  const byId = new Map(map.blocks.map((x) => [x.id, x]));
  const [kind, color] = KIND[b.kind];
  const go = (id: string) => (
    <button type="button" className="smart-go" onClick={() => select(id)}>
      {byId.get(id) ? title(byId.get(id)!) : id}
    </button>
  );
  const at = (p: S["MapPath"]) => (p.dir ? refLink(ctx.web, "tree", gitRef, p.path === "." ? undefined : p.path) : refLink(ctx.web, "blob", gitRef, p.path));
  // the file that says so, by its name (its path on hover); a guess says so
  const said = (e: Edge) => (
    <>
      {e.inferred && <span className="smart-tag">a guess</span>}
      {e.via && (
        <Link className="meta" to={refLink(ctx.web, "blob", gitRef, e.via)} title={e.via}>
          {e.via.split("/").pop()}
        </Link>
      )}
    </>
  );
  const needs = lay.edges.filter((e) => e.from === b.id && e.kind !== "part");
  const holds = lay.edges.filter((e) => e.from === b.id && e.kind === "part");
  const neededBy = lay.edges.filter((e) => e.to === b.id && e.kind !== "part");
  const total = b.languages.reduce((n, l) => n + l.files, 0) || 1;
  const langs = b.languages.slice(0, 5);
  const pct = (n: number) => Math.max(1, Math.round((100 * n) / total));
  return (
    <aside className="side-card smart-panel" aria-label={`${title(b)}, details`}>
      <header>
        <div className="smart-kind" style={{ "--k": color } as CSSProperties}>
          <i className="dot" />
          {kind}
          {b.role && ` · ${b.role}`}
          {b.configured && " · from assay.toml"}
        </div>
        <button type="button" className="icon-btn" aria-label="Close" title="Close (Esc)" onClick={() => select(null)}>
          <X size={14} />
        </button>
      </header>
      <div>
        <h2>{title(b)}</h2>
        {b.roots.map((r) => (
          <Link key={r.path} className="smart-root" to={at(r)}>
            {r.path === "." ? "/" : r.path}
          </Link>
        ))}
        {b.image && <div className="smart-root">{b.image}</div>}
        {b.parent && <div className="muted small">Part of {go(b.parent)}</div>}
      </div>
      {b.files > 0 && (
        <section>
          <h3>{plural(b.files, "file")}</h3>
          <div className="lang-bar" role="img" aria-label={langs.map((l) => `${l.name} ${pct(l.files)}%`).join(", ")}>
            {langs.map((l) => (
              <span key={l.name} style={{ flexGrow: l.files, background: l.color ?? "var(--text-faint)" }} title={`${l.name} ${pct(l.files)}%`} />
            ))}
          </div>
          <div className="smart-langs">
            {langs.map((l) => (
              <span key={l.name}>
                <span className="lang-dot" style={{ background: l.color ?? "var(--text-faint)" }} />
                {l.name} <span className="muted">{pct(l.files)}%</span>
              </span>
            ))}
          </div>
        </section>
      )}
      {(b.owners.length > 0 || b.people.length > 0) && (
        <section>
          <h3>People to ask</h3>
          <ul>
            {b.owners.map((o) => (
              <li key={o}>
                <span className="grow">{o.startsWith("@") ? <Link to={`/${o.slice(1)}`}>{o}</Link> : o}</span>
                <span className="smart-tag" title="CODEOWNERS or assay.toml">
                  declared
                </span>
              </li>
            ))}
            {b.people.map((p) => (
              <li key={p.name}>
                <Avatar name={p.name} username={p.username} />
                <span className="grow">{p.username ? <UserLink username={p.username}>{p.name}</UserLink> : p.name}</span>
                <span className="meta" title={p.last}>
                  {plural(p.commits, "commit")} · {timeAgo(p.last)}
                </span>
              </li>
            ))}
          </ul>
          {b.bus_factor === 1 && b.people.length > 0 && <p className="smart-note">One person made most of its recent changes: worth sharing what they know.</p>}
          {b.people.length > 0 && <p className="muted small">From the commits of the last {plural(map.days, "day")}.</p>}
        </section>
      )}
      <EdgeList title="Needs">
        {needs.map((e) => (
          <li key={`${e.to}:${e.kind}`}>
            <span className="grow">
              {e.kind !== "part" && VERB[e.kind]} {go(e.to)}
            </span>
            {said(e)}
          </li>
        ))}
      </EdgeList>
      <EdgeList title="Holds">
        {holds.map((e) => (
          <li key={e.to}>
            <span className="grow">{go(e.to)}</span>
          </li>
        ))}
      </EdgeList>
      <EdgeList title="Needed by">
        {neededBy.map((e) => (
          <li key={`${e.from}:${e.kind}`}>
            <span className="grow">
              {go(e.from)} {e.kind !== "part" && VERB[e.kind]} it
            </span>
            {said(e)}
          </li>
        ))}
      </EdgeList>
      {b.jobs.length > 0 && (
        <section>
          <h3>CI jobs</h3>
          <ul>
            {b.jobs.map((name) => {
              const j = jobs.get(name);
              return (
                <li key={name}>
                  {j && <StatusIcon status={j.status} size={14} allowFailure={j.allow_failure} />}
                  <span className="grow">{j ? <Link to={`${ctx.web}/-/jobs/${j.id}`}>{name}</Link> : <code>{name}</code>}</span>
                  {j && <span className="meta">{statusLabel(j.status, j.allow_failure)}</span>}
                </li>
              );
            })}
          </ul>
        </section>
      )}
      {b.manifests.length > 0 && (
        <section>
          <h3>Found in</h3>
          <ul>
            {b.manifests.map((m) => (
              <li key={m.path}>
                <Link className="grow smart-root" to={at(m)}>
                  {m.path}
                </Link>
              </li>
            ))}
          </ul>
        </section>
      )}
    </aside>
  );
}

function EdgeList({ title, children }: { title: string; children: ReactNode[] }) {
  if (!children.length) return null;
  return (
    <section>
      <h3>{title}</h3>
      <ul>{children}</ul>
    </section>
  );
}

function Legend() {
  const sample = (dash?: string) => (
    <svg width="22" height="6" aria-hidden>
      <line x1="0" y1="3" x2="22" y2="3" stroke="currentColor" strokeWidth="1.5" strokeDasharray={dash} />
    </svg>
  );
  return (
    <span className="smart-legend">
      <span>{sample()} declared</span>
      <span>{sample("5 3")} a guess</span>
      <span>{sample("2 3")} part of</span>
      <span>arrows point to what is needed</span>
    </span>
  );
}

/** The files in no block, what could not be read, and how to correct the map. */
function Notes({ ctx, gitRef, map }: { ctx: Ctx; gitRef: string; map: S["RepoMap"] }) {
  return (
    <div className="smart-notes">
      {map.unassigned > 0 && (
        <p>
          {plural(map.unassigned, "file is", "files are")} in no block:{" "}
          {map.unassigned_sample.map((f, i) => (
            <span key={f}>
              {i > 0 && ", "}
              <Link to={refLink(ctx.web, "blob", gitRef, f)}>
                <code>{f}</code>
              </Link>
            </span>
          ))}
          {map.unassigned > map.unassigned_sample.length && ", …"}
        </p>
      )}
      {map.warnings.length > 0 && (
        <div>
          <p>Not read:</p>
          <ul>
            {map.warnings.map((w) => (
              <li key={w}>
                <code>{w}</code>
              </li>
            ))}
          </ul>
        </div>
      )}
      {map.truncated && <p>This repository is large: only its first 100,000 files, and the latest 5,000 commits, were read.</p>}
      <p>
        The blocks come from the manifests. An <code>assay.toml</code> at the top of the repository renames, merges, ignores or adds blocks and links, and names who to ask.
      </p>
    </div>
  );
}

/** The jobs of the latest pipeline of `commit`, by name (their latest attempt). */
function useJobs(ctx: Ctx, commit: string): Map<string, Job> {
  const statuses = useQuery({
    queryKey: ["commit-status", ctx.p.id, commit],
    queryFn: () => api.get<S["CommitStatus"][]>(`/projects/${ctx.p.id}/commit_statuses${qs({ shas: commit })}`),
    staleTime: 15_000,
  });
  const iid = statuses.data?.find((s) => s.sha === commit)?.pipeline_iid;
  const pipeline = useQuery({
    queryKey: ["pipeline", ctx.p.id, iid],
    queryFn: () => api.get<S["PipelineDetail"]>(`/projects/${ctx.p.id}/pipelines/${iid}`),
    enabled: iid != null,
    staleTime: 15_000,
  });
  return useMemo(() => new Map((pipeline.data?.jobs ?? []).filter((j) => !j.retried).map((j) => [j.name, j])), [pipeline.data]);
}

const WAITING = ["pending", "created", "preparing", "waiting_for_resource", "scheduled"];

/** A block's CI: failed when one of its jobs did (unless allowed to), running while one runs. */
function blockStatus(b: Block, jobs: Map<string, Job>): string | null {
  const mine = b.jobs.map((n) => jobs.get(n)).filter((j): j is Job => !!j);
  if (!mine.length) return null;
  if (mine.some((j) => j.status === "failed" && !j.allow_failure)) return "failed";
  if (mine.some((j) => j.status === "running" || j.status === "canceling")) return "running";
  if (mine.some((j) => WAITING.includes(j.status))) return "pending";
  if (mine.every((j) => ["success", "skipped", "manual"].includes(j.status) || (j.status === "failed" && j.allow_failure))) return "success";
  return mine[0].status;
}

/** `order`: the linked blocks column by column, top to bottom (a list's order). */
type Layout = { edges: Edge[]; graph: string[]; order: string[]; loose: string[]; box: Map<string, Box>; ends: Map<string, number>; width: number; height: number };

/**
 * Columns: each block one right of the furthest block needing it (a link back is left out),
 * one nothing needs just before what it needs; in each column, ordered by where their
 * neighbours are. A block linked to none is left out of the graph (`loose`).
 */
function layout(map: S["RepoMap"]): Layout {
  const known = new Set(map.blocks.map((b) => b.id));
  const edges: Edge[] = map.links.filter((l) => known.has(l.from) && known.has(l.to)).map((l) => ({ from: l.from, to: l.to, kind: l.kind, inferred: l.inferred, via: l.via }));
  const linkedPair = (a: string, b: string) => map.links.some((l) => (l.from === a && l.to === b) || (l.from === b && l.to === a));
  for (const b of map.blocks) {
    if (b.parent && known.has(b.parent) && !linkedPair(b.parent, b.id)) edges.push({ from: b.parent, to: b.id, kind: "part", inferred: false, via: "" });
  }
  const linked = new Set(edges.flatMap((e) => [e.from, e.to]));
  const graph = map.blocks.filter((b) => linked.has(b.id)).map((b) => b.id);
  const loose = map.blocks.filter((b) => !linked.has(b.id)).map((b) => b.id);
  const ins = new Map(graph.map((id) => [id, [] as Edge[]]));
  const outs = new Map(graph.map((id) => [id, [] as Edge[]]));
  for (const e of edges) {
    outs.get(e.from)!.push(e);
    ins.get(e.to)!.push(e);
  }

  const layer = new Map<string, number>();
  const visiting = new Set<string>();
  const layerOf = (id: string): number => {
    const have = layer.get(id);
    if (have !== undefined) return have;
    visiting.add(id);
    let l = 0;
    for (const e of ins.get(id)!) if (!visiting.has(e.from)) l = Math.max(l, layerOf(e.from) + 1);
    visiting.delete(id);
    layer.set(id, l);
    return l;
  };
  graph.forEach(layerOf);
  for (const id of graph) {
    if (ins.get(id)!.length || !outs.get(id)!.length) continue;
    layer.set(id, Math.max(0, Math.min(...outs.get(id)!.map((e) => layer.get(e.to)!)) - 1));
  }
  // columns left empty close up
  const used = [...new Set(layer.values())].sort((a, b) => a - b);
  const cols: string[][] = used.map(() => []);
  for (const id of graph) cols[used.indexOf(layer.get(id)!)].push(id);
  const rows = Math.max(0, ...cols.map((c) => c.length));

  // a few sweeps each way, each block towards the mean place of its neighbours
  const slot = new Map<string, number>();
  const settle = () => cols.forEach((c) => c.forEach((id, i) => slot.set(id, i - (c.length - 1) / 2)));
  settle();
  const mean = (xs: number[]) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : null);
  const order = (c: string[], key: (id: string) => number | null) => {
    const k = new Map(c.map((id) => [id, key(id) ?? slot.get(id)!]));
    c.sort((a, b) => k.get(a)! - k.get(b)!);
    settle();
  };
  for (let pass = 0; pass < 4; pass++) {
    for (let l = 1; l < cols.length; l++) order(cols[l], (id) => mean(ins.get(id)!.map((e) => slot.get(e.from)!)));
    for (let l = cols.length - 2; l >= 0; l--) order(cols[l], (id) => mean(outs.get(id)!.map((e) => slot.get(e.to)!)));
  }
  const box = new Map<string, Box>();
  cols.forEach((c, l) => c.forEach((id, i) => box.set(id, { x: PAD + l * (W + GX), y: PAD + ((rows - c.length) / 2 + i) * (H + GY) })));

  // the ends of the edges spread along each card's side, in the order of what is at their other end
  const ends = new Map<string, number>();
  const spread = (list: Edge[], side: string, other: (e: Edge) => string) => {
    list.sort((a, b) => box.get(other(a))!.y - box.get(other(b))!.y);
    list.forEach((e, i) => ends.set(`${side}:${e.from}>${e.to}`, (i - (list.length - 1) / 2) * Math.min(12, 48 / list.length)));
  };
  for (const id of graph) {
    const x = box.get(id)!.x;
    spread(outs.get(id)!.filter((e) => box.get(e.to)!.x > x), "out", (e) => e.to);
    spread(ins.get(id)!.filter((e) => box.get(e.from)!.x <= x), "in", (e) => e.from);
  }
  // room on the right for a link between two blocks of the last column
  const width = cols.length ? PAD * 2 + cols.length * W + (cols.length - 1) * GX + 24 : 0;
  const height = rows ? PAD * 2 + rows * H + (rows - 1) * GY : 0;
  return { edges, graph, order: cols.flat(), loose, box, ends, width, height };
}

function path(e: Edge, lay: Layout) {
  const a = lay.box.get(e.from)!;
  const b = lay.box.get(e.to)!;
  const y1 = a.y + H / 2 + (lay.ends.get(`out:${e.from}>${e.to}`) ?? 0);
  const y2 = b.y + H / 2 + (lay.ends.get(`in:${e.from}>${e.to}`) ?? 0);
  if (b.x > a.x) {
    const x1 = a.x + W;
    const x2 = b.x - 3;
    const dx = Math.max(28, (x2 - x1) / 2);
    return `M${x1},${y1} C${x1 + dx},${y1} ${x2 - dx},${y2} ${x2},${y2}`;
  }
  if (b.x < a.x) {
    const x1 = a.x;
    const x2 = b.x + W + 3;
    const dx = Math.max(28, (x1 - x2) / 2);
    return `M${x1},${y1} C${x1 - dx},${y1} ${x2 + dx},${y2} ${x2},${y2}`;
  }
  const x = a.x + W;
  return `M${x},${y1} C${x + 44},${y1} ${x + 44},${y2} ${x + 3},${y2}`;
}
