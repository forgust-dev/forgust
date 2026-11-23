import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, ReactNode, Ref } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import {
  AlertTriangle,
  AppWindow,
  ArrowLeft,
  ArrowRight,
  BookOpen,
  Box,
  Boxes,
  Container,
  Database,
  DatabaseZap,
  FileCode,
  FileText,
  GitPullRequest,
  HardDrive,
  Hourglass,
  Library,
  ListEnd,
  Mail,
  Pencil,
  Search,
  Server,
  SquareTerminal,
  Table2,
  Users,
  Workflow,
  X,
  Zap,
  type LucideIcon,
} from "lucide-react";
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
import { Select } from "../../components/controls";
import { useSession } from "../../session";
import type { AssayEdit } from "./assayToml";
import { blockTitle } from "./blocks";
import { StatusIcon, duration, statusLabel } from "./ci/Status";
import { LOGOS, type Brand, type Logo } from "./logos";
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
/** The frame's bottom-left corner, which the zoom buttons take (React Flow's 15px margin, then
 *  30 by 86px: styles.css), and the minimap beside them (122px wide, 58px from the corner), with
 *  a few pixels around: where the canvas opens, no block is under them. */
const CORNER = { w: 15 + 30 + 8, h: 15 + 86 + 8 };
const CORNER_MAP = { w: 15 + 58 + 122 + 8, h: 15 + 86 + 8 };
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
  helm: ["Helm chart", "#3a4bb0"],
  kubernetes: ["Kubernetes", "#326ce5"],
  ansible: ["Ansible", "#c43c3c"],
  terraform: ["Terraform module", "#7b4fba"],
  pulumi: ["Pulumi project", "#9b3c94"],
  ci: ["CI", "#b8901a"],
  docs: ["Docs", "#8a8178"],
  other: ["Block", "#8a8178"],
};
/** What a block is, as its card says it: its kind, or what its manifest makes it (a role, a
 *  collection of roles). */
function kindName(b: Block): string {
  if (b.kind === "ansible") {
    if (b.manifests.some((m) => /(^|\/)galaxy\.yml$/.test(m.path))) return "Ansible collection";
    if (b.roots.some((r) => /(^|\/)roles\/[^/]+$/.test(r.path))) return "Ansible role";
  }
  return KIND[b.kind][0];
}
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
/** A service whose image has no logo here: what it does. */
const ROLE_ICON: Record<S["MapRole"], LucideIcon> = {
  web: AppWindow,
  api: Server,
  cli: SquareTerminal,
  library: Library,
  database: Database,
  cache: DatabaseZap,
  queue: ListEnd,
  search: Search,
  storage: HardDrive,
  mail: Mail,
};
/** The logo of a service's image, by the image's name: without its registry, owner, tag or digest. */
const IMAGE_LOGO = new Map<string, Brand>([
  ["postgres", "postgresql"],
  ["postgresql", "postgresql"],
  ["mysql", "mysql"],
  ["mariadb", "mariadb"],
  ["mongo", "mongodb"],
  ["mongodb", "mongodb"],
  ["redis", "redis"],
  ["redis-stack", "redis"],
  ["redis-stack-server", "redis"],
  ["clickhouse", "clickhouse"],
  ["clickhouse-server", "clickhouse"],
  ["rabbitmq", "rabbitmq"],
  ["kafka", "kafka"],
  ["cp-kafka", "kafka"],
  ["nats", "nats"],
  ["elasticsearch", "elasticsearch"],
  ["opensearch", "opensearch"],
  ["meilisearch", "meilisearch"],
  ["minio", "minio"],
  ["nginx", "nginx"],
  ["nginx-unprivileged", "nginx"],
  ["traefik", "traefik"],
  ["caddy", "caddy"],
  ["gitlab-runner", "gitlab"],
  ["gitlab-ce", "gitlab"],
  ["gitlab-ee", "gitlab"],
  ["grafana", "grafana"],
  ["prometheus", "prometheus"],
  ["node", "node"],
  ["python", "python"],
  ["golang", "go"],
  ["rust", "rust"],
]);
/** What stands for a block: its technology's logo when it is known for sure (a crate's, an image's,
 *  a service running a known image), else an icon of what it is. */
function mark(b: Block): { logo: Logo } | { icon: LucideIcon } {
  switch (b.kind) {
    case "rust":
    case "python":
    case "go":
    case "docker":
    case "helm":
    case "kubernetes":
    case "ansible":
    case "terraform":
    case "pulumi":
      return { logo: LOGOS[b.kind] };
    case "node": {
      const code = b.languages[0]?.name;
      return { logo: code === "TypeScript" ? LOGOS.typescript : code === "JavaScript" ? LOGOS.javascript : LOGOS.npm };
    }
    case "service": {
      const known = b.image ? IMAGE_LOGO.get(b.image.replace(/@.*$/, "").split("/").pop()!.replace(/:.*$/, "").toLowerCase()) : undefined;
      if (known) return { logo: LOGOS[known] };
      return { icon: b.role ? ROLE_ICON[b.role] : Container };
    }
    case "migrations":
      return { icon: Table2 };
    case "ci":
      return { icon: Workflow };
    case "docs":
      return { icon: BookOpen };
    default:
      return { icon: Box };
  }
}
/** A logo too dark for the dark theme's cards, or too pale for the light theme's: drawn in, or
 *  towards, the text's colour (styles.css). By WCAG's relative luminance. */
