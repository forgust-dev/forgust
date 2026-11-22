import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useSearchParams } from "react-router-dom";
import { AlertTriangle, X } from "lucide-react";
import {
  Background,
  BackgroundVariant,
  BaseEdge,
  Controls,
  EdgeLabelRenderer,
  Handle,
  MiniMap,
  Position,
  ReactFlow,
  ReactFlowProvider,
  getBezierPath,
  useReactFlow,
  type Edge as FlowEdge,
  type EdgeProps,
  type Node,
  type NodeProps,
} from "@xyflow/react";
import "@xyflow/react/dist/base.css";
import ELK from "elkjs/lib/elk.bundled.js";
import type { ElkExtendedEdge, ElkNode } from "elkjs/lib/elk-api";
import { api, qs, refLink, timeAgo, type S } from "../../api/client";
import { Avatar, Empty, ListHead, Q, UserLink } from "../../components/ui";
import { RefPicker } from "./Code";
import { StatusIcon, statusLabel } from "./ci/Status";
import type { Ctx } from "./Project";

type Block = S["MapBlock"];
type Job = S["JobDto"];
/** A link, or a block's parent holding it (`part`). */
type Edge = { from: string; to: string; kind: S["MapLinkKind"] | "part"; inferred: boolean; via: string };

// a card, and the margin around what a block holds
const W = 204;
const H = 92;
const INSET = 12;
/** Under this zoom the cards' text is too small to read: the graph overflows the frame instead,
 *  from its start (what needs the rest, on the left), and pans. */
const READABLE = 0.8;
/** The frame is as tall as the graph at the zoom it opens at, within these (the most: a share of
 *  the window's height). */
const FRAME_MIN = 440;
const FRAME_MAX = 0.72;
/** Around the graph in the frame. */
const MARGIN = 20;
/** The details over the frame's right side (styles.css). */
const PANEL = 340;
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
const ROLE: Record<S["MapRole"], string> = {
  web: "web",
  api: "API",
  cli: "CLI",
  library: "library",
  database: "database",
  cache: "cache",
  queue: "queue",
  search: "search",
  storage: "storage",
  mail: "mail",
};
/** A phone's list, by what the blocks are: what runs first, the data last. */
const GROUPS: [title: string, kinds: S["MapKind"][]][] = [
  ["Services", ["service"]],
  ["Images", ["docker"]],
  ["Code", ["rust", "node", "python", "go"]],
  ["Schemas", ["migrations"]],
  ["CI and docs", ["ci", "docs"]],
  ["Other", ["other"]],
];
/** That one person knows a block only says something once it changed this many times. */
const SOLO_FROM = 5;
const solo = (b: Block) => b.bus_factor === 1 && b.people.reduce((n, p) => n + p.commits, 0) >= SOLO_FROM;
const SOLO = "Only one person made a fifth or more of its recent changes";

/** A block's title: its name, but its id when the name is only its path (an image, a schema). */
const title = (b: Block) => (b.name === b.roots[0]?.path ? b.id : b.name);
const where = (path: string) => (path === "." ? "whole repository" : path);
const place = (b: Block) =>
  b.roots.length
    ? where(b.roots[0].path) + (b.roots.length > 1 ? ` +${b.roots.length - 1}` : "")
    : (b.image?.replace(/@sha256:[0-9a-f]+$/, "") ?? b.manifests.map((m) => m.path).join(", "));
const plural = (n: number, one: string, many = `${one}s`) => `${n.toLocaleString()} ${n === 1 ? one : many}`;

/** The project's code as blocks (what is built, run or deployed apart), what each needs, and
 *  the people to ask; `gitRef`: the branch, tag or commit shown (the default branch if none). */
export function SmartView({ ctx, gitRef }: { ctx: Ctx; gitRef: string }) {
  const ref = gitRef || ctx.p.default_branch || "main";
  // the map, and where its blocks go: laid out with it, so that the page comes in at once
  const q = useQuery({
    queryKey: ["map", ctx.p.id, ref],
    queryFn: async () => {
      const map = await api.get<S["RepoMap"]>(`${ctx.base}/map${qs({ ref })}`);
      return { map, lay: await plan(map) };
    },
    staleTime: 30_000,
  });
  return (
    <div className="stack smart-view">
      <ListHead title="Smart View" sub="The parts of this code, what each needs, and who to ask about them.">
        <RefPicker ctx={ctx} current={ref} path="" kind="smart_view" />
      </ListHead>
      <Q q={q}>{({ map, lay }) => <MapView ctx={ctx} gitRef={ref} map={map} lay={lay} />}</Q>
    </div>
  );
}

