import { useState, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, timeAgo, type S } from "../api/client";
import { ErrorBox, Field, levelName, Q, StateBadge } from "./ui";
import { Select } from "./controls";
import { AddActions, EmptyRow, ListSection } from "./settings";
import { CircleCheck, CircleX, Clock } from "lucide-react";

const EVENTS = ["push", "merge_request", "issue", "confidential_issue", "note", "confidential_note", "release", "pipeline", "build"];

/** What a hook posts: the JSON payload, or one message for a chat app's incoming webhook. */
const FORMATS = [
  { value: "generic", label: "JSON payload (GitLab's shapes)" },
  { value: "slack", label: "Slack" },
  { value: "mattermost", label: "Mattermost" },
  { value: "discord", label: "Discord" },
  { value: "matrix", label: "Matrix (hookshot)" },
  { value: "teams", label: "Microsoft Teams (workflow)" },
];
const formatName = (f: string) => FORMATS.find((x) => x.value === f)?.label.split(" (")[0] ?? f;

/** Webhooks of a project (`/projects/:id`) or group (`/groups/:id`). */
export function Webhooks({ base, title, description }: { base: string; title: string; description: ReactNode }) {
  const qc = useQueryClient();
  const key = ["hooks", base];
  const list = useQuery({ queryKey: key, queryFn: () => api.get<S["Webhook"][]>(`${base}/hooks`) });
  const [url, setUrl] = useState("");
  const [secret, setSecret] = useState("");
  const [events, setEvents] = useState<string[]>(["push", "merge_request"]);
  const [format, setFormat] = useState("generic");
  const [open, setOpen] = useState<number | null>(null);
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const close = () => {
    setAdding(false);
    setUrl("");
    setSecret("");
    setError(null);
  };
  const add = useMutation({
    mutationFn: () => api.post(`${base}/hooks`, { url, secret, events, format }),
    onSuccess: () => {
      close();
      qc.invalidateQueries({ queryKey: key });
    },
    onError: setError,
  });
  const del = useMutation({ mutationFn: (id: number) => api.del(`${base}/hooks/${id}`), onSuccess: () => qc.invalidateQueries({ queryKey: key }) });
  return (
    <ListSection
      id="webhooks"
      title={title}
      description={description}
      add="Add a webhook"
      adding={adding}
      onAdd={() => setAdding(true)}
      form={
      <form
        className="stack"
        aria-label="New webhook"
        onSubmit={(e) => {
          e.preventDefault();
          add.mutate();
        }}
      >
        <ErrorBox error={error} />
        <div className="grid2">
          <Field label="URL" hint="Private and loopback addresses are refused">
            <input placeholder="https://ci.example.com/hooks/forgust" value={url} onChange={(e) => setUrl(e.target.value)} />
          </Field>
          <Field label="Secret" hint="Signs each delivery: X-Forgust-Signature: sha256=HMAC(secret, body)">
            <input type="password" value={secret} onChange={(e) => setSecret(e.target.value)} />
          </Field>
        </div>
        <Field
          label="Posts"
          hint={
            format === "generic"
              ? "The event's JSON payload, as GitLab sends it."
              : "One short message per event, in the shape of the app's incoming webhook. Not sent: pipelines starting, jobs passing, merge request updates."
          }
        >
          <Select label="Posts" value={format} onChange={setFormat} options={FORMATS} />
        </Field>
        <div className="row wrap" style={{ gap: 16 }}>
          {EVENTS.map((ev) => (
            <label key={ev} className="check">
              <input type="checkbox" checked={events.includes(ev)} onChange={(e) => setEvents(e.target.checked ? [...events, ev] : events.filter((x) => x !== ev))} />
              <code>{ev}</code>
            </label>
          ))}
        </div>
        <AddActions label="Add webhook" disabled={!url || events.length === 0} busy={add.isPending} onCancel={close} />
      </form>
      }
    >
      <Q q={list}>
        {(hooks) =>
          hooks.length === 0 ? (
            <EmptyRow>No webhooks.</EmptyRow>
          ) : (
            <div className="list">
              {hooks.map((h) => (
                <div key={h.id} className="list-group">
                  <div className="item">
                    <div className="grow" style={{ minWidth: 0 }}>
                      <code className="ellipsis" style={{ display: "block" }} title={h.url}>
                        {h.url}
                      </code>
                      <div className="small muted">
                        {h.format !== "generic" && `${formatName(h.format)} · `}
                        called on {h.events.join(", ")}
                      </div>
                    </div>
                    <div className="row" style={{ flex: "none", gap: 6 }}>
                      <button className="btn small" onClick={() => setOpen(open === h.id ? null : h.id)}>
                        Deliveries
                      </button>
                      <button className="btn small danger" onClick={() => del.mutate(h.id)}>
                        Remove
                      </button>
                    </div>
                  </div>
                  {open === h.id && <Deliveries base={base} id={h.id} />}
                </div>
              ))}
            </div>
          )
        }
      </Q>
    </ListSection>
  );
}