function tone(hex: string) {
  const [r, g, b] = [1, 3, 5].map((i) => {
    const c = parseInt(hex.slice(i, i + 2), 16) / 255;
    return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  const lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
  return lum < 0.05 ? " dark" : lum > 0.5 ? " pale" : "";
}
/** A phone's list, by what the blocks are: what runs first, the data last. */
const GROUPS: [title: string, kinds: S["MapKind"][]][] = [
  ["Services", ["service"]],
  ["Images", ["docker"]],
  ["Infrastructure", ["helm", "kubernetes", "ansible", "terraform", "pulumi"]],
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
const title = blockTitle;
const where = (path: string) => (path === "." ? "whole repository" : path);
const place = (b: Block) =>
  b.roots.length
    ? where(b.roots[0].path) + (b.roots.length > 1 ? ` +${b.roots.length - 1}` : "")
    : (b.image?.replace(/@sha256:[0-9a-f]+$/, "") ?? b.manifests.map((m) => m.path).join(", "));
const plural = (n: number, one: string, many = `${one}s`) => `${n.toLocaleString()} ${n === 1 ? one : many}`;

/** The project's code as blocks (what is built, run or deployed apart), what each needs, and
 *  the people to ask: the latest map a job made of the default branch, or, from a merge
 *  request (`?mr=`), of its head once it has one. */
export function SmartView({ ctx }: { ctx: Ctx }) {
  const [params, setParams] = useSearchParams();
  const mr = params.get("mr");
  const of = mr && /^\d+$/.test(mr) ? mr : undefined;
  // the map, and where its blocks go: laid out with it, so that the page comes in at once
  const q = useQuery({
    queryKey: ["map", ctx.p.id, of ?? ""],
    queryFn: async () => {
      const view = await api.get<S["SmartView"]>(`${ctx.base}/map${qs({ merge_request: of })}`);
      return { view, map: view.map ?? null, lay: view.map ? await plan(view.map) : null };
    },
    staleTime: 30_000,
    // a commit is being mapped: its map comes in when the job is done
    refetchInterval: (query) => (query.state.data?.view.run?.state === "pending" ? 4000 : false),
  });
  const [words, setWords] = useState("");
  // the blocks the words find, in the order the graph shows them
  const found = useMemo(() => (q.data?.map && q.data.lay && words.trim() ? findBlocks(q.data.map, q.data.lay, words) : null), [q.data, words]);
  // Enter: the next block found is chosen, and brought into view
  const next = () => {
    if (!found?.length) return;
    const at = found.indexOf(params.get("block") ?? "");
    const id = found[(at + 1) % found.length];
    setParams(
      (now) => {
        const n = new URLSearchParams(now);
        n.set("block", id);
        return n;
      },
      { replace: true },
    );
  };
  return (
    <div className="stack smart-view">
      <ListHead title="Smart View" sub="The parts of this code, what each needs, and who to ask about them.">
        {!!q.data?.map?.blocks.length && <FindBlock value={words} onChange={setWords} found={found?.length ?? 0} onNext={next} />}
      </ListHead>
      <Q q={q}>
        {({ view, map, lay }) => (
          <>
            <RunNote ctx={ctx} view={view} />
            {map && lay && <MapView ctx={ctx} gitRef={map.ref} map={map} lay={lay} found={found} words={words} />}
          </>
        )}
      </Q>
    </div>
  );
}

/** What stands between the page and an up-to-date map: the Smart View is off, a commit is being
 *  mapped, or the last one could not be. Nothing when the map shown is the latest. */
function RunNote({ ctx, view }: { ctx: Ctx; view: S["SmartView"] }) {
  if (!view.enabled) {
    return (
      <Empty>
        The Smart View is off for this project. Turned on, a job maps the code at each new commit of the default branch and of open merge requests.{" "}
        {ctx.p.can_admin ? <Link to={`${ctx.web}/-/settings#smart-view`}>Turn it on in the settings</Link> : "A Maintainer turns it on in the project's settings."}
      </Empty>
    );
  }
  const run = view.run;
  const job = run?.pipeline_iid ? <Link to={`${ctx.web}/-/pipelines/${run.pipeline_iid}`}>pipeline #{run.pipeline_iid}</Link> : "a pipeline";
  if (!run) {
    return view.map ? null : <Empty>No map yet: the default branch is mapped at its next commit.</Empty>;
  }
  const commit = <code>{run.commit.slice(0, 8)}</code>;
  if (run.state === "pending") {
    return (
      <div className="banner" role="status">
        <span className="grow small">
          {view.map ? <>A newer commit ({commit}) is being mapped by {job}: this is the map of the one before.</> : <>The code is being mapped by {job}: the map shows here when it is done.</>}
        </span>
      </div>
    );
  }
  return (
    <div className="banner danger" role="alert">
      <AlertTriangle size={16} />
      <span className="grow small">
        The commit {commit} could not be mapped ({job}){run.error ? <>: {run.error}</> : "."}
        {view.map && " This is the map of an earlier one."}
      </span>
    </div>
  );
}

/** Words that find blocks by their name, what they are for, where they are or what they are. */
function FindBlock({ value, onChange, found, onNext }: { value: string; onChange: (v: string) => void; found: number; onNext: () => void }) {
  return (
    <label className="search-field smart-find">
      <Search size={14} aria-hidden />
      <input
        type="search"
        value={value}
        placeholder="Find a block"
        aria-label="Find a block"
        aria-description="Enter chooses the next one"
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            onNext();
          } else if (e.key === "Escape" && value) {
            // the words go, not the block chosen
            e.preventDefault();
            onChange("");
          }
        }}
      />
      {value.trim() && (
        <span className="smart-find-count" aria-live="polite">
          {found ? plural(found, "block") : "none"}
        </span>
      )}
    </label>
  );
}

/** The blocks with all of `words` in their id, name, description, roots, image or kind: those
 *  named so first, then those for or at it, then those that are it; each in the graph's order
 *  (left to right, top to bottom, then those linked to none). */
function findBlocks(map: S["RepoMap"], lay: Plan, words: string): string[] {
  const want = words.toLowerCase().split(/\s+/).filter(Boolean);
  const rank = new Map([...lay.order, ...lay.loose].map((id, i) => [id, i]));
  const has = (parts: string[]) => {
    const text = parts.join(" ").toLowerCase();
    return want.every((w) => text.includes(w));
  };
  const score = (b: Block) => {
    const name = [b.id, b.name];
    const about = [...name, b.description ?? "", ...b.roots.map((r) => r.path), b.image ?? ""];
    return has(name) ? 0 : has(about) ? 1 : has([...about, kindName(b), b.role ? ROLE[b.role] : ""]) ? 2 : -1;
  };
  return map.blocks
    .map((b) => ({ id: b.id, score: score(b) }))
    .filter((x) => x.score >= 0)
    .sort((a, b) => a.score - b.score || (rank.get(a.id) ?? 0) - (rank.get(b.id) ?? 0))
    .map((x) => x.id);
}