function MapView({ ctx, gitRef, map, lay }: { ctx: Ctx; gitRef: string; map: S["RepoMap"]; lay: Plan }) {
  const [params, setParams] = useSearchParams();
  const byId = useMemo(() => new Map(map.blocks.map((b) => [b.id, b])), [map]);
  const jobs = useJobs(ctx, map.commit);
  const [main, room] = useWidth();
  const list = room > 0 && room < LIST_BELOW;
  // the graph on its canvas, once there is room for it
  const framed = room >= LIST_BELOW && lay.graph.length > 0;
  const asked = params.get("block");
  const chosen = asked && byId.has(asked) ? asked : null;
  const select = useCallback(
    (id: string | null) =>
      setParams(
        (now) => {
          const next = new URLSearchParams(now);
          if (id) next.set("block", id);
          else next.delete("block");
          return next;
        },
        { replace: true },
      ),
    [setParams],
  );
  useEffect(() => {
    if (!chosen) return;
    const close = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !e.defaultPrevented) select(null);
    };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [chosen, select]);
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
  const card = (b: Block, links?: string) => (
    <BlockCard key={b.id} b={b} status={blockStatus(b, jobs)} chosen={chosen === b.id} dim={!!chosen && !near.has(b.id)} onSelect={() => select(b.id)} links={links} />
  );
  const details = chosen ? <Details ctx={ctx} gitRef={gitRef} map={map} lay={lay} b={byId.get(chosen)!} jobs={jobs} select={select} /> : null;
  const alone = map.blocks.filter(solo).length;
  // a phone's list: what each needs, what uses it and what holds it, since no line shows it
  const counts = (id: string) => {
    const mine = (e: Edge) => e.kind !== "part";
    const needs = lay.edges.filter((e) => e.from === id && mine(e)).length;
    const usedBy = lay.edges.filter((e) => e.to === id && mine(e)).length;
    const holder = lay.edges.find((e) => e.to === id && e.kind === "part")?.from;
    const holds = lay.edges.filter((e) => e.from === id && e.kind === "part").length;
    const said = [needs && `needs ${needs}`, usedBy && `used by ${usedBy}`, holder && `part of ${title(byId.get(holder)!)}`, holds && `contains ${holds}`].filter(Boolean);
    return said.length ? said.join(" · ") : "not linked to other blocks";
  };
  const rank = new Map([...lay.order, ...lay.loose].map((id, i) => [id, i]));
  return (
    <>
      <div className="smart-bar">
        <span>
          {plural(map.blocks.length, "block")} · {plural(map.links.length, "link")} · {plural(map.unassigned, "file")} in no block
          {alone > 0 && <span title={SOLO}> · {plural(alone, "block")} known by one person</span>}
        </span>
        {!list && lay.drawn.length > 0 && <Legend />}
      </div>
      <p className="smart-guide">{framed && "From left to right: what runs, what it is built from, the code, its data. "}Choose a block to see what it needs and who to ask.</p>
      {/* the details: over the canvas when there is one, else beside the blocks */}
      <div className={`smart-layout${details && room > 0 && !framed ? " open" : ""}`}>
        <div className="smart-main" ref={main}>
          {framed && <Canvas key={lay.id} lay={lay} byId={byId} jobs={jobs} chosen={chosen} near={near} select={select} room={room} panel={details} />}
          {list
            ? GROUPS.map(([group, kinds]) => {
                const here = map.blocks.filter((b) => kinds.includes(b.kind)).sort((a, b) => rank.get(a.id)! - rank.get(b.id)!);
                return (
                  here.length > 0 && (
                    <section key={group} className="smart-group" aria-label={group}>
                      <h2>{group}</h2>
                      <div className="smart-grid list">{here.map((b) => card(b, counts(b.id)))}</div>
                    </section>
                  )
                );
              })
            : lay.loose.length > 0 && (
                <section className="smart-group" aria-label="Not linked to other blocks">
                  <h2>Not linked to other blocks</h2>
                  <div className="smart-grid">{lay.loose.map((id) => card(byId.get(id)!))}</div>
                </section>
              )}
          <Notes ctx={ctx} gitRef={gitRef} map={map} />
        </div>
        {!framed && details}
      </div>
    </>
  );
}