function Deliveries({ base, id }: { base: string; id: number }) {
  const q = useQuery({ queryKey: ["deliveries", base, id], queryFn: () => api.get<S["Delivery"][]>(`${base}/hooks/${id}/deliveries`) });
  return (
    <div style={{ padding: "0 14px 10px" }}>
      <Q q={q}>
        {(list) =>
          list.length === 0 ? (
            <p className="muted small">No deliveries yet.</p>
          ) : (
            <table className="small" style={{ width: "100%", borderCollapse: "collapse" }}>
              <tbody>
                {list.map((d) => (
                  <tr key={d.id}>
                    <td>
                      {d.status === "delivered" ? (
                        <StateBadge tone="ok" icon={CircleCheck}>
                          Delivered
                        </StateBadge>
                      ) : d.status === "failed" ? (
                        <StateBadge tone="danger" icon={CircleX}>
                          Failed
                        </StateBadge>
                      ) : (
                        <StateBadge tone="neutral" icon={Clock}>
                          {d.status[0].toUpperCase() + d.status.slice(1)}
                        </StateBadge>
                      )}
                    </td>
                    <td>
                      <code>{d.event}</code>
                    </td>
                    <td>{d.response_code ?? "—"}</td>
                    <td className="muted">{d.error ?? ""}</td>
                    <td className="faint">
                      {d.attempts} attempt(s) · {d.duration_ms ?? 0} ms · {timeAgo(d.created_at)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )
        }
      </Q>
    </div>
  );
}

/** Approval rules: N approvals from users with at least a role. */
export function ApprovalRules({ projectId }: { projectId: number }) {
  const qc = useQueryClient();
  const key = ["rules", projectId];
  const list = useQuery({ queryKey: key, queryFn: () => api.get<S["ApprovalRule"][]>(`/projects/${projectId}/approval_rules`) });
  const [name, setName] = useState("Maintainers");
  const [required, setRequired] = useState(1);
  const [level, setLevel] = useState("maintainer");
  const [error, setError] = useState<unknown>(null);
  const [adding, setAdding] = useState(false);
  const close = () => {
    setAdding(false);
    setError(null);
  };
  const save = useMutation({
    mutationFn: () => api.post(`/projects/${projectId}/approval_rules`, { name, approvals_required: required, min_access_level: level }),
    onSuccess: () => {
      close();
      qc.invalidateQueries({ queryKey: key });
    },
    onError: setError,
  });
  const del = useMutation({ mutationFn: (id: number) => api.del(`/projects/${projectId}/approval_rules/${id}`), onSuccess: () => qc.invalidateQueries({ queryKey: key }) });
  const names: Record<number, string> = { 20: "reporter", 30: "developer", 40: "maintainer", 50: "owner" };
  return (
    <ListSection
      id="approvals"
      title="Merge request approvals"
      description="How many approvals a merge request needs, and from whom, before it can be merged."
      add="Add a rule"
      adding={adding}
      onAdd={() => setAdding(true)}
      form={
      <form
        className="stack"
        aria-label="New approval rule"
        onSubmit={(e) => {
          e.preventDefault();
          save.mutate();
        }}
      >
        <ErrorBox error={error} />
        <div className="row wrap">
        <input style={{ width: 200 }} value={name} onChange={(e) => setName(e.target.value)} aria-label="Rule name" />
        <input style={{ width: 80 }} type="number" min={1} max={20} value={required} onChange={(e) => setRequired(Number(e.target.value))} aria-label="Approvals required" />
        <span className="muted">approvals from</span>
        <Select
          style={{ width: 200 }}
          value={level}
          onChange={setLevel}
          label="Minimum role"
          options={["reporter", "developer", "maintainer", "owner"].map((l) => ({ value: l, label: `${levelName(l)}s and above` }))}
        />
        </div>
        <AddActions label="Save rule" busy={save.isPending} onCancel={close} />
      </form>
      }
    >
      <Q q={list}>
        {(rules) =>
          rules.length === 0 ? (
            <EmptyRow>No approval rules: merge requests can be merged without approvals.</EmptyRow>
          ) : (
            <div className="list">
              {rules.map((r) => (
                <div className="item" key={r.id}>
                  <strong className="grow">{r.name}</strong>
                  <span className="small muted">
                    {r.approvals_required} approval{r.approvals_required === 1 ? "" : "s"} from {levelName(names[r.min_access_level] ?? "maintainer")}s and above
                  </span>
                  <button className="btn small danger" onClick={() => del.mutate(r.id)}>
                    Remove
                  </button>
                </div>
              ))}
            </div>
          )
        }
      </Q>
    </ListSection>
  );
}
