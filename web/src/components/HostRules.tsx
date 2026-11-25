import { useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type S } from "../api/client";
import { Plus } from "lucide-react";
import { ErrorBox, Field, Q } from "./ui";
import { useFocusSwap } from "./settings";

/** Credentials Renovate uses for private registries; passwords are write-only. */
export function HostRules({ base, empty: emptyText, title = "Private registries" }: { base: string; empty: string; title?: string }) {
  const qc = useQueryClient();
  const key = ["renovate-host-rules", base];
  const q = useQuery({ queryKey: key, queryFn: () => api.get<S["HostRule"][]>(base) });
  const empty = { match_host: "", host_type: "", username: "", password: "" };
  const [r, setR] = useState(empty);
  const [adding, setAdding] = useState(false);
  const button = useRef<HTMLButtonElement>(null);
  const card = useRef<HTMLDivElement>(null);
  useFocusSwap(adding, card, button);
  const [error, setError] = useState<unknown>(null);
  const refresh = () => qc.invalidateQueries({ queryKey: key });
  const close = () => {
    setAdding(false);
    setR(empty);
    setError(null);
  };
  const add = useMutation({
    mutationFn: () => api.post(base, { ...r, host_type: r.host_type || null }),
    onSuccess: () => {
      close();
      refresh();
    },
    onError: setError,
  });
  const del = useMutation({ mutationFn: (id: number) => api.del(`${base}/${id}`), onSuccess: refresh, onError: setError });
  return (
    <div className="stack" style={{ marginTop: 16 }}>
      <h3 className="small">{title}</h3>
      <ErrorBox error={error} />
      <Q q={q}>
        {(rules) => (
          <div className="list">
            {rules.length === 0 && <div className="item muted small">{emptyText}</div>}
            {rules.map((h) => (
              <div className="item" key={h.id}>
                <code className="grow">{h.match_host}</code>
                <span className="small faint">
                  {h.host_type || "any type"}
                  {h.username && ` · ${h.username}`}
                </span>
                <button type="button" className="btn small danger" aria-label={`Delete ${h.match_host}`} onClick={() => del.mutate(h.id)}>
                  Delete
                </button>
              </div>
            ))}
          </div>
        )}
      </Q>
      {/* the list first; the form once asked for */}
      {!adding ? (
        <div>
          <button ref={button} type="button" className="btn small" onClick={() => setAdding(true)}>
            <Plus size={14} /> Add a registry
          </button>
        </div>
      ) : (
      <div ref={card} className="card card-body add-form stack" role="group" aria-label="New registry">
      <div className="grid2">
        <Field label="Host">
          <input value={r.match_host} placeholder="npm.example.com" onChange={(e) => setR({ ...r, match_host: e.target.value })} />
        </Field>
        <Field label="Type" hint="docker, npm, maven, pypi… (empty: any)">
          <input value={r.host_type} onChange={(e) => setR({ ...r, host_type: e.target.value })} />
        </Field>
        <Field label="Username">
          <input value={r.username} autoComplete="off" onChange={(e) => setR({ ...r, username: e.target.value })} />
        </Field>
        <Field label="Password or token" hint="Stored encrypted, never shown again">
          <input type="password" value={r.password} autoComplete="new-password" onChange={(e) => setR({ ...r, password: e.target.value })} />
        </Field>
      </div>
      <div className="row wrap">
        <button type="button" className="btn primary" aria-busy={add.isPending} disabled={add.isPending || !r.match_host || !r.password} onClick={() => add.mutate()}>
          Add registry
        </button>
        <button type="button" className="btn" onClick={close}>
          Cancel
        </button>
      </div>
      </div>
      )}
    </div>
  );
}