/** The width of the element `ref` is put on, followed as it resizes; 0 until it is measured. */
function useWidth(): [(el: HTMLDivElement | null) => void, number] {
  const [el, setEl] = useState<HTMLDivElement | null>(null);
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    if (!el) return;
    const measure = () => setWidth(el.clientWidth);
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [el]);
  return [setEl, width];
}

/** `links`: what it needs and what uses it, in words (a phone's list, which draws no line). */
type CardData = { b: Block; status: string | null; chosen: boolean; dim: boolean; onSelect: () => void; links?: string };
type BlockNode = Node<CardData, "block">;
type HolderNode = Node<CardData, "holder">;
type LinkData = { e: Edge; mine: boolean; dim: boolean; tip: string };
type LinkEdge = FlowEdge<LinkData, "link">;

type CanvasProps = {
  lay: Plan;
  byId: Map<string, Block>;
  jobs: Map<string, Job>;
  chosen: string | null;
  near: Set<string>;
  select: (id: string | null) => void;
  /** The width the frame takes. */
  room: number;
  /** The chosen block's details, over the frame's right side. */
  panel: ReactNode;
};

/** The linked blocks on a canvas that pans and zooms, from what needs to what is needed; a block
 *  drawn inside the one holding it. It opens at a zoom the cards read at: the whole graph when it
 *  fits, else its start. */
function Canvas(props: CanvasProps) {
  return (
    <ReactFlowProvider>
      <Flow {...props} />
    </ReactFlowProvider>
  );
}

function Flow({ lay, byId, jobs, chosen, near, select, room, panel }: CanvasProps) {
  const flow = useReactFlow();
  const arrow = useId();
  // the frame's border takes 2px; its height: the graph's, within FRAME_MIN and FRAME_MAX
  const width = room - 2;
  const tallest = Math.max(FRAME_MIN, FRAME_MAX * window.innerHeight);
  const zoom = Math.max(READABLE, Math.min(1, (width - 2 * MARGIN) / lay.width, (tallest - 2 * MARGIN) / lay.height));
  const height = Math.round(Math.max(FRAME_MIN, Math.min(tallest, lay.height * zoom + 2 * MARGIN)));
  const fitsAcross = lay.width * zoom <= width - 2 * MARGIN;
  const fitsDown = lay.height * zoom <= height - 2 * MARGIN;
  // where the canvas opens: read once
  const [start] = useState(() => ({
    zoom,
    x: fitsAcross ? (width - lay.width * zoom) / 2 : MARGIN,
    y: fitsDown ? (height - lay.height * zoom) / 2 : MARGIN,
  }));

  const nodes = useMemo<Node<CardData>[]>(
    () =>
      lay.graph.map((id) => {
        const p = lay.at.get(id)!;
        const b = byId.get(id)!;
        return {
          id,
          type: lay.holders.has(id) ? "holder" : "block",
          position: { x: p.x, y: p.y },
          parentId: p.inside,
          width: p.w,
          height: p.h,
          draggable: false,
          selectable: false,
          focusable: false,
          data: { b, status: blockStatus(b, jobs), chosen: chosen === id, dim: !!chosen && !near.has(id), onSelect: () => select(id) },
        };
      }),
    [lay, byId, jobs, chosen, near, select],
  );
  const edges = useMemo<LinkEdge[]>(
    () =>
      lay.drawn.map((e) => {
        const mine = !!chosen && (e.from === chosen || e.to === chosen);
        return {
          id: `${e.from}>${e.to}:${e.kind}`,
          source: e.from,
          target: e.to,
          type: "link",
          selectable: false,
          focusable: false,
          markerEnd: e.kind === "part" ? undefined : `url(#${arrow}${mine ? "-hl" : ""})`,
          data: { e, mine, dim: !!chosen && !mine, tip: tip(e, byId) },
        };
      }),
    [lay, chosen, byId, arrow],
  );

  // a block chosen out of sight, or under the panel: brought to the middle of what is in view
  useEffect(() => {
    if (!chosen || !lay.at.has(chosen)) return;
    const frame = requestAnimationFrame(() => {
      const n = flow.getInternalNode(chosen);
      if (!n) return;
      // a block holding others: its card, at the top of its frame
      const inset = lay.holders.has(chosen) ? INSET : 0;
      const x = n.internals.positionAbsolute.x + inset;
      const y = n.internals.positionAbsolute.y + inset;
      const vp = flow.getViewport();
      // the panel covers the frame's right side, unless it rises from the bottom (styles.css)
      const free = window.matchMedia("(max-width: 900px)").matches ? width : width - PANEL - INSET;
      const left = x * vp.zoom + vp.x;
      const top = y * vp.zoom + vp.y;
      if (left >= 0 && top >= 0 && left + W * vp.zoom <= free && top + H * vp.zoom <= height) return;
      void flow.setViewport({ x: free / 2 - (x + W / 2) * vp.zoom, y: height / 2 - (y + H / 2) * vp.zoom, zoom: vp.zoom });
    });
    return () => cancelAnimationFrame(frame);
  }, [chosen, lay, flow, width, height]);

  return (
    <div className="smart-frame" style={{ height }}>
      <svg className="smart-defs" aria-hidden>
        <defs>
          {["", "-hl"].map((v) => (
            <marker key={v} id={`${arrow}${v}`} viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
              <path d="M0,1 L9,5 L0,9 z" className={`smart-arrow${v}`} />
            </marker>
          ))}
        </defs>
      </svg>
      <ReactFlow
        nodes={nodes}
        edges={edges}
        nodeTypes={NODE_TYPES}
        edgeTypes={EDGE_TYPES}
        defaultViewport={start}
        minZoom={0.2}
        maxZoom={1.5}
        zIndexMode="auto"
        nodesDraggable={false}
        nodesConnectable={false}
        nodesFocusable={false}
        edgesFocusable={false}
        elementsSelectable={false}
        // the page scrolls under the wheel; Ctrl (⌘) and the wheel, a pinch or the buttons zoom
        zoomOnScroll={false}
        preventScrolling={false}
        zoomOnDoubleClick={false}
        panActivationKeyCode={null}
        selectionKeyCode={null}
        multiSelectionKeyCode={null}
        deleteKeyCode={null}
        onPaneClick={() => select(null)}
        // a click on a holder's frame, around what it holds (its card takes its own)
        onNodeClick={(_, n) => select(n.id)}
      >
        <Background variant={BackgroundVariant.Dots} gap={20} size={1} />
        <Controls showInteractive={false} position="bottom-left" />
        {/* beside the buttons: a graph from left to right is crowded on the right, not there */}
        {!(fitsAcross && fitsDown) && (
          <MiniMap pannable zoomable position="bottom-left" style={{ width: 120, height: 76, marginLeft: 58 }} ariaLabel="Where the view is in the whole graph" />
        )}
      </ReactFlow>
      {panel}
    </div>
  );
}

