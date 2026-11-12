import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useQuery } from "@tanstack/react-query";
import { CircleDot, GitMerge, Paperclip } from "lucide-react";
import { api, qs, type S } from "../api/client";
import { Avatar, useDebounced } from "./ui";
import { Markdown } from "./Markdown";

type Trigger = { char: "@" | "#" | "!"; start: number; query: string };
type Item = {
  key: string;
  insert: string;
  label: string;
  sub: string;
  kind: "user" | "issue" | "mr";
  state?: string;
};

/** The reference being typed before the caret: `@na`, `#12`, `!fix`. */
function triggerAt(text: string, caret: number): Trigger | null {
  const before = text.slice(0, caret);
  const m = before.match(/(?:^|[\s([{,;])([@#!])([\w.\-]*)$/);
  if (!m) return null;
  return {
    char: m[1] as Trigger["char"],
    start: caret - m[2].length - 1,
    query: m[2],
  };
}

/**
 * Where the caret is inside a textarea, relative to it (a mirror element laid
 * out like the textarea, measured up to the caret).
 */
function caretXY(ta: HTMLTextAreaElement, pos: number) {
  const div = document.createElement("div");
  const cs = getComputedStyle(ta);
  for (const p of [
    "boxSizing",
    "width",
    "fontFamily",
    "fontSize",
    "fontWeight",
    "lineHeight",
    "letterSpacing",
    "paddingTop",
    "paddingRight",
    "paddingBottom",
    "paddingLeft",
    "borderTopWidth",
    "borderLeftWidth",
    "tabSize",
  ] as const) {
    div.style[p] = cs[p];
  }
  div.style.position = "absolute";
  div.style.visibility = "hidden";
  div.style.whiteSpace = "pre-wrap";
  div.style.overflowWrap = "break-word";
  div.textContent = ta.value.slice(0, pos);
  const mark = document.createElement("span");
  mark.textContent = "​";
  div.appendChild(mark);
  document.body.appendChild(div);
  const x = mark.offsetLeft;
  const y = mark.offsetTop + parseFloat(cs.lineHeight || "20") - ta.scrollTop;
  div.remove();
  // fixed coordinates in the window (the list is portaled out of clipping parents)
  const r = ta.getBoundingClientRect();
  return {
    x: Math.max(8, Math.min(r.left + x, window.innerWidth - 300)),
    y: r.top + Math.min(Math.max(y, 0), ta.clientHeight),
  };
}

/** Suggestions for a trigger: project members, issues, merge requests. */
function useSuggestions(projectId: number, t: Trigger | null): Item[] {
  const query = useDebounced(t?.query ?? "", 120);
  const kind = t?.char;
  const q = useQuery({
    queryKey: ["md-suggest", projectId, kind, query],
    enabled: !!kind,
    staleTime: 30_000,
    // keep the last list while the next query loads: the list never blinks
    // shut between keystrokes (an Enter then would insert a newline)
    placeholderData: (prev, prevQuery) => (prevQuery?.queryKey[2] === kind ? prev : undefined),
    queryFn: async (): Promise<Item[]> => {
      if (kind === "@") {
        const users = await api.get<S["UserBrief"][]>(
          `/projects/${projectId}/users${qs({ search: query })}`,
        );
        return users.map((u) => ({
          key: `u${u.id}`,
          insert: `@${u.username}`,
          label: u.name || u.username,
          sub: `@${u.username}`,
          kind: "user",
        }));
      }
      if (kind === "#") {
        const r = await api.get<S["IssueList"]>(
          `/projects/${projectId}/issues${qs({ search: query, state: "all", per_page: 8 })}`,
        );
        return r.issues.map((i) => ({
          key: `i${i.iid}`,
          insert: `#${i.iid}`,
          label: i.title,
          sub: `#${i.iid}`,
          kind: "issue",
          state: i.state,
        }));
      }
      const list = await api.get<S["MrListItem"][]>(
        `/projects/${projectId}/merge_requests${qs({ search: query, state: "all", per_page: 8 })}`,
      );
      return list.map((m) => ({
        key: `m${m.iid}`,
        insert: `!${m.iid}`,
        label: m.title,
        sub: `!${m.iid}`,
        kind: "mr",
        state: m.state,
      }));
    },
  });
  return kind ? (q.data ?? []) : [];
}

/**
 * A Markdown textarea with a Preview tab rendered by the server (same renderer
 * and sanitizer as saved notes, with links resolved inside the project).
 * Typing `@`, `#` or `!` suggests people, issues and merge requests.
 */
export function MarkdownEditor({
  projectId,
  value,
  onChange,
  rows = 4,
  placeholder,
  autoFocus,
  label,
}: {
  projectId: number;
  value: string;
  onChange: (v: string) => void;
  rows?: number;
  placeholder?: string;
  autoFocus?: boolean;
  label?: string;
}) {
  const [tab, setTab] = useState<"write" | "preview">("write");
  const ta = useRef<HTMLTextAreaElement>(null);
  const [trigger, setTrigger] = useState<Trigger | null>(null);
  const [active, setActive] = useState(0);
  const [pos, setPos] = useState({ x: 0, y: 0 });
  const listId = useId();
  const items = useSuggestions(projectId, trigger);
  const open = trigger != null && items.length > 0;
  const pending = useRef<number | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const [uploading, setUploading] = useState(0);
  const [uploadError, setUploadError] = useState<string | null>(null);
  // the latest value: uploads finish after other edits
  const latest = useRef(value);
  latest.current = value;

  /** Upload files and put their Markdown at the caret (or the end). */
  const attach = async (files: File[]) => {
    if (files.length === 0) return;
    setUploadError(null);
    for (const f of files) {
      setUploading((n) => n + 1);
      try {
        const u = await api.upload(projectId, f);
        const el = ta.current;
        const cur = latest.current;
        const at = el && document.activeElement === el ? el.selectionEnd : cur.length;
        const before = cur.slice(0, at);
        const sep = before && !before.endsWith("\n") ? "\n" : "";
        const insert = `${sep}${u.markdown}\n`;
        pending.current = at + insert.length;
        onChange(`${before}${insert}${cur.slice(at)}`);
      } catch (e) {
        setUploadError(`${f.name}: ${e instanceof Error ? e.message : "upload failed"}`);
      } finally {
        setUploading((n) => n - 1);
      }
    }
  };

  const preview = useQuery({
    queryKey: ["markdown", projectId, value],
    queryFn: () =>
      api.post<S["MarkdownHtml"]>(`/projects/${projectId}/markdown`, {
        text: value,
      }),
    enabled: tab === "preview" && value.trim() !== "",
    staleTime: Infinity,
  });

  // a reference dismissed with Escape stays closed until another one starts
  const dismissed = useRef<number | null>(null);
  const look = () => {
    const el = ta.current;
    if (!el || el.selectionStart !== el.selectionEnd) return setTrigger(null);
    const t = triggerAt(el.value, el.selectionStart);
    if (!t) dismissed.current = null;
    if (t && dismissed.current === t.start) return setTrigger(null);
    setTrigger(t);
    if (t) {
      setPos(caretXY(el, t.start));
      setActive(0);
    }
  };

  const pick = (it: Item) => {
    const el = ta.current;
    if (!el || !trigger) return;
    const caret = el.selectionStart;
    const next = `${value.slice(0, trigger.start)}${it.insert} ${value.slice(caret)}`;
    pending.current = trigger.start + it.insert.length + 1;
    setTrigger(null);
    onChange(next);
  };

  // put the caret after an inserted reference once the new value is rendered
  useLayoutEffect(() => {
    if (pending.current != null && ta.current) {
      ta.current.setSelectionRange(pending.current, pending.current);
      ta.current.focus();
      pending.current = null;
    }
  }, [value]);

  useEffect(() => {
    if (active >= items.length) setActive(0);
  }, [items.length, active]);

  // the list sits at fixed coordinates: scrolling the page closes it
  useEffect(() => {
    if (!trigger) return;
    const close = (e: Event) => e.target !== ta.current && setTrigger(null);
    window.addEventListener("scroll", close, true);
    return () => window.removeEventListener("scroll", close, true);
  }, [trigger]);

  return (
    <div className="md-editor">
      <div className="md-tabs" role="tablist">
        <button
          type="button"
          role="tab"
          aria-selected={tab === "write"}
          className={tab === "write" ? "active" : ""}
          onClick={() => setTab("write")}
        >
          Write
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={tab === "preview"}
          className={tab === "preview" ? "active" : ""}
          onClick={() => setTab("preview")}
        >
          Preview
        </button>
        <span className="grow" />
        <span className="faint small hide-narrow">
          Markdown · @ # ! to reference
        </span>
        <button type="button" className="icon-btn" title="Attach files (or paste or drop them)" aria-label="Attach a file" onClick={() => fileInput.current?.click()}>
          <Paperclip size={14} />
        </button>
        <input
          ref={fileInput}
          type="file"
          multiple
          hidden
          aria-label="Files to attach"
          onChange={(e) => {
            attach(Array.from(e.target.files ?? []));
            e.target.value = "";
          }}
        />
      </div>
      {(uploading > 0 || uploadError) && (
        <div className={`md-upload small ${uploadError ? "error-text" : "muted"}`} role="status">
          {uploading > 0 ? `Uploading ${uploading} file${uploading > 1 ? "s" : ""}…` : uploadError}
        </div>
      )}
      {tab === "write" ? (
        <div className="md-write">
          <textarea
            ref={ta}
            aria-label={label ?? "Markdown"}
            autoFocus={autoFocus}
            rows={rows}
            value={value}
            placeholder={placeholder}
            aria-autocomplete="list"
            aria-controls={open ? listId : undefined}
            aria-activedescendant={open ? `${listId}-${active}` : undefined}
            onChange={(e) => {
              onChange(e.target.value);
              requestAnimationFrame(look);
            }}
            onClick={look}
            onPaste={(e) => {
              const files = Array.from(e.clipboardData.files);
              if (files.length > 0) {
                e.preventDefault();
                attach(files);
              }
            }}
            onDragOver={(e) => {
              if (e.dataTransfer.types.includes("Files")) e.preventDefault();
            }}
            onDrop={(e) => {
              const files = Array.from(e.dataTransfer.files);
              if (files.length > 0) {
                e.preventDefault();
                attach(files);
              }
            }}
            onBlur={() => setTimeout(() => setTrigger(null), 120)}
            onKeyDown={(e) => {
              if (open) {
                if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                  e.preventDefault();
                  const n = items.length;
                  setActive(
                    (a) => (a + (e.key === "ArrowDown" ? 1 : n - 1)) % n,
                  );
                  return;
                }
                if (e.key === "Enter" || e.key === "Tab") {
                  e.preventDefault();
                  pick(items[active]);
                  return;
                }
              }
              // Escape drops the suggestions, also before they arrive (or before
              // the reference just typed was even noticed)
              const now = e.key === "Escape" ? (trigger ?? triggerAt(e.currentTarget.value, e.currentTarget.selectionStart)) : null;
              if (now) {
                e.preventDefault();
                e.stopPropagation();
                dismissed.current = now.start;
                setTrigger(null);
                return;
              }
              // Ctrl/Cmd+Enter submits the surrounding form
              if (
                e.key === "Enter" &&
                (e.ctrlKey || e.metaKey) &&
                value.trim()
              ) {
                e.preventDefault();
                e.currentTarget.form?.requestSubmit();
              }
            }}
            onKeyUp={(e) => {
              if (["ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key))
                look();
            }}
          />
          {open &&
            createPortal(
              <ul
                className="md-suggest"
                id={listId}
                role="listbox"
                aria-label="Suggestions"
                style={{ left: pos.x, top: pos.y + 4 }}
              >
                {items.map((it, i) => (
                  <li
                    key={it.key}
                    id={`${listId}-${i}`}
                    role="option"
                    aria-selected={i === active}
                    className={i === active ? "active" : ""}
                    onMouseDown={(e) => {
                      e.preventDefault();
                      pick(it);
                    }}
                    onMouseEnter={() => setActive(i)}
                  >
                    {it.kind === "user" ? (
                      <Avatar name={it.label} username={it.sub.slice(1)} />
                    ) : it.kind === "issue" ? (
                      <CircleDot
                        size={14}
                        className={it.state === "closed" ? "faint" : "ic-ok"}
                      />
                    ) : (
                      <GitMerge
                        size={14}
                        className={
                          it.state === "merged"
                            ? "ic-accent"
                            : it.state === "closed"
                              ? "ic-danger"
                              : "ic-ok"
                        }
                      />
                    )}
                    <span className="grow ellipsis">{it.label}</span>
                    <span className="faint mono small">{it.sub}</span>
                  </li>
                ))}
              </ul>,
              document.body,
            )}
        </div>
      ) : (
        <div className="md-preview markdown" style={{ minHeight: rows * 22 }}>
          {!value.trim() ? (
            <span className="faint">Nothing to preview.</span>
          ) : preview.isLoading ? (
            <span className="faint">Rendering…</span>
          ) : preview.error ? (
            <span className="faint">Preview unavailable.</span>
          ) : (
            <Markdown html={preview.data?.html ?? ""} />
          )}
        </div>
      )}
    </div>
  );
}
