import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, LEVELS, timeAgo, type S } from "../api/client";
import { Badge, Card, ErrorBox, Field, LevelSelect, NewSecret, Q } from "./ui";

const SCOPES = ["api", "read_api", "read_repository", "write_repository", "mcp"];

/** Project or group access tokens (bot users, ADR-0015). */
/**
 * A list behind `url`, with its create (POST `url`) and delete (DELETE
 * `url/id`): both refresh the list, and either one's error shows in `error`.
 */
export function useCrudList<T, N = unknown>(queryKey: unknown[], url: string, onAdded: (n: N) => void) {
  const qc = useQueryClient();
  const list = useQuery({ queryKey, queryFn: () => api.get<T[]>(url) });
  const [error, setError] = useState<unknown>(null);
  const refresh = () => qc.invalidateQueries({ queryKey });
  const add = useMutation({
    mutationFn: (body: object) => api.post<N>(url, body),
    onSuccess: (n) => {
      onAdded(n);
      refresh();
    },
    onError: setError,
  });
  const remove = useMutation({ mutationFn: (id: number) => api.del(`${url}/${id}`), onSuccess: refresh, onError: setError });
  return { list, add, remove, error, setError };
}

export function AccessTokens({ base }: { base: string }) {
  const [name, setName] = useState("");
  const [level, setLevel] = useState("developer");
  const [scopes, setScopes] = useState<string[]>(["read_repository"]);
  const [created, setCreated] = useState<S["NewToken"] | null>(null);
  const { list, add: create, remove: revoke, error } = useCrudList<S["AccessTokenDto"], S["NewToken"]>(["access-tokens", base], `${base}/access_tokens`, (t) => {
    setCreated(t);
    setName("");
  });
  return (
    <div className="stack">
      <Card title="New access token">
        <form
          className="stack"
          onSubmit={(e) => {
            e.preventDefault();
            create.mutate({ name, access_level: level, scopes });
          }}
        >
          <ErrorBox error={error} />
          {created && <NewSecret token={created.token} username={created.username} />}
          <div className="grid2">
            <Field label="Name">
              <input value={name} onChange={(e) => setName(e.target.value)} />
            </Field>
            <Field label="Role">
              <LevelSelect value={level} onChange={setLevel} levels={LEVELS.slice(0, 4)} />
            </Field>
          </div>
          <ScopePicker scopes={scopes} onChange={setScopes} />
          <div>
            <button className="btn primary" disabled={!name}>
              Create token
            </button>
          </div>
        </form>
      </Card>
      <Q q={list}>
        {(tokens) =>
          tokens.length === 0 ? (
            <p className="muted">No access tokens.</p>
          ) : (
            <div className="list">
              {tokens.map((t) => (
                <div className="item" key={t.bot_user_id + t.name}>
                  <div className="grow">
                    <strong>{t.name}</strong> <span className="faint small">@{t.bot_username}</span>
                    <div className="small muted">{t.scopes.join(", ")}</div>
                  </div>
                  <Badge tone="accent">{t.access_level}</Badge>
                  {t.revoked ? <Badge>revoked</Badge> : <span className="faint small">{t.last_used_at ? `used ${timeAgo(t.last_used_at)}` : "never used"}</span>}
                  {!t.revoked && (
                    <button className="btn small danger" onClick={() => revoke.mutate(t.bot_user_id)}>
                      Revoke
                    </button>
                  )}
                </div>
              ))}
            </div>
          )
        }
      </Q>
    </div>
  );
}

export function ScopePicker({ scopes, onChange, options = SCOPES }: { scopes: string[]; onChange: (s: string[]) => void; options?: string[] }) {
  return (
    <div className="row wrap" style={{ gap: 16 }}>
      {options.map((s) => (
        <label key={s} className="check">
          <input type="checkbox" checked={scopes.includes(s)} onChange={(e) => onChange(e.target.checked ? [...scopes, s] : scopes.filter((x) => x !== s))} />
          <code>{s}</code>
        </label>
      ))}
    </div>
  );
}

export function DeployTokens({ base }: { base: string }) {
  const [name, setName] = useState("");
  const [scopes, setScopes] = useState<string[]>(["read_repository"]);
  const [created, setCreated] = useState<S["NewToken"] | null>(null);
  const { list, add: create, remove: revoke, error } = useCrudList<S["DeployToken"], S["NewToken"]>(["deploy-tokens", base], `${base}/deploy_tokens`, (t) => {
    setCreated(t);
    setName("");
  });
  return (
    <div className="stack">
      <Card title="New deploy token">
        <form
          className="stack"
          onSubmit={(e) => {
            e.preventDefault();
            create.mutate({ name, scopes });
          }}
        >
          <ErrorBox error={error} />
          {created && <NewSecret token={created.token} username={created.username} />}
          <Field label="Name">
            <input value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <ScopePicker scopes={scopes} onChange={setScopes} options={["read_repository", "write_repository"]} />
          <div>
            <button className="btn primary" disabled={!name}>
              Create deploy token
            </button>
          </div>
        </form>
      </Card>
      <Q q={list}>
        {(tokens) =>
          tokens.length === 0 ? (
            <p className="muted">No deploy tokens.</p>
          ) : (
            <div className="list">
              {tokens.map((t) => (
                <div className="item" key={t.id}>
                  <div className="grow">
                    <strong>{t.name}</strong> <code className="small">{t.username}</code>
                    <div className="small muted">{t.scopes.join(", ")}</div>
                  </div>
                  {t.revoked ? (
                    <Badge>revoked</Badge>
                  ) : (
                    <button className="btn small danger" onClick={() => revoke.mutate(t.id)}>
                      Revoke
                    </button>
                  )}
                </div>
              ))}
            </div>
          )
        }
      </Q>
    </div>
  );
}