function BlockNodeView({ data }: NodeProps<BlockNode>) {
  return (
    <>
      <Handle type="target" position={Position.Left} isConnectable={false} className="smart-handle" />
      <BlockCard {...data} />
      <Handle type="source" position={Position.Right} isConnectable={false} className="smart-handle" />
    </>
  );
}

/** A block holding others: its card at the top of a frame, what it holds inside. */
function HolderNodeView({ data, width, height }: NodeProps<HolderNode>) {
  const at = { top: INSET + H / 2 };
  return (
    <div className={`smart-holder${data.dim ? " dim" : ""}`} style={{ width, height }}>
      <Handle type="target" position={Position.Left} isConnectable={false} className="smart-handle" style={at} />
      <BlockCard {...data} />
      <Handle type="source" position={Position.Right} isConnectable={false} className="smart-handle" style={at} />
    </div>
  );
}

/** A link: what it says on hover; on the chosen block's, its verb. */
function LinkEdgeView({ id, sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition, markerEnd, data }: EdgeProps<LinkEdge>) {
  const [d, labelX, labelY] = getBezierPath({ sourceX, sourceY, sourcePosition, targetX, targetY, targetPosition });
  if (!data) return null;
  const { e, mine, dim } = data;
  const cls = [e.kind === "part" ? "part" : e.inferred ? "inferred" : "declared", mine ? "hl" : dim ? "dim" : ""].join(" ");
  return (
    <>
      <title>{data.tip}</title>
      <BaseEdge id={id} path={d} markerEnd={markerEnd} className={`smart-link ${cls}`} interactionWidth={16} />
      {mine && e.kind !== "part" && (
        <EdgeLabelRenderer>
          <div className="smart-link-label" style={{ transform: `translate(-50%, -50%) translate(${labelX}px, ${labelY}px)` }}>
            {VERB[e.kind]}
          </div>
        </EdgeLabelRenderer>
      )}
    </>
  );
}

const NODE_TYPES = { block: BlockNodeView, holder: HolderNodeView };
const EDGE_TYPES = { link: LinkEdgeView };

