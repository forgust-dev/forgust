import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, timeAgo, type S } from "../api/client";
import { Badge, ErrorBox, Field, levelName, Q } from "./ui";
import { Select } from "./controls";

const EVENTS = ["push", "merge_request", "note", "release", "pipeline", "build"];

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
export function Webhooks({ base }: { base: string }) {
  const qc = useQueryClient();
  const key = ["hooks", base];
  const list = useQuery({ queryKey: key, queryFn: () => api.get<S["Webhook"][]>(`${base}/hooks`) });
  const [url, setUrl] = useState("");
  const [secret, setSecret] = useState("");
  const [events, setEvents] = useState<string[]>(["push", "merge_request"]);
  const [format, setFormat] = useState("generic");
  const [open, setOpen] = useState<number | null>(null);
  const [error, setError] = useState<unknown>(null);
  const add = useMutation({
    mutationFn: () => api.post(`${base}/hooks`, { url, secret, events, format }),
    onSuccess: () => {
      setUrl("");
      setSecret("");
      setError(null);
      qc.invalidateQueries({ queryKey: key });
    },
    onError: setError,
  });
  const del = useMutation({ mutationFn: (id: number) => api.del(`${base}/hooks/${id}`), onSuccess: () => qc.invalidateQueries({ queryKey: key }) });
  return (
    <div className="stack">
      <ErrorBox error={error} />
      <form
        className="stack"
        onSubmit={(e) => {
          e.preventDefault();
          add.mutate();
        }}
      >
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
          <div className="spacer" />
          <button className="btn primary" disabled={!url || events.length === 0}>
            Add webhook
          </button>
        </div>
      </form>
      <Q q={list}>
        {(hooks) =>
          hooks.length === 0 ? (
            <p className="muted">No webhooks.</p>
          ) : (
            <div className="list">
              {hooks.map((h) => (
                <div key={h.id} className="list-group">
                  <div className="item">
                    <code className="grow ellipsis">{h.url}</code>
                    {h.format !== "generic" && <Badge tone="accent">{formatName(h.format)}</Badge>}
                    {h.events.map((e) => (
                      <Badge key={e}>{e}</Badge>
                    ))}
                    <button className="btn small" onClick={() => setOpen(open === h.id ? null : h.id)}>
                      Deliveries
                    </button>
                    <button className="btn small danger" onClick={() => del.mutate(h.id)}>
                      Remove
                    </button>
                  </div>
                  {open === h.id && <Deliveries base={base} id={h.id} />}
                </div>
              ))}
            </div>
          )
        }
      </Q>
    </div>
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
                      <Badge tone={d.status === "delivered" ? "ok" : d.status === "failed" ? "warn" : undefined}>{d.status}</Badge>
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
  const save = useMutation({
    mutationFn: () => api.post(`/projects/${projectId}/approval_rules`, { name, approvals_required: required, min_access_level: level }),
    onSuccess: () => qc.invalidateQueries({ queryKey: key }),
    onError: setError,
  });
  const del = useMutation({ mutationFn: (id: number) => api.del(`/projects/${projectId}/approval_rules/${id}`), onSuccess: () => qc.invalidateQueries({ queryKey: key }) });
  const names: Record<number, string> = { 20: "reporter", 30: "developer", 40: "maintainer", 50: "owner" };
  return (
    <div className="stack">
      <ErrorBox error={error} />
      <form
        className="row wrap"
        onSubmit={(e) => {
          e.preventDefault();
          save.mutate();
        }}
      >
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
        <button className="btn primary">Save rule</button>
      </form>
      <Q q={list}>
        {(rules) =>
          rules.length === 0 ? (
            <p className="muted">No approval rules: merge requests can be merged without approvals.</p>
          ) : (
            <div className="list">
              {rules.map((r) => (
                <div className="item" key={r.id}>
                  <strong className="grow">{r.name}</strong>
                  <span>
                    {r.approvals_required} from {names[r.min_access_level]}s
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
    </div>
  );
}
