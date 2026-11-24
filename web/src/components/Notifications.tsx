import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Menu } from "@base-ui/react/menu";
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

/** What a level means, in a sentence: what you hear about `where` (`here`, `in your projects`). */
function hear(level: string, where: string): string {
  switch (level) {
    case "watch":
      return `You hear about everything ${where}: new issues and merge requests, every comment and change.`;
    case "participating":
      return `You hear about what you take part in, are assigned to or asked to review, and your mentions, ${where}.`;
    case "mention":
      return `You hear only when someone @mentions you ${where}.`;
    default:
      return `You hear about nothing ${where}.`;
  }
}

/** Your default level, for every project you have not chosen one for. */
export function NotificationDefaults() {
  const qc = useQueryClient();
  const toast = useToast();
  const q = useQuery({ queryKey: ["notifications", "me"], queryFn: () => api.get<S["LevelDto"]>("/user/notification_settings") });
  // shown at once; put back if the server refuses
  const [picked, setPicked] = useState<string | null>(null);
  // `back`: an undo, which is not undone in turn
  const set = async (level: string, back = false) => {
    const before = picked ?? q.data?.level;
    setPicked(level);
    try {
      await api.put("/user/notification_settings", { level });
      qc.invalidateQueries({ queryKey: ["notifications"] });
      toast.ok(`${label(level)} by default`, {
        detail: `${hear(level, "in your projects")} A project's bell can say otherwise.`,
        undo: before && before !== level && !back ? () => void set(before, true) : undefined,
      });
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

/** The bell on a project (`name`): your level here (or your default). */
export function NotificationBell({ projectId, name }: { projectId: number; name: string }) {
  const qc = useQueryClient();
  const toast = useToast();
  const key = ["notifications", "project", projectId];
  const q = useQuery({ queryKey: key, queryFn: () => api.get<S["LevelDto"]>(`/projects/${projectId}/notification_settings`) });
  // `back`: an undo, which is not undone in turn
  const set = async (level: string, back = false) => {
    const before = q.data?.level;
    try {
      const r = await api.put<S["LevelDto"]>(`/projects/${projectId}/notification_settings`, { level });
      qc.setQueryData(key, r);
      toast.ok(`${name}: ${level === "global" ? `your default (${label(r.effective)})` : label(level)}`, {
        detail: hear(r.effective, "here"),
        undo: before && before !== level && !back ? () => void set(before, true) : undefined,
      });
    } catch (e) {
      toast.error(e);
    }
  };
  const effective = q.data?.effective ?? "participating";
  const Icon = effective === "watch" ? BellRing : effective === "disabled" ? BellOff : Bell;
  const current = q.data?.level ?? "global";
  const items = [{ value: "global", label: "Your default", hint: "As in your settings" }, ...LEVELS];
  return (
    <Menu.Root modal={false}>
      <Menu.Trigger className="btn small" aria-label={`Notifications: ${label(effective)}`} title="Notifications for this project">
        <Icon size={14} /> <span className="hide-narrow">{label(effective)}</span>
      </Menu.Trigger>
      <Menu.Portal>
        <Menu.Positioner className="positioner" align="end" sideOffset={6}>
          <Menu.Popup className="dropdown notif-menu">
            {items.map((l) => (
              <Menu.Item key={l.value} className="dropdown-item" onClick={() => set(l.value)}>
                <span className="notif-check">{current === l.value && <Check size={14} />}</span>
                <span>
                  <strong>{l.label}</strong>
                  <span className="muted small notif-hint">{l.hint}</span>
                </span>
              </Menu.Item>
            ))}
          </Menu.Popup>
        </Menu.Positioner>
      </Menu.Portal>
    </Menu.Root>
  );
}