/** What a link says, in a sentence. */
function tip(e: Edge, byId: Map<string, Block>) {
  const name = (id: string) => (byId.has(id) ? title(byId.get(id)!) : id);
  if (e.kind === "part") return `${name(e.to)} is part of ${name(e.from)}`;
  return `${name(e.from)} ${VERB[e.kind]} ${name(e.to)}${e.via ? ` · ${e.via}` : ""}${e.inferred ? " (a guess)" : ""}`;
}

function BlockCard({ b, status, chosen, dim, onSelect, links }: CardData) {
  const [kind, color] = KIND[b.kind];
  const lang = b.languages[0];
  return (
    <button
      type="button"
      className={`smart-card${chosen ? " selected" : ""}${dim ? " dim" : ""}`}
      style={{ "--k": color } as CSSProperties}
      data-id={b.id}
      aria-pressed={chosen}
      onClick={(e) => {
        e.stopPropagation();
        onSelect();
      }}
    >
      <span className="smart-kind">
        <i className="dot" />
        <span>{`${kind}${b.role ? ` · ${ROLE[b.role]}` : ""}`}</span>
        {(solo(b) || status) && (
          <span className="smart-flags">
            {solo(b) && (
              <span className="smart-solo" title={SOLO}>
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
      {/* what it is for, when its manifest says: its place is in the details */}
      {b.description ? (
        <span className="smart-desc" title={`${b.description}\n${place(b)}`}>
          {b.description}
        </span>
      ) : (
        <span className="smart-place">{place(b)}</span>
      )}
      {links && <span className="smart-links">{links}</span>}
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

function Details({ ctx, gitRef, map, lay, b, jobs, select }: { ctx: Ctx; gitRef: string; map: S["RepoMap"]; lay: Plan; b: Block; jobs: Map<string, Job>; select: (id: string | null) => void }) {
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
          {b.role && ` · ${ROLE[b.role]}`}
          {b.configured && " · from assay.toml"}
        </div>
        <button type="button" className="icon-btn" aria-label="Close" title="Close (Esc)" onClick={() => select(null)}>
          <X size={14} />
        </button>
      </header>
      <div>
        <h2>{title(b)}</h2>
        {b.description && <p className="smart-about">{b.description}</p>}
        {b.roots.map((r) => (
          <Link key={r.path} className="smart-root" to={at(r)}>
            {where(r.path)}
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
          {solo(b) && <p className="smart-note">{SOLO}: worth sharing what they know.</p>}
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
      <EdgeList title="Contains">
        {holds.map((e) => (
          <li key={e.to}>
            <span className="grow">{go(e.to)}</span>
          </li>
        ))}
      </EdgeList>
      <EdgeList title="Used by">
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
        <details className="smart-unassigned">
          <summary>{plural(map.unassigned, "file is", "files are")} in no block</summary>
          <p>
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
        </details>
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

/** Where a block of the graph goes, inside the block holding it if any (`inside`), and its size. */
type Placed = { x: number; y: number; w: number; h: number; inside?: string };

type Plan = {
  /** What the blocks and their links are, for the canvas to open again on a new one. */
  id: string;
  /** The links, and each block's parent holding it (`part`): for the details and what is near. */
  edges: Edge[];
  /** What the canvas draws as lines: the links, and the `part`s of the whole repository (a block
   *  drawn inside the one holding it needs no line). */
  drawn: Edge[];
  /** The blocks of the graph, each after the one holding it; `order`: column by column, top to
   *  bottom (a list's); `loose`: those linked to none. */
  graph: string[];
  order: string[];
  loose: string[];
  /** The blocks holding others, drawn as frames around them. */
  holders: Set<string>;
  at: Map<string, Placed>;
  width: number;
  height: number;
};

const elk = new ELK();

/** In layers from left to right, what needs the rest first; a block holding others laid out with
 *  what it holds, its card on top. */
const LAYOUT = {
  "elk.algorithm": "layered",
  "elk.direction": "RIGHT",
  "elk.hierarchyHandling": "INCLUDE_CHILDREN",
  "elk.layered.spacing.nodeNodeBetweenLayers": "56",
  "elk.spacing.nodeNode": "16",
  "elk.layered.nodePlacement.strategy": "NETWORK_SIMPLEX",
  "elk.layered.considerModelOrder.strategy": "NODES_AND_EDGES",
  "elk.padding": "[top=0,left=0,bottom=0,right=0]",
};

/** Where the blocks go. A block sits inside its parent (a crate's schema, an app's tool), unless
 *  a link joins them already (an image built from the crate it lies in), or the parent is the
 *  whole repository: then a dotted line says it. */
async function plan(map: S["RepoMap"]): Promise<Plan> {
  const byId = new Map(map.blocks.map((b) => [b.id, b]));
  const links: Edge[] = map.links.filter((l) => byId.has(l.from) && byId.has(l.to)).map((l) => ({ from: l.from, to: l.to, kind: l.kind, inferred: l.inferred, via: l.via }));
  const linked = (a: string, b: string) => links.some((e) => (e.from === a && e.to === b) || (e.from === b && e.to === a));
  const inside = new Map<string, string>();
  const lines: Edge[] = [];
  for (const b of map.blocks) {
    if (!b.parent || !byId.has(b.parent) || linked(b.parent, b.id)) continue;
    const part: Edge = { from: b.parent, to: b.id, kind: "part", inferred: false, via: "" };
    if (byId.get(b.parent)!.roots.some((r) => r.path === ".")) lines.push(part);
    else inside.set(b.id, b.parent);
  }
  const edges = [...links, ...lines, ...[...inside].map(([child, parent]): Edge => ({ from: parent, to: child, kind: "part", inferred: false, via: "" }))];
  const drawn = [...links, ...lines];
  const inGraph = new Set(edges.flatMap((e) => [e.from, e.to]));
  const loose = map.blocks.filter((b) => !inGraph.has(b.id)).map((b) => b.id);
  const holders = new Set(inside.values());
  const id = JSON.stringify([[...inGraph].sort(), drawn.map((e) => [e.from, e.to, e.kind]), [...inside]]);
  const empty: Plan = { id, edges, drawn, graph: [], order: [], loose, holders, at: new Map(), width: 0, height: 0 };
  if (!inGraph.size) return empty;

  // the blocks in ELK's tree: each inside the one holding it
  const held = new Map<string, string[]>();
  const top: string[] = [];
  for (const b of map.blocks) {
    if (!inGraph.has(b.id)) continue;
    const p = inside.get(b.id);
    if (p) held.set(p, [...(held.get(p) ?? []), b.id]);
    else top.push(b.id);
  }
  const node = (id: string): ElkNode => {
    const kids = held.get(id);
    if (!kids) return { id, width: W, height: H };
    return {
      id,
      children: kids.map(node),
      layoutOptions: {
        "elk.padding": `[top=${H + 2 * INSET},left=${INSET},bottom=${INSET},right=${INSET}]`,
        "elk.nodeSize.constraints": "MINIMUM_SIZE",
        "elk.nodeSize.minimum": `(${W + 2 * INSET},${H + 2 * INSET})`,
      },
    };
  };
  const root: ElkNode = { id: "root", layoutOptions: LAYOUT, children: top.map(node), edges: [] };
  // a line goes in the deepest block holding both of its ends (else the root), as ELK wants it
  const nodes = new Map<string, ElkNode>();
  const index = (n: ElkNode) => {
    nodes.set(n.id, n);
    n.children?.forEach(index);
  };
  index(root);
  const holding = (id: string) => {
    const out: string[] = [];
    for (let p = inside.get(id); p; p = inside.get(p)) out.push(p);
    return out;
  };
  drawn.forEach((e, i) => {
    const around = new Set(holding(e.from));
    const home = holding(e.to).find((p) => around.has(p)) ?? "root";
    const line: ElkExtendedEdge = { id: `e${i}`, sources: [e.from], targets: [e.to] };
    const n = nodes.get(home)!;
    n.edges = [...(n.edges ?? []), line];
  });
  const done = await elk.layout(root);

  const at = new Map<string, Placed>();
  const graph: string[] = [];
  const walk = (n: ElkNode, holder?: string) => {
    for (const c of n.children ?? []) {
      at.set(c.id, { x: c.x ?? 0, y: c.y ?? 0, w: c.width ?? W, h: c.height ?? H, inside: holder });
      graph.push(c.id);
      walk(c, c.id);
    }
  };
  walk(done);
  const abs = (id: string): { x: number; y: number } => {
    const p = at.get(id)!;
    if (!p.inside) return { x: p.x, y: p.y };
    const o = abs(p.inside);
    return { x: o.x + p.x, y: o.y + p.y };
  };
  const order = [...graph].sort((a, b) => abs(a).x - abs(b).x || abs(a).y - abs(b).y);
  return { ...empty, graph, order, at, width: done.width ?? 0, height: done.height ?? 0 };
}