/** What depends on `ids`, directly or through others, nearest first, each with the block of the
 *  chain it needs (none when it needs one of `ids` itself). A block holding another changes with
 *  it: a crate with its schema. */
function dependents(edges: Edge[], ids: string[]): Map<string, string | null> {
  const users = new Map<string, string[]>();
  for (const e of edges) users.set(e.to, [...(users.get(e.to) ?? []), e.from]);
  const out = new Map<string, string | null>();
  const start = new Set(ids);
  let wave = ids;
  while (wave.length) {
    const next: string[] = [];
    for (const id of wave) {
      for (const u of users.get(id) ?? []) {
        if (start.has(u) || out.has(u)) continue;
        out.set(u, start.has(id) ? null : id);
        next.push(u);
      }
    }
    wave = next;
  }
  return out;
}

function MapView({ ctx, gitRef, map, lay, found, words }: { ctx: Ctx; gitRef: string; map: S["RepoMap"]; lay: Plan; found: string[] | null; words: string }) {
  const [params, setParams] = useSearchParams();
  const byId = useMemo(() => new Map(map.blocks.map((b) => [b.id, b])), [map]);
  const { jobs, pipeline } = useJobs(ctx, map.commit);
  const [main, room] = useWidth();
  const list = room > 0 && room < LIST_BELOW;
  // the graph on its canvas, once there is room for it
  const framed = room >= LIST_BELOW && lay.graph.length > 0;
  const asked = params.get("block");
  const chosen = asked && byId.has(asked) ? asked : null;
  // the chain: everything that depends on the chosen block lights up, not only what is near it
  const chain = params.get("impact") === "1";
  const setParam = useCallback(
    (key: string, value: string | null) =>
      setParams(
        (now) => {
          const next = new URLSearchParams(now);
          if (value) next.set(key, value);
          else next.delete(key);
          return next;
        },
        { replace: true },
      ),
    [setParams],
  );
  const select = useCallback((id: string | null) => setParam("block", id), [setParam]);
  const setChain = useCallback((on: boolean) => setParam("impact", on ? "1" : null), [setParam]);
  useEffect(() => {
    if (!chosen) return;
    const close = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !e.defaultPrevented) select(null);
    };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [chosen, select]);
  // the blocks a merge request changes (`?mr=`), from its page
  const mr = params.get("mr");
  const touched = useQuery({
    queryKey: ["mr", ctx.p.id, Number(mr), "blocks", "map"],
    queryFn: () => api.get<S["MrBlocks"]>(`/projects/${ctx.p.id}/merge_requests/${mr}/blocks`),
    enabled: !!mr && /^\d+$/.test(mr),
    staleTime: 60_000,
  });
  const changed = useMemo(() => (touched.data ? touched.data.blocks.map((t) => t.block.id).filter((id) => byId.has(id)) : null), [touched.data, byId]);
  // what stays bright, the rest stepping back: the chosen block and what is near it (or, asked,
  // all that depends on it); else the blocks found; else those the merge request changes (and,
  // asked, what depends on them)
  const lit = useMemo(() => {
    if (!chosen) {
      if (found) return new Set(found);
      if (!changed) return null;
      const s = new Set(changed);
      if (chain) for (const id of dependents(lay.edges, changed).keys()) s.add(id);
      return s;
    }
    const s = new Set([chosen]);
    if (chain) for (const id of dependents(lay.edges, [chosen]).keys()) s.add(id);
    else
      for (const e of lay.edges) {
        if (e.from === chosen) s.add(e.to);
        if (e.to === chosen) s.add(e.from);
      }
    return s;
  }, [chosen, chain, found, changed, lay]);
  // the lines between the bright blocks light up too: a chain's, a merge request's
  const joined = (!!chosen && chain) || (!chosen && !found && !!changed);
  // what the canvas brings into view: the chain, or what the merge request changes
  const fit = useMemo(() => (lit && joined ? [...lit].filter((id) => lay.at.has(id)) : []), [lit, joined, lay]);
  // corrections, written into assay.toml in the editor: its own, or a new one
  const nav = useNavigate();
  const { user } = useSession();
  const fix = useCallback(
    (edit: AssayEdit) => nav(map.config_file ? refLink(ctx.web, "edit", gitRef, "assay.toml") : refLink(ctx.web, "new", gitRef), { state: { assay: edit } }),
    [nav, map.config_file, ctx.web, gitRef],
  );
  // a block whose jobs failed holds up what depends on it, as a pipeline's failed stage would:
  // each such block, and the failed one it waits on
  const waiting = useMemo(() => {
    const failed = map.blocks.filter((b) => blockStatus(b, jobs) === "failed").map((b) => b.id);
    const out = new Map<string, string>();
    for (const f of failed) {
      for (const id of dependents(lay.edges, [f]).keys()) if (!out.has(id) && !failed.includes(id)) out.set(id, f);
    }
    return out;
  }, [map, jobs, lay]);
  const waitsOn = useCallback(
    (id: string) => {
      const f = waiting.get(id);
      return f ? title(byId.get(f)!) : undefined;
    },
    [waiting, byId],
  );

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
        No blocks found. They come from manifests: <code>Cargo.toml</code>, <code>package.json</code>, <code>pyproject.toml</code>, <code>go.mod</code>, a <code>Dockerfile</code>, a compose file, a Helm chart, Kubernetes manifests,
        Ansible roles, Terraform modules, a Pulumi project or a CI configuration. An <code>assay.toml</code> at the top of the repository can declare others.
      </Empty>
    );
  }
  const card = (b: Block, links?: string) => (
    <BlockCard
      key={b.id}
      b={b}
      status={blockStatus(b, jobs)}
      waits={waitsOn(b.id)}
      chosen={chosen === b.id}
      dim={!!lit && !lit.has(b.id)}
      onSelect={() => select(b.id)}
      links={links}
    />
  );
  // a block chosen from another's details opens at its top
  const details = chosen ? (
    <Details
      key={chosen}
      ctx={ctx}
      map={map}
      lay={lay}
      b={byId.get(chosen)!}
      jobs={jobs}
      pipeline={pipeline}
      waits={waiting.get(chosen)}
      chain={chain}
      setChain={setChain}
      select={select}
      gitRef={gitRef}
      me={user?.username}
      fix={ctx.p.can_write_repository ? fix : undefined}
    />
  ) : null;
  // a phone's list keeps only the blocks found
  const shown = (b: Block) => !list || !found || found.includes(b.id);
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
      {mr && touched.data && (
        <div className="smart-mr" role="status">
          <GitPullRequest size={15} aria-hidden />
          <span className="grow">
            <Link to={`${ctx.web}/-/merge_requests/${mr}`}>!{mr}</Link> changes {plural(touched.data.blocks.length, "block")}
            {touched.data.unassigned > 0 && ` and ${plural(touched.data.unassigned, "file")} in none`}
            {touched.data.blocks.length > 0 && ": "}
            {touched.data.blocks.slice(0, 8).map((t, i) => (
              <span key={t.block.id}>
                {i > 0 && ", "}
                <button type="button" className="smart-go" onClick={() => select(t.block.id)}>
                  {title(t.block)}
                </button>
              </span>
            ))}
            {touched.data.blocks.length > 8 && `, and ${touched.data.blocks.length - 8} more`}
          </span>
          {touched.data.blocks.length > 0 && (
            <button type="button" className={`btn small${chain ? " on" : ""}`} aria-pressed={chain} onClick={() => setChain(!chain)}>
              With what depends on them
            </button>
          )}
          <button type="button" className="icon-btn" aria-label="Show every block alike" title="Show every block alike" onClick={() => setParam("mr", null)}>
            <X size={14} />
          </button>
        </div>
      )}
      {/* the details: over the canvas when there is one, else beside the blocks */}
      <div className={`smart-layout${details && room > 0 && !framed ? " open" : ""}`}>
        <div className="smart-main" ref={main}>
          {framed && (
            <Canvas
              key={lay.id}
              lay={lay}
              byId={byId}
              jobs={jobs}
              waitsOn={waitsOn}
              chosen={chosen}
              joined={joined}
              lit={lit}
              fit={fit}
              select={select}
              room={room}
              panel={details}
            />
          )}
          {list && found?.length === 0 && <Empty>No block has “{words.trim()}” in its name, what it is for or where it is.</Empty>}
          {list
            ? GROUPS.map(([group, kinds]) => {
                const here = map.blocks.filter((b) => kinds.includes(b.kind) && shown(b)).sort((a, b) => rank.get(a.id)! - rank.get(b.id)!);
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

/** `links`: what it needs and what uses it, in words (a phone's list, which draws no line);
 *  `waits`: the block whose failed jobs hold it up. */
type CardData = { b: Block; status: string | null; waits?: string; chosen: boolean; dim: boolean; onSelect: () => void; links?: string };
type BlockNode = Node<CardData, "block">;
type HolderNode = Node<CardData, "holder">;
/** `mine`: a line lit with the chosen block (`said`: one of its own, which says its verb). */
type LinkData = { e: Edge; mine: boolean; said: boolean; dim: boolean; tip: string };
type LinkEdge = FlowEdge<LinkData, "link">;

type CanvasProps = {
  lay: Plan;
  byId: Map<string, Block>;
  jobs: Map<string, Job>;
  waitsOn: (id: string) => string | undefined;
  chosen: string | null;
  /** The lines between bright blocks light up too: a chain's, a merge request's. */
  joined: boolean;
  /** What stays bright; none: all of it. */
  lit: Set<string> | null;
  /** The blocks brought into view, zooming out if need be. */
  fit: string[];
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

function Flow({ lay, byId, jobs, waitsOn, chosen, joined, lit, fit: shown, select, room, panel }: CanvasProps) {
  const flow = useReactFlow();
  const arrow = useId();
  // the frame's border takes 2px; its height: the graph's, within FRAME_MIN and FRAME_MAX
  const width = room - 2;
  const tallest = Math.max(FRAME_MIN, FRAME_MAX * window.innerHeight);
  const zoom = Math.max(READABLE, Math.min(1, (width - 2 * MARGIN) / lay.width, (tallest - 2 * MARGIN) / lay.height));
  const fit = Math.round(Math.max(FRAME_MIN, Math.min(tallest, lay.height * zoom + 2 * MARGIN)));
  const fitsAcross = lay.width * zoom <= width - 2 * MARGIN;
  const fitsDown = lay.height * zoom <= fit - 2 * MARGIN;
  // the minimap shows once the graph overflows the frame, beside the zoom buttons
  const corner = fitsAcross && fitsDown ? CORNER : CORNER_MAP;
  // where the canvas opens, read once. A block the corner's buttons would cover moves the graph
  // right of them; a graph that fits across without room for that gets the frame taller instead,
  // the buttons below it
  const [open] = useState(() => {
    const at = { zoom, x: fitsAcross ? (width - lay.width * zoom) / 2 : MARGIN, y: fitsDown ? (fit - lay.height * zoom) / 2 : MARGIN };
    const shift = cornerShift(lay, at, fit, corner);
    if (shift && fitsAcross && fitsDown && at.x + shift + lay.width * zoom > width - MARGIN) return { start: at, below: corner.h };
    return { start: { ...at, x: at.x + shift }, below: 0 };
  });
  const height = fit + open.below;

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
          data: { b, status: blockStatus(b, jobs), waits: waitsOn(id), chosen: chosen === id, dim: !!lit && !lit.has(id), onSelect: () => select(id) },
        };
      }),
    [lay, byId, jobs, waitsOn, chosen, lit, select],
  );
  const edges = useMemo<LinkEdge[]>(
    () =>
      lay.drawn.map((e) => {
        const said = !!chosen && (e.from === chosen || e.to === chosen);
        // a chain's, a merge request's: a line between two bright blocks
        const mine = said || (joined && !!lit?.has(e.from) && !!lit?.has(e.to));
        return {
          id: `${e.from}>${e.to}:${e.kind}`,
          source: e.from,
          target: e.to,
          type: "link",
          selectable: false,
          focusable: false,
          markerEnd: e.kind === "part" ? undefined : `url(#${arrow}${mine ? "-hl" : ""})`,
          data: { e, mine, said, dim: !!lit && !mine, tip: tip(e, byId) },
        };
      }),
    [lay, chosen, joined, lit, byId, arrow],
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
  // a chain, or what a merge request changes: all of it brought into view (beside the panel when
  // it is open), zooming out if it must; read again only when the blocks do
  const fitKey = shown.join(" ");
  const panelOpen = !!panel;
  useEffect(() => {
    if (!fitKey) return;
    const frame = requestAnimationFrame(() => {
      const side = panelOpen && !window.matchMedia("(max-width: 900px)").matches ? PANEL + INSET : 0;
      void flow.fitView({
        nodes: fitKey.split(" ").map((id) => ({ id })),
        padding: { top: `${MARGIN}px`, right: `${side + MARGIN}px`, bottom: `${corner.h}px`, left: `${MARGIN}px` },
        maxZoom: flow.getViewport().zoom,
        minZoom: 0.4,
      });
    });
    return () => cancelAnimationFrame(frame);
  }, [fitKey, panelOpen, flow, corner]);

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
        defaultViewport={open.start}
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
        {/* the whole graph, above the corner's buttons */}
        <Controls
          showInteractive={false}
          position="bottom-left"
          fitViewOptions={{ padding: { top: `${MARGIN}px`, right: `${MARGIN}px`, bottom: `${corner.h}px`, left: `${MARGIN}px` } }}
        />
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
  const { e, mine, said, dim } = data;
  const cls = [e.kind === "part" ? "part" : e.inferred ? "inferred" : "declared", mine ? "hl" : dim ? "dim" : ""].join(" ");
  return (
    <>
      <title>{data.tip}</title>
      <BaseEdge id={id} path={d} markerEnd={markerEnd} className={`smart-link ${cls}`} interactionWidth={16} />
      {said && e.kind !== "part" && (
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

/** A block's logo, its name on hover, or the icon of what it is, in its kind's colour (`mark`). */
function Mark({ b }: { b: Block }) {
  const m = mark(b);
  if ("icon" in m) return <m.icon className="smart-mark" aria-hidden />;
  return (
    <svg className={`smart-mark logo${tone(m.logo.hex)}`} viewBox="0 0 24 24" style={{ "--logo": m.logo.hex } as CSSProperties} aria-hidden>
      <title>{m.logo.title}</title>
      <path d={m.logo.path} />
    </svg>
  );
}

function BlockCard({ b, status, waits, chosen, dim, onSelect, links }: CardData) {
  const [kind, color] = [kindName(b), KIND[b.kind][1]];
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
        <Mark b={b} />
        <span>{`${kind}${b.role ? ` · ${ROLE[b.role]}` : ""}`}</span>
        {(solo(b) || status || waits) && (
          <span className="smart-flags">
            {solo(b) && (
              <span className="smart-solo" title={SOLO}>
                1 person
              </span>
            )}
            {waits && (
              <span className="smart-wait" title={`Waits on ${waits}, whose jobs failed`}>
                <Hourglass size={13} aria-label={`Waits on ${waits}`} />
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

type DetailsProps = {
  ctx: Ctx;
  gitRef: string;
  map: S["RepoMap"];
  lay: Plan;
  b: Block;
  jobs: Map<string, Job>;
  pipeline?: S["PipelineDetail"];
  /** The block whose failed jobs hold this one up. */
  waits?: string;
  chain: boolean;
  setChain: (on: boolean) => void;
  select: (id: string | null) => void;
  /** Who is looking (a username), for what concerns them. */
  me?: string;
  /** Opens a correction in the editor; none for who cannot commit here. */
  fix?: (edit: AssayEdit) => void;
};

function Details({ ctx, gitRef, map, lay, b, jobs, pipeline, waits, chain, setChain, select, me, fix }: DetailsProps) {
  const byId = new Map(map.blocks.map((x) => [x.id, x]));
  const [kind, color] = [kindName(b), KIND[b.kind][1]];
  // once it scrolls, a line parts the name, which stays, from what goes under it
  const [scrolled, setScrolled] = useState(false);
  const ci = useRef<HTMLElement>(null);
  const go = (id: string) => (
    <button type="button" className="smart-go" onClick={() => select(id)}>
      {byId.get(id) ? title(byId.get(id)!) : id}
    </button>
  );
  const at = (p: S["MapPath"]) => (p.dir ? refLink(ctx.web, "tree", gitRef, p.path === "." ? undefined : p.path) : refLink(ctx.web, "blob", gitRef, p.path));
  // a block in a list: its mark and name, then what the link is, whether it is a guess, and the file that says so
  const row = (id: string, what?: string, e?: Edge) => (
    <li key={`${id}:${e?.kind ?? ""}`}>
      <RowMark b={byId.get(id)} />
      <span className="grow">{go(id)}</span>
      {what && <span className="meta">{what}</span>}
      {e?.inferred && <span className="smart-tag">a guess</span>}
      {e?.via && (
        <Link className="smart-via" to={refLink(ctx.web, "blob", gitRef, e.via)} title={`From ${e.via}`} aria-label={`From ${e.via}`}>
          <FileText size={13} aria-hidden />
        </Link>
      )}
    </li>
  );
  const verb = (e: Edge) => (e.kind === "part" ? undefined : VERB[e.kind]);
  const needs = lay.edges.filter((e) => e.from === b.id && e.kind !== "part");
  const holds = lay.edges.filter((e) => e.from === b.id && e.kind === "part");
  const neededBy = lay.edges.filter((e) => e.to === b.id && e.kind !== "part");
  // where it is: its image, its folders or files, and the manifest it was found in (by its name, in one of them)
  const spots = [
    ...(b.image ? [<span key="image">{b.image}</span>] : []),
    ...b.roots.map((r) => (
      <Link key={r.path} to={at(r)}>
        {where(r.path)}
      </Link>
    )),
    ...b.manifests
      .filter((m) => !b.roots.some((r) => r.path === m.path))
      .map((m) => {
        const r = b.roots.find((r) => r.dir && r.path !== "." && m.path.startsWith(`${r.path}/`));
        return (
          <Link key={m.path} to={at(m)} title={`Found in ${m.path}`}>
            {r ? m.path.slice(r.path.length + 1) : m.path}
          </Link>
        );
      }),
  ];
  const own = blockStatus(b, jobs);
  // the people to ask: those named for it first, on the line of their commits or reviews when they made some
  const reviews = new Map(b.reviewers.map((r) => [r.username, r]));
  const committers = new Set(b.people.flatMap((p) => (p.username ? [p.username] : [])));
  const onlyReviewed = b.reviewers.filter((r) => !committers.has(r.username));
  const named = new Set(b.owners.flatMap((o) => (o.startsWith("@") ? [o.slice(1).toLowerCase()] : [])));
  const isNamed = (username?: string | null) => !!username && named.has(username.toLowerCase());
  const active = new Set([...committers, ...onlyReviewed.map((r) => r.username)].map((u) => u.toLowerCase()));
  const isMe = (username?: string | null) => !!me && !!username && username.toLowerCase() === me.toLowerCase();
  const declared = (
    <span className="smart-tag" title="Named in CODEOWNERS or assay.toml">
      declared
    </span>
  );
  const person = (p: Block["people"][number]) => {
    // who changed it and reviewed it too: both, by the latest
    const r = p.username ? reviews.get(p.username) : undefined;
    const last = r && r.last > p.last ? r.last : p.last;
    return (
      <li key={`p:${p.name}`}>
        <Avatar name={p.name} username={p.username} />
        <span className="grow">
          <span className="smart-line">
            {p.username ? <UserLink username={p.username}>{p.name}</UserLink> : p.name}
            {isNamed(p.username) && declared}
          </span>
          <span className="smart-sub">
            <span title={last}>
              {plural(p.commits, "commit")}
              {r && ` · ${plural(r.reviews, "review")}`} · {timeAgo(last)}
            </span>
            {fix && isMe(p.username) && (
              <>
                {" · "}
                <button type="button" className="smart-me" title="Leave yourself out of the people to ask, under all your names and addresses" onClick={() => fix({ kind: "exclude", who: p.name })}>
                  Leave me out
                </button>
              </>
            )}
          </span>
        </span>
      </li>
    );
  };
  const reviewer = (r: Block["reviewers"][number]) => (
    <li key={`r:${r.username}`}>
      <Avatar name={r.name || r.username} username={r.username} />
      <span className="grow">
        <span className="smart-line">
          <UserLink username={r.username}>{r.name || r.username}</UserLink>
          {isNamed(r.username) && declared}
        </span>
        <span className="smart-sub" title={r.last}>
          {plural(r.reviews, "review")} · {timeAgo(r.last)}
        </span>
      </span>
    </li>
  );
  // named, without a commit or a review in the window
  const owner = (o: string) => {
    // a user's or a group's path only: what a repository's file says is never a link out of here
    const handle = /^@[A-Za-z0-9_.-]+(\/[A-Za-z0-9_.-]+)*$/.test(o) ? o.slice(1) : null;
    return (
      <li key={`o:${o}`}>
        <Avatar name={handle ?? o} username={handle && !handle.includes("/") ? handle : undefined} />
        <span className="grow">
          <span className="smart-line">
            {handle ? <Link to={`/${handle}`}>{o}</Link> : o}
            {declared}
          </span>
        </span>
      </li>
    );
  };
  const people = [
    ...b.people.filter((p) => isNamed(p.username)).map(person),
    ...onlyReviewed.filter((r) => isNamed(r.username)).map(reviewer),
    ...b.owners.filter((o) => !(o.startsWith("@") && active.has(o.slice(1).toLowerCase()))).map(owner),
    ...b.people.filter((p) => !isNamed(p.username)).map(person),
    ...onlyReviewed.filter((r) => !isNamed(r.username)).map(reviewer),
  ];
  const total = b.languages.reduce((n, l) => n + l.files, 0) || 1;
  const langs = b.languages.slice(0, 5);
  const pct = (n: number) => Math.max(1, Math.round((100 * n) / total));
  return (
    <aside className={`side-card smart-panel${scrolled ? " scrolled" : ""}`} aria-label={`${title(b)}, details`} onScroll={(e) => setScrolled(e.currentTarget.scrollTop > 0)}>
      <header className="smart-panel-head" style={{ "--k": color } as CSSProperties}>
        <Mark b={b} />
        <div className="grow">
          <h2>{title(b)}</h2>
          <div className="smart-panel-kind">
            {kind}
            {b.role && ` · ${ROLE[b.role]}`}
            {b.configured && " · from assay.toml"}
          </div>
        </div>
        <button type="button" className="icon-btn" aria-label="Close" title="Close (Esc)" onClick={() => select(null)}>
          <X size={14} />
        </button>
      </header>
      <div className="smart-intro">
        {b.description && <p className="smart-about">{b.description}</p>}
        {spots.length > 0 && (
          <div className="smart-where">
            {spots.flatMap((s, i) =>
              i
                ? [
                    <span key={`sep${i}`} className="sep" aria-hidden>
                      ·
                    </span>,
                    s,
                  ]
                : [s],
            )}
          </div>
        )}
        {b.parent && <div className="muted small">Part of {go(b.parent)}</div>}
        {own && (
          <div className="smart-states">
            <button
              type="button"
              className={`smart-state ${own}`}
              title="See its jobs"
              onClick={() => ci.current?.scrollIntoView({ block: "start", behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" })}
            >
              <span aria-hidden>
                <StatusIcon status={own} size={13} />
              </span>
              CI {statusLabel(own)}
            </button>
          </div>
        )}
        {waits && (
          <p className="smart-note smart-waits">
            <Hourglass size={14} aria-hidden />
            <span>Waits on {go(waits)}, whose jobs failed.</span>
          </p>
        )}
      </div>
      {b.files > 0 && (
        <Part icon={FileCode} title="Files" count={b.files}>
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
        </Part>
      )}
      {people.length > 0 && (
        <Part icon={Users} title="People to ask" count={people.length}>
          <ul>{people}</ul>
          {solo(b) && <p className="smart-note">{SOLO}: worth sharing what they know.</p>}
          {(b.people.length > 0 || b.reviewers.length > 0) && (
            <p className="muted small">
              From the {b.reviewers.length > 0 ? "commits and reviews" : "commits"} of the last {plural(map.days, "day")}.
            </p>
          )}
        </Part>
      )}
      {b.jobs.length > 0 && (
        <Part icon={Workflow} title="CI" count={b.jobs.length} at={ci}>
          <ul>
            {b.jobs.map((name) => {
              const j = jobs.get(name);
              // its stage, and how long it took once done
              const done = j && j.duration != null && !["running", "pending", "created", "manual", "scheduled"].includes(j.status);
              return (
                <li key={name}>
                  {j && <StatusIcon status={j.status} size={14} allowFailure={j.allow_failure} />}
                  <span className="grow">{j ? <Link to={`${ctx.web}/-/jobs/${j.id}`}>{name}</Link> : <code>{name}</code>}</span>
                  <span className="meta">{j ? `${j.stage} · ${done ? duration(j.duration!) : statusLabel(j.status, j.allow_failure)}` : pipeline && "not in this pipeline"}</span>
                </li>
              );
            })}
          </ul>
          {pipeline && (
            <p className="muted small">
              From <Link to={`${ctx.web}/-/pipelines/${pipeline.iid}`}>pipeline #{pipeline.iid}</Link>
              {` · ${statusLabel(pipeline.status)}${pipeline.status === "failed" && own !== "failed" ? " on other jobs" : ""} · ${timeAgo(pipeline.created_at)}`}
            </p>
          )}
        </Part>
      )}
      {needs.length > 0 && (
        <Part icon={ArrowRight} title="Needs" count={needs.length}>
          <ul>{needs.map((e) => row(e.to, verb(e), e))}</ul>
        </Part>
      )}
      {holds.length > 0 && (
        <Part icon={Boxes} title="Contains" count={holds.length}>
          <ul>{holds.map((e) => row(e.to))}</ul>
        </Part>
      )}
      {neededBy.length > 0 && (
        <Part icon={ArrowLeft} title="Used by" count={neededBy.length}>
          <ul>{neededBy.map((e) => row(e.from, verb(e), e))}</ul>
        </Part>
      )}
      <Impact b={b} lay={lay} byId={byId} usedBy={new Set(neededBy.map((e) => e.from))} chain={chain} setChain={setChain} go={go} />
      {fix && <Fix key={b.id} b={b} map={map} me={me} fix={fix} />}
    </aside>
  );
}

/** Corrections of the map, each opening assay.toml in the editor with it written in: checked
 *  there before it is committed. */
function Fix({ b, map, me, fix }: { b: Block; map: S["RepoMap"]; me?: string; fix: (edit: AssayEdit) => void }) {
  const [open, setOpen] = useState<"rename" | "merge" | null>(null);
  const [name, setName] = useState(b.name);
  const [about, setAbout] = useState(b.description ?? "");
  const others = map.blocks.filter((x) => x.id !== b.id).sort((x, y) => title(x).localeCompare(title(y)));
  const [into, setInto] = useState(others[0]?.id ?? "");
  const known = !!me && b.owners.some((o) => o.toLowerCase() === `@${me.toLowerCase()}`);
  const t = title(b);
  const keys: Record<string, string> = {};
  if (name.trim() && name.trim() !== b.name) keys.name = name.trim();
  if (about.trim() !== (b.description ?? "")) keys.description = about.trim();
  const toggle = (what: "rename" | "merge") => setOpen(open === what ? null : what);
  return (
    <section className="smart-fix">
      <div className="smart-part-head">
        <h3>
          <Pencil size={14} aria-hidden />
          Correct the map
        </h3>
      </div>
      <div className="row">
        <button type="button" className={`btn small${open === "rename" ? " on" : ""}`} aria-expanded={open === "rename"} onClick={() => toggle("rename")}>
          Rename or describe
        </button>
        {others.length > 0 && (
          <button type="button" className={`btn small${open === "merge" ? " on" : ""}`} aria-expanded={open === "merge"} onClick={() => toggle("merge")}>
            Merge into…
          </button>
        )}
        <button type="button" className="btn small" title="Its files go to the block around it" onClick={() => fix({ kind: "set", block: b.id, title: t, keys: { ignore: true } })}>
          Not a block
        </button>
        {me && !known && (
          <button type="button" className="btn small" title="Name yourself among the people to ask about it" onClick={() => fix({ kind: "referent", block: b.id, title: t, who: `@${me}` })}>
            I know it
          </button>
        )}
      </div>
      {open === "rename" && (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            if (Object.keys(keys).length) fix({ kind: "set", block: b.id, title: t, keys });
          }}
        >
          <label>
            Name
            <input value={name} onChange={(e) => setName(e.target.value)} />
          </label>
          <label>
            What it is for
            <input value={about} placeholder="In a line" onChange={(e) => setAbout(e.target.value)} />
          </label>
          <div className="row">
            <button className="btn small primary" disabled={!Object.keys(keys).length}>
              Write it in assay.toml
            </button>
          </div>
        </form>
      )}
      {open === "merge" && (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            if (into) fix({ kind: "set", block: b.id, title: t, keys: { merge_into: into } });
          }}
        >
          <label>
            Part of
            <Select value={into} onChange={setInto} options={others.map((x) => ({ value: x.id, label: title(x) }))} label="The block it is part of" />
          </label>
          <div className="row">
            <button className="btn small primary" disabled={!into}>
              Write it in assay.toml
            </button>
          </div>
        </form>
      )}
      <p className="muted small">Each opens assay.toml with the change written in, to check before it is committed.</p>
    </section>
  );
}

/** What a change to the block reaches: what uses it (listed above it), then what holds it and
 *  what all those reach in turn, who knows each and their CI jobs; lit up on the map when asked. */
function Impact({
  b,
  lay,
  byId,
  usedBy,
  chain,
  setChain,
  go,
}: {
  b: Block;
  lay: Plan;
  byId: Map<string, Block>;
  usedBy: Set<string>;
  chain: boolean;
  setChain: (on: boolean) => void;
  go: (id: string) => ReactNode;
}) {
  const deps = dependents(lay.edges, [b.id]);
  if (!deps.size) return null;
  // reached straight from it, not using it: what holds it
  const more = [...deps].filter(([id]) => !usedBy.has(id));
  const jobs = [...new Set([...deps.keys()].flatMap((id) => byId.get(id)?.jobs ?? []))];
  const name = (id: string) => (byId.has(id) ? title(byId.get(id)!) : id);
  const reach = `It reaches ${plural(deps.size, "block")}`;
  return (
    <Part
      icon={Zap}
      title="If it changes"
      className="smart-impact"
      action={
        <button type="button" className={`btn small${chain ? " on" : ""}`} aria-pressed={chain} onClick={() => setChain(!chain)}>
          Light them up
        </button>
      }
    >
      <p className="smart-lead">{!usedBy.size ? `${reach}:` : more.length ? `${reach}: those that use it, and these:` : `${reach}: those that use it.`}</p>
      {more.length > 0 && (
        <ul>
          {more.map(([id, through]) => {
            const d = byId.get(id);
            return (
              <li key={id}>
                <RowMark b={d} />
                <span className="grow">
                  <span className="smart-line">{go(id)}</span>
                  <span className="smart-sub">{through ? `through ${name(through)}` : "holds it"}</span>
                </span>
                {d && d.people.length > 0 && (
                  <span className="smart-people">
                    {d.people.slice(0, 3).map((p) => (
                      <Avatar key={p.name} name={p.name} username={p.username} title={p.name} />
                    ))}
                  </span>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {jobs.length > 0 && (
        <p className="smart-jobs">
          <span>Their CI jobs:</span>
          {jobs.map((j) => (
            <code key={j}>{j}</code>
          ))}
        </p>
      )}
    </Part>
  );
}

/** A part of the details: its heading, with an icon and how many it lists, and an action. */
function Part({
  icon: Icon,
  title: heading,
  count,
  action,
  className,
  at,
  children,
}: {
  icon: LucideIcon;
  title: string;
  count?: number;
  action?: ReactNode;
  className?: string;
  at?: Ref<HTMLElement>;
  children: ReactNode;
}) {
  return (
    <section className={className} ref={at}>
      <div className="smart-part-head">
        <h3>
          <Icon size={14} aria-hidden />
          {heading}
          {count != null && <span className="smart-count">{count.toLocaleString()}</span>}
        </h3>
        {action}
      </div>
      {children}
    </section>
  );
}

/** A block's mark in a list of the details, in its kind's colour. */
function RowMark({ b }: { b?: Block }) {
  return (
    <span className="smart-row-mark" style={b ? ({ "--k": KIND[b.kind][1] } as CSSProperties) : undefined} aria-hidden>
      {b && <Mark b={b} />}
    </span>
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
      <p>
        The blocks come from the manifests. An <code>assay.toml</code> at the top of the repository renames, merges, ignores or adds blocks and links, and names who to ask.
      </p>
    </div>
  );
}

/** The latest pipeline of `commit`, and its jobs by name (their latest attempt). */
function useJobs(ctx: Ctx, commit: string): { jobs: Map<string, Job>; pipeline?: S["PipelineDetail"] } {
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
  return useMemo(
    () => ({ jobs: new Map((pipeline.data?.jobs ?? []).filter((j) => !j.retried).map((j) => [j.name, j])), pipeline: pipeline.data }),
    [pipeline.data],
  );
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

/** Where a block of the graph is, from the graph's top left, inside the blocks holding it. */
function absolute(lay: Plan, id: string): { x: number; y: number } {
  const p = lay.at.get(id)!;
  if (!p.inside) return { x: p.x, y: p.y };
  const o = absolute(lay, p.inside);
  return { x: o.x + p.x, y: o.y + p.y };
}

/** How far right the graph must move, opening at `at` in a frame `height` tall, for no card to
 *  be under the frame's bottom-left `corner`: 0 when none is. A holder's card is at the top of
 *  its frame; the room around what it holds may go under. */
function cornerShift(lay: Plan, at: { x: number; y: number; zoom: number }, height: number, corner: { w: number; h: number }): number {
  let shift = 0;
  for (const id of lay.graph) {
    const o = absolute(lay, id);
    const inset = lay.holders.has(id) ? INSET : 0;
    const left = at.x + (o.x + inset) * at.zoom;
    const top = at.y + (o.y + inset) * at.zoom;
    if (left < corner.w && top < height && top + H * at.zoom > height - corner.h) shift = Math.max(shift, corner.w - left);
  }
  return shift;
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
/** The graph's top in ELK's tree: no block's id (theirs are letters, digits and dashes). */
const ROOT = "#root";

async function plan(map: S["RepoMap"]): Promise<Plan> {
  const byId = new Map(map.blocks.map((b) => [b.id, b]));
  const links: Edge[] = map.links.filter((l) => byId.has(l.from) && byId.has(l.to)).map((l) => ({ from: l.from, to: l.to, kind: l.kind, inferred: l.inferred, via: l.via }));
  const linked = (a: string, b: string) => links.some((e) => (e.from === a && e.to === b) || (e.from === b && e.to === a));
  const inside = new Map<string, string>();
  const lines: Edge[] = [];
  for (const b of map.blocks) {
    if (!b.parent || !byId.has(b.parent) || linked(b.parent, b.id)) continue;
    const part: Edge = { from: b.parent, to: b.id, kind: "part", inferred: false, via: "" };
    // never inside a block that is inside it (a map made before assay refused such loops)
    let loops = false;
    for (let p: string | undefined = b.parent, n = 0; p && !loops && n <= map.blocks.length; p = inside.get(p), n++) loops = p === b.id;
    if (loops) continue;
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
  const root: ElkNode = { id: ROOT, layoutOptions: LAYOUT, children: top.map(node), edges: [] };
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
    const home = holding(e.to).find((p) => around.has(p)) ?? ROOT;
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
