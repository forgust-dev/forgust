import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { CaseSensitive, FileCode, Regex, X } from "lucide-react";
import { api, qs, refLink, type S } from "../api/client";
import { Empty, ErrorBox, Spinner, useDebounced } from "../components/ui";
import { Select } from "../components/controls";

const PER_PAGE = 20;

/** The lines of one file that match, with their real numbers, linked to the file. */
function FileHit({ f }: { f: S["CodeHit"] }) {
  const blob = refLink(`/${f.project_path}`, "blob", f.ref, f.path);
  const shown = f.chunks.reduce((n, c) => n + c.lines.filter((l) => l.includes("<mark>")).length, 0);
  return (
    <section className="code-hit" aria-label={`${f.project_path}: ${f.path}`}>
      <header className="code-hit-head">
        <FileCode size={15} className="faint" />
        <span className="code-hit-path">
          <Link to={`/${f.project_path}`} className="muted">
            {f.project_path}
          </Link>
          <span className="faint"> / </span>
          <Link to={blob}>{f.path}</Link>
        </span>
        {f.language && <span className="badge">{f.language}</span>}
        <span className="faint small code-hit-count">{f.matches > shown ? `${shown} of ${f.matches} matching lines` : f.matches === 1 ? "1 matching line" : `${f.matches} matching lines`}</span>
      </header>
      {f.chunks.map((c) => (
        <div className="code code-wrap code-hit-chunk" key={c.start}>
          {c.lines.map((html, i) => (
            <div className="code-line" key={i}>
              <span className="ln">
                <Link to={`${blob}#L${c.start + i}`}>{c.start + i}</Link>
              </span>
              <span className="src" dangerouslySetInnerHTML={{ __html: html || " " }} />
            </div>
          ))}
        </div>
      ))}
    </section>
  );
}

/** A filter typed into the toolbar: it settles before it is put in the address. */
function FilterInput({ value, onChange, placeholder, width }: { value: string; onChange: (v: string) => void; placeholder: string; width: number }) {
  const [text, setText] = useState(value);
  useEffect(() => setText(value), [value]);
  const settled = useDebounced(text.trim(), 300);
  useEffect(() => {
    if (settled !== value) onChange(settled);
  }, [settled]);
  return <input type="search" value={text} placeholder={placeholder} aria-label={placeholder} style={{ width }} onChange={(e) => setText(e.target.value)} />;
}

/**
 * Text inside the files of default branches. `params` is the address: `q`,
 * `regex`, `case`, `path`, `lang`, and `project_id` or `group_id` to search one
 * place. `set` changes one of them ("" removes it).
 */
export function CodeSearch({ params, set }: { params: URLSearchParams; set: (k: string, v: string) => void }) {
  const q = params.get("q") ?? "";
  const regex = params.get("regex") === "1";
  const exact = params.get("case") === "1";
  const path = params.get("path") ?? "";
  const lang = params.get("lang") ?? "";
  const project = params.get("project_id") ?? "";
  const group = params.get("group_id") ?? "";
  const r = useInfiniteQuery({
    queryKey: ["search-code", q, regex, exact, path, lang, project, group],
    queryFn: ({ pageParam }) =>
      api.get<S["CodeResults"]>(
        `/search/code${qs({ q, regex: regex || undefined, case_sensitive: exact || undefined, path, language: lang, project_id: project, group_id: group, after: pageParam, per_page: PER_PAGE })}`,
      ),
    initialPageParam: "",
    getNextPageParam: (last) => last.next ?? undefined,
    enabled: !!q,
    retry: false,
  });
  // a file's language comes from its extension: the list is what the server knows
  const languages = useQuery({ queryKey: ["search-code-languages"], queryFn: () => api.get<string[]>("/search/code/languages"), staleTime: Infinity });
  const files = r.data?.pages.flatMap((p) => p.files) ?? [];
  const partial = r.data?.pages.some((p) => p.partial) ?? false;
  return (
    <div className="stack" style={{ gap: 12 }}>
      <div className="row wrap code-search-filters">
        <button type="button" className="btn small" aria-pressed={exact} onClick={() => set("case", exact ? "" : "1")} title="Match upper and lower case exactly">
          <CaseSensitive size={15} /> Match case
        </button>
        <button type="button" className="btn small" aria-pressed={regex} onClick={() => set("regex", regex ? "" : "1")} title="The search text is a regular expression">
          <Regex size={14} /> Regular expression
        </button>
        <FilterInput value={path} onChange={(v) => set("path", v)} placeholder="Path contains…" width={170} />
        <Select
          label="Language"
          value={lang}
          onChange={(v) => set("lang", v)}
          style={{ width: 170, height: 28 }}
          options={[{ value: "", label: "Any language" }, ...(languages.data ?? []).map((l) => ({ value: l.toLowerCase(), label: l }))]}
        />
        {(project || group) && (
          <span className="pill">
            in {project || group}
            <button type="button" className="icon-btn" aria-label="Search everywhere" title="Search everywhere" onClick={() => set(project ? "project_id" : "group_id", "")}>
              <X size={12} />
            </button>
          </span>
        )}
      </div>
      {r.isLoading ? (
        <Spinner />
      ) : r.error ? (
        <ErrorBox error={r.error} />
      ) : files.length === 0 ? (
        <Empty>
          No code matches “{q}”{partial ? " in the files looked at so far" : ""}. Code search covers each project's default branch.
        </Empty>
      ) : (
        <>
          {files.map((f) => (
            <FileHit key={`${f.project_id}:${f.path}`} f={f} />
          ))}
          {partial && <p className="small muted">The search stopped early: there were too many files to read. A longer text or a filter finds the rest.</p>}
          {r.hasNextPage && (
            <div className="pager">
              <button className="btn" disabled={r.isFetchingNextPage} onClick={() => r.fetchNextPage()}>
                {r.isFetchingNextPage ? "Searching…" : "More results"}
              </button>
            </div>
          )}
        </>
      )}
    </div>
  );
}
