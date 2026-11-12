import { Fragment, forwardRef, useState, type ComponentPropsWithoutRef, type ReactNode } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { Check, Milestone, Settings2, Tag } from "lucide-react";
import { api, type S } from "../api/client";
import { Popover } from "./controls";

/** A milestone: its icon and title, linking to its page when its id is known. */
export function MilestoneChip({ web, id, title }: { web: string; id?: number | null; title: string }) {
  const inner = (
    <>
      <Milestone size={12} aria-hidden />
      {title}
    </>
  );
  return id != null ? (
    <Link className="milestone-chip" to={`${web}/-/milestones/${id}`} title={`Milestone ${title}`}>
      {inner}
    </Link>
  ) : (
    <span className="milestone-chip" title={`Milestone ${title}`}>
      {inner}
    </span>
  );
}

export function useLabels(projectId: number) {
  return useQuery({
    queryKey: ["labels", projectId],
    queryFn: () => api.get<S["LabelWithCounts"][]>(`/projects/${projectId}/labels`),
    staleTime: 30_000,
  });
}

/** A label: its color as a dot, its name; links to the filtered list when `to` is set. */
export function LabelChip({ label, to }: { label: Pick<S["Label"], "name" | "color" | "description">; to?: string }) {
  const inner = (
    <>
      <span className="label-dot" style={{ background: label.color }} />
      {label.name}
    </>
  );
  return to ? (
    <Link className="label-chip" to={to} title={label.description || label.name}>
      {inner}
    </Link>
  ) : (
    <span className="label-chip" title={label.description || label.name}>
      {inner}
    </span>
  );
}

export function LabelList({ labels, to }: { labels: S["Label"][]; to?: (l: S["Label"]) => string }) {
  if (labels.length === 0) return null;
  return (
    <span className="label-list">
      {labels.map((l) => (
        <LabelChip key={l.id} label={l} to={to?.(l)} />
      ))}
    </span>
  );
}

/**
 * Checkboxes over the project's labels; `onChange` gets the new set of names.
 * Changes apply as they are clicked, like GitLab's sidebar.
 */
export function LabelPicker({ projectId, manage, value, onChange, trigger }: { projectId: number; manage: string; value: string[]; onChange: (names: string[]) => void; trigger: ReactNode }) {
  const labels = useLabels(projectId);
  const [filter, setFilter] = useState("");
  const list = (labels.data ?? []).filter((l) => l.name.toLowerCase().includes(filter.toLowerCase()));
  const toggle = (name: string) => onChange(value.includes(name) ? value.filter((n) => n !== name) : [...value, name]);
  return (
    <Popover trigger={trigger} label="Labels" align="end">
      <div className="label-picker">
        <input autoFocus placeholder="Filter labels" aria-label="Filter labels" value={filter} onChange={(e) => setFilter(e.target.value)} />
        <div className="label-picker-list" role="listbox" aria-multiselectable>
          {list.length === 0 && <p className="muted small">{labels.data?.length ? "No label matches." : "No labels yet."}</p>}
          {list.map((l) => {
            const on = value.includes(l.name);
            return (
              <button key={l.id} type="button" role="option" aria-selected={on} className={on ? "on" : ""} onClick={() => toggle(l.name)}>
                <span className="check">{on && <Check size={13} />}</span>
                <span className="label-dot" style={{ background: l.color }} />
                <span className="grow ellipsis">{l.name}</span>
              </button>
            );
          })}
        </div>
        <Link to={manage} className="label-picker-manage">
          <Settings2 size={13} /> Manage labels
        </Link>
      </div>
    </Popover>
  );
}

/** A trigger for LabelPicker in a side panel (forwards the popover's props and ref). */
export const EditLabelsButton = forwardRef<HTMLButtonElement, ComponentPropsWithoutRef<"button">>(function EditLabelsButton(props, ref) {
  return (
    <button ref={ref} className="link-btn small" type="button" {...props}>
      <Tag size={13} /> Edit
    </button>
  );
});

/**
 * A system note's text: `~"label"` and `%"milestone"` references as chips
 * (colors and links from the project's when known), `!12`, `#3` and `@user`
 * as links.
 */
export function SystemText({ body, web, labels, milestones }: { body: string; web: string; labels?: S["LabelWithCounts"][]; milestones?: S["Milestone"][] }) {
  const parts = body.split(/(~"[^"]+"|%"[^"]*"|(?<![\w&/])[!#]\d+\b|(?<![\w/])@[\w.-]*\w)/g);
  return (
    <span className="system-text">
      {parts.map((part, i) => {
        let m = part.match(/^~"([^"]+)"$/);
        if (m) {
          const l = labels?.find((x) => x.name === m![1]);
          return <LabelChip key={i} label={{ name: m[1], color: l?.color ?? "var(--text-faint)", description: l?.description ?? "" }} />;
        }
        if ((m = part.match(/^%"([^"]*)"$/))) {
          const ms = milestones?.find((x) => x.title === m![1]);
          return <MilestoneChip key={i} web={web} id={ms?.id} title={m[1]} />;
        }
        if ((m = part.match(/^([!#])(\d+)$/))) {
          return (
            <Link key={i} to={`${web}/-/${m[1] === "!" ? "merge_requests" : "issues"}/${m[2]}`}>
              {part}
            </Link>
          );
        }
        if (part.startsWith("@") && part.length > 1) {
          return (
            <Link key={i} to={`/${part.slice(1)}`} className="user-link">
              {part}
            </Link>
          );
        }
        return <Fragment key={i}>{part}</Fragment>;
      })}
    </span>
  );
}
