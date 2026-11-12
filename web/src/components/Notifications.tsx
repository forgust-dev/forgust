import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { DropdownMenu } from "radix-ui";
import { Bell, BellOff, BellRing, Check } from "lucide-react";
import { api, type S } from "../api/client";
import { useToast } from "../shell/toast";

export const LEVELS: { value: string; label: string; hint: string }[] = [
  { value: "watch", label: "Watch", hint: "Everything: new issues and merge requests, every comment and change" },
  { value: "participating", label: "Participate", hint: "What you take part in, are assigned to or asked to review, and mentions" },
  { value: "mention", label: "On mention", hint: "Only when someone @mentions you" },
  { value: "disabled", label: "Disabled", hint: "No email" },
];

const label = (v: string) => LEVELS.find((l) => l.value === v)?.label ?? v;

/** Your default level, for every project you have not chosen one for. */
export function NotificationDefaults() {
  const qc = useQueryClient();
  const toast = useToast();
  const q = useQuery({ queryKey: ["notifications", "me"], queryFn: () => api.get<S["LevelDto"]>("/user/notification_settings") });
  // shown at once; put back if the server refuses
  const [picked, setPicked] = useState<string | null>(null);
  const set = async (level: string) => {
    setPicked(level);
    try {
      await api.put("/user/notification_settings", { level });
      qc.invalidateQueries({ queryKey: ["notifications"] });
      toast.ok(`Notifications: ${label(level)}`);
    } catch (e) {
      setPicked(null);
      toast.error(e);
    }
  };
  return (
    <section className="card stack" style={{ padding: 16, maxWidth: 640 }}>
      <div>
        <h2 style={{ margin: 0, fontSize: "1rem" }}>Email notifications</h2>
        <p className="muted small" style={{ margin: "4px 0 0" }}>
          Your default for every project. Choose otherwise for a project with the bell on its page. Watching by default covers the projects you are a
          member of.
        </p>
      </div>
      <div role="radiogroup" aria-label="Default notification level" className="stack" style={{ gap: 8 }}>
        {LEVELS.map((l) => (
          <label key={l.value} className="check">
            <input type="radio" name="level" checked={(picked ?? q.data?.level) === l.value} onChange={() => set(l.value)} />
            <span>
              <strong>{l.label}</strong> <span className="muted small">{l.hint}</span>
            </span>
          </label>
        ))}
      </div>
    </section>
  );
}

/** The bell on a project: your level here (or your default). */
export function NotificationBell({ projectId }: { projectId: number }) {
  const qc = useQueryClient();
  const toast = useToast();
  const key = ["notifications", "project", projectId];
  const q = useQuery({ queryKey: key, queryFn: () => api.get<S["LevelDto"]>(`/projects/${projectId}/notification_settings`) });
  const set = async (level: string) => {
    try {
      const r = await api.put<S["LevelDto"]>(`/projects/${projectId}/notification_settings`, { level });
      qc.setQueryData(key, r);
      toast.ok(`Notifications for this project: ${level === "global" ? `your default (${label(r.effective)})` : label(level)}`);
    } catch (e) {
      toast.error(e);
    }
  };
  const effective = q.data?.effective ?? "participating";
  const Icon = effective === "watch" ? BellRing : effective === "disabled" ? BellOff : Bell;
  const current = q.data?.level ?? "global";
  const items = [{ value: "global", label: "Your default", hint: "As in your settings" }, ...LEVELS];
  return (
    <DropdownMenu.Root modal={false}>
      <DropdownMenu.Trigger asChild>
        <button className="btn small" aria-label={`Notifications: ${label(effective)}`} title="Notifications for this project">
          <Icon size={14} /> <span className="hide-narrow">{label(effective)}</span>
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content className="dropdown notif-menu" align="end" sideOffset={6}>
          {items.map((l) => (
            <DropdownMenu.Item key={l.value} className="dropdown-item" onSelect={() => set(l.value)}>
              <span className="notif-check">{current === l.value && <Check size={14} />}</span>
              <span>
                <strong>{l.label}</strong>
                <span className="muted small notif-hint">{l.hint}</span>
              </span>
            </DropdownMenu.Item>
          ))}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
