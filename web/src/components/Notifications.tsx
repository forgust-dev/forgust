import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { DropdownMenu } from "radix-ui";
import { Bell, BellOff, BellRing, Check } from "lucide-react";
import { api, type S } from "../api/client";
import { useNotificationChannels } from "../prefetch";
import { useToast } from "../shell/toast";
import { Section } from "./ui";

export const LEVELS: { value: string; label: string; hint: string }[] = [
  { value: "watch", label: "Watch", hint: "Everything: new issues and merge requests, every comment and change" },
  { value: "participating", label: "Participate", hint: "What you take part in, are assigned to or asked to review, and mentions" },
  { value: "mention", label: "On mention", hint: "Only when someone @mentions you" },
  { value: "disabled", label: "Disabled", hint: "Nothing" },
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
    <div className="settings-page">
      <Section
        title="What you hear about"
        description="Your default for every project. Choose otherwise for a project with the bell on its page. Watching by default covers the projects you are a member of."
      >
        <div role="radiogroup" aria-label="Default notification level" className="card stack" style={{ padding: 16, gap: 8 }}>
          {LEVELS.map((l) => (
            <label key={l.value} className="check top">
              <input type="radio" name="level" checked={(picked ?? q.data?.level) === l.value} onChange={() => set(l.value)} />
              <span>
                <strong>{l.label}</strong> <span className="muted small">{l.hint}</span>
              </span>
            </label>
          ))}
        </div>
      </Section>
      <NotificationChannels />
    </div>
  );
}

// (a kind the server adds must be named here: the type says so)
const KINDS: Record<S["Kind"], { label: string; hint: string }> = {
  mention: { label: "Mentions", hint: "Someone @mentions you" },
  assignment: { label: "Assignments and review requests", hint: "You are assigned, or asked to review" },
  activity: { label: "Activity", hint: "New issues and merge requests, comments, closing and merging" },
  review: { label: "Reviews of your merge requests", hint: "Approved, or changes requested" },
  pipeline_failed: { label: "Failed pipelines", hint: "Pipelines you started, and those of your merge requests" },
  pipeline_fixed: { label: "Fixed pipelines", hint: "A pipeline passes where the one before failed" },
  security: { label: "Security findings", hint: "What a scan found: vulnerable dependencies, secrets in the code" },
};

type Channel = "web" | "email" | "screen";

/** Where each kind of notification goes: the bell in the app, a message on
 * screen as it arrives, an email. */
function NotificationChannels() {
  const qc = useQueryClient();
  const toast = useToast();
  const key = ["notifications", "channels"];
  const q = useNotificationChannels();
  // shown at once; put back if the server refuses
  const [picked, setPicked] = useState<Record<string, boolean>>({});
  const on = (c: S["ChannelDto"], channel: Channel) => picked[`${c.kind}:${channel}`] ?? c[channel];
  const set = async (kind: string, channel: Channel, enabled: boolean) => {
    const id = `${kind}:${channel}`;
    setPicked((p) => ({ ...p, [id]: enabled }));
    try {
      await api.put("/user/notification_channels", { kind, channel, enabled });
    } catch (e) {
      toast.error(e);
    }
    // the list is read again (answers to quick changes may cross); then this pick
    // goes, unless a newer one replaced it
    await qc.invalidateQueries({ queryKey: key });
    setPicked(({ [id]: now, ...rest }) => (now === enabled ? rest : { ...rest, [id]: now }));
  };
  return (
    <Section
      title="Where it goes"
      description="For each kind of notification: the bell at the top of the app, a message on screen when it arrives while the app is open, and an email."
    >
      <div className="card" style={{ padding: "12px 16px" }}>
        <table className="notif-channels">
          <thead>
            <tr>
              <th scope="col" aria-label="Kind" />
              <th scope="col">In app</th>
              <th scope="col">On screen</th>
              <th scope="col">Email</th>
            </tr>
          </thead>
          <tbody>
            {(q.data ?? []).map((c) => {
              const k = KINDS[c.kind];
              const box = (channel: Channel, name: string, disabled = false) => (
                <td>
                  <input
                    type="checkbox"
                    aria-label={`${k.label}: ${name}`}
                    checked={on(c, channel) && !disabled}
                    disabled={disabled}
                    onChange={(e) => set(c.kind, channel, e.target.checked)}
                  />
                </td>
              );
              return (
                <tr key={c.kind}>
                  <th scope="row">
                    <strong>{k.label}</strong>
                    <span className="muted small notif-hint">{k.hint}</span>
                  </th>
                  {box("web", "in app")}
                  {/* what does not reach the bell cannot show on screen */}
                  {box("screen", "on screen", !on(c, "web"))}
                  {box("email", "email")}
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </Section>
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
