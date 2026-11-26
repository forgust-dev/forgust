import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type S } from "../api/client";
import { Plus } from "lucide-react";
import { ErrorBox, Field, PasswordInput, Q } from "./ui";
import { FormActions, FormDialog, useJustAdded } from "./FormDialog";

/** Credentials Renovate uses for private registries; passwords are write-only. */
export function HostRules({ base, empty: emptyText, title = "Private registries" }: { base: string; empty: string; title?: string }) {
  const qc = useQueryClient();
  const key = ["renovate-host-rules", base];
  const q = useQuery({ queryKey: key, queryFn: () => api.get<S["HostRule"][]>(base) });
  const empty = { match_host: "", host_type: "", username: "", password: "" };
  const [r, setR] = useState(empty);
  const [adding, setAdding] = useState(false);
  const fresh = useJustAdded();
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
      fresh.mark(r.match_host);
      close();
      refresh();
    },
    onError: setError,
  });
  const del = useMutation({ mutationFn: (id: number) => api.del(`${base}/${id}`), onSuccess: refresh, onError: setError });
  return (
    <div className="stack" style={{ marginTop: 16 }}>
      <h3 className="small">{title}</h3>
      {!adding && <ErrorBox error={error} />}
      <Q q={q}>
        {(rules) => (
          <div className="list">
            {rules.length === 0 && <div className="item muted small">{emptyText}</div>}
            {rules.map((h) => (
              <div className={"item" + fresh.fresh(h.match_host)} key={h.id}>
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
      <div>
        <button type="button" className="btn small" onClick={() => setAdding(true)}>
          <Plus size={14} /> Add a registry
        </button>
      </div>
      <FormDialog open={adding} onClose={close} title="Add a registry">
      <form
        className="stack"
        onSubmit={(e) => {
          // in React's tree the window sits inside the dependency updates form: this one is its own
          e.preventDefault();
          e.stopPropagation();
          add.mutate();
        }}
      >
      <ErrorBox error={error} />
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
          <PasswordInput value={r.password} autoComplete="new-password" onChange={(e) => setR({ ...r, password: e.target.value })} />
        </Field>
      </div>
      <FormActions label="Add registry" busy={add.isPending} disabled={!r.match_host || !r.password} onCancel={close} />
      </form>
      </FormDialog>
    </div>
  );
}
