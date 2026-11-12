import { forwardRef, useState, type ComponentPropsWithoutRef, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { Check, CircleCheck, CircleDot, Clock, MessageSquare, RotateCcw, UserPlus } from "lucide-react";
import { api, qs, type S } from "../api/client";
import { Popover } from "./controls";
import { Avatar, UserLink } from "./ui";

type Person = S["UserBrief"];
type Reviewer = S["Reviewer"];

/** People who can be assigned in a project (Reporter+), filtered by `search`. */
export function useProjectUsers(projectId: number, search: string, enabled = true) {
  return useQuery({
    queryKey: ["project-users", projectId, search],
    queryFn: () => api.get<Person[]>(`/projects/${projectId}/users${qs({ search })}`),
    staleTime: 30_000,
    enabled,
  });
}

const display = (p: Person) => p.name || p.username;

/** Avatars with names, one per line. */
export function PeopleList({ people, empty = "None" }: { people: Person[]; empty?: string }) {
  if (people.length === 0) return <span className="muted">{empty}</span>;
  return (
    <span className="people-list">
      {people.map((p) => (
        <span key={p.id} className="person" title={`@${p.username}`}>
          <Avatar name={display(p)} username={p.username} />
          <UserLink username={p.username} className="ellipsis">
            {display(p)}
          </UserLink>
        </span>
      ))}
    </span>
  );
}

/** Avatars only (list rows). */
export function PeopleAvatars({ people, label }: { people: Person[]; label: string }) {
  if (people.length === 0) return null;
  return (
    <span className="avatars" aria-label={`${label}: ${people.map((p) => p.username).join(", ")}`}>
      {people.slice(0, 3).map((p) => (
        <Avatar key={p.id} name={display(p)} username={p.username} title={`${label}: ${display(p)}`} />
      ))}
    </span>
  );
}

const REVIEW: Record<string, { icon: ReactNode; text: string; cls: string }> = {
  unreviewed: { icon: <Clock size={13} />, text: "Review requested", cls: "faint" },
  reviewed: { icon: <MessageSquare size={13} />, text: "Commented", cls: "muted" },
  approved: { icon: <CircleCheck size={13} />, text: "Approved", cls: "ok-text" },
  requested_changes: { icon: <CircleDot size={13} />, text: "Requested changes", cls: "danger-text" },
};

/** Reviewers with what each did; `onRerequest` shows a re-request button. */
export function ReviewerList({ reviewers, onRerequest }: { reviewers: Reviewer[]; onRerequest?: (r: Reviewer) => void }) {
  if (reviewers.length === 0) return <span className="muted">None</span>;
  return (
    <span className="people-list">
      {reviewers.map((r) => {
        const s = REVIEW[r.state] ?? REVIEW.unreviewed;
        return (
          <span key={r.id} className="person" title={`@${r.username}: ${s.text}`}>
            <Avatar name={display(r)} username={r.username} ok={r.state === "approved"} />
            <UserLink username={r.username} className="ellipsis grow">
              {display(r)}
            </UserLink>
            {r.review_note_id != null ? (
              <a
                href={`#note_${r.review_note_id}`}
                className={`review-state ${s.cls}`}
                title={`@${r.username} requested changes: see the comment`}
                aria-label={`${s.text}: see the comment`}
                onClick={(e) => {
                  e.preventDefault();
                  document.getElementById(`note_${r.review_note_id}`)?.scrollIntoView({ behavior: "smooth", block: "center" });
                }}
              >
                {s.icon}
              </a>
            ) : (
              <span className={`review-state ${s.cls}`} aria-label={s.text}>
                {s.icon}
              </span>
            )}
            {onRerequest && r.state !== "unreviewed" && (
              <button type="button" className="icon-btn" title={`Ask ${display(r)} to review again`} aria-label={`Re-request review from ${r.username}`} onClick={() => onRerequest(r)}>
                <RotateCcw size={13} />
              </button>
            )}
          </span>
        );
      })}
    </span>
  );
}

/**
 * Pick people of a project; changes apply as they are clicked, like the label
 * picker. `selected` are shown first so they can be removed without searching.
 */
export function PeoplePicker({ projectId, label, selected, onChange, trigger }: { projectId: number; label: string; selected: Person[]; onChange: (ids: number[]) => void; trigger: ReactNode }) {
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState("");
  const users = useProjectUsers(projectId, search.trim(), open);
  const ids = selected.map((p) => p.id);
  const rest = (users.data ?? []).filter((u) => !ids.includes(u.id));
  const q = search.trim().toLowerCase();
  const shown = [...selected.filter((p) => !q || p.username.toLowerCase().includes(q) || p.name.toLowerCase().includes(q)), ...rest];
  const toggle = (id: number) => onChange(ids.includes(id) ? ids.filter((x) => x !== id) : [...ids, id]);
  return (
    <span onFocusCapture={() => setOpen(true)} onPointerDownCapture={() => setOpen(true)}>
      <Popover trigger={trigger} label={label} align="end">
        <div className="label-picker">
          <input autoFocus placeholder="Search people" aria-label={`Search ${label.toLowerCase()}`} value={search} onChange={(e) => setSearch(e.target.value)} />
          <div className="label-picker-list" role="listbox" aria-multiselectable>
            {shown.length === 0 && <p className="muted small">{users.isLoading ? "Loading…" : "Nobody matches."}</p>}
            {shown.map((u) => {
              const on = ids.includes(u.id);
              return (
                <button key={u.id} type="button" role="option" aria-selected={on} className={on ? "on" : ""} onClick={() => toggle(u.id)}>
                  <span className="check">{on && <Check size={13} />}</span>
                  <Avatar name={display(u)} username={u.username} />
                  <span className="grow ellipsis">
                    {display(u)} <span className="faint">@{u.username}</span>
                  </span>
                </button>
              );
            })}
          </div>
        </div>
      </Popover>
    </span>
  );
}

/** A trigger for PeoplePicker in a side panel (forwards the popover's props and ref). */
export const EditPeopleButton = forwardRef<HTMLButtonElement, ComponentPropsWithoutRef<"button">>(function EditPeopleButton(props, ref) {
  return (
    <button ref={ref} className="link-btn small" type="button" {...props}>
      <UserPlus size={13} /> Edit
    </button>
  );
});
