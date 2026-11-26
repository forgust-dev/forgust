import { useState, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, LEVELS, timeAgo, type S } from "../api/client";
import { Badge, ErrorBox, Field, LevelSelect, NewSecret, Q, StateBadge } from "./ui";
import { EmptyRow, ListSection } from "./settings";
import { FormActions, useJustAdded } from "./FormDialog";
import { Ban } from "lucide-react";

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

export function AccessTokens({ base, title, description }: { base: string; title: string; description: ReactNode }) {
  const [name, setName] = useState("");
  const [level, setLevel] = useState("developer");
  const [scopes, setScopes] = useState<string[]>(["read_repository"]);
  const [created, setCreated] = useState<S["NewToken"] | null>(null);
  const [adding, setAdding] = useState(false);
  const fresh = useJustAdded();
  // the new token is shown once: in the window, in place of its form, until Done
  const { list, add: create, remove: revoke, error, setError } = useCrudList<S["AccessTokenDto"], S["NewToken"]>(["access-tokens", base], `${base}/access_tokens`, (t) => {
    setCreated(t);
  });
  const close = () => {
    // the row came in behind the window: highlighted once it is in view
    if (created) fresh.mark(created.username ?? "");
    setAdding(false);
    setCreated(null);
    setName("");
    setError(null);
  };
  return (
    <ListSection
      id="access-tokens"
      title={title}
      description={description}
      add="Create a token"
      result={created && <NewSecret token={created.token} username={created.username} />}
      resultTitle="Token created"
      adding={adding}
      onClose={close}
      onAdd={() => {
        setCreated(null);
        setAdding(true);
      }}
      form={
        <form
          className="stack"
          aria-label="New access token"
          onSubmit={(e) => {
            e.preventDefault();
            create.mutate({ name, access_level: level, scopes });
          }}
        >
          <ErrorBox error={error} />
          <div className="grid2">
            <Field label="Name">
              <input value={name} onChange={(e) => setName(e.target.value)} />
            </Field>
            <Field label="Role">
              <LevelSelect value={level} onChange={setLevel} levels={LEVELS.slice(0, 4)} />
            </Field>
          </div>
          <ScopePicker scopes={scopes} onChange={setScopes} />
          <FormActions label="Create token" disabled={!name} busy={create.isPending} onCancel={close} />
        </form>
      }
    >
      {!adding && <ErrorBox error={error} />}
      <Q q={list}>
        {(tokens) =>
          tokens.length === 0 ? (
            <EmptyRow>No access tokens.</EmptyRow>
          ) : (
            <div className="list">
              {tokens.map((t) => (
                <div className={"item" + fresh.fresh(t.bot_username)} key={t.bot_user_id + t.name}>
                  <div className="grow">
                    <strong>{t.name}</strong> <span className="faint small">@{t.bot_username}</span>
                    <div className="small muted">{t.scopes.join(", ")}</div>
                  </div>
                  <Badge>{t.access_level}</Badge>
                  {t.revoked ? (
                    <StateBadge tone="neutral" icon={Ban}>
                      Revoked
                    </StateBadge>
                  ) : <span className="faint small">{t.last_used_at ? `used ${timeAgo(t.last_used_at)}` : "never used"}</span>}
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
    </ListSection>
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

export function DeployTokens({ base, title, description }: { base: string; title: string; description: ReactNode }) {
  const [name, setName] = useState("");
  const [scopes, setScopes] = useState<string[]>(["read_repository"]);
  const [created, setCreated] = useState<S["NewToken"] | null>(null);
  const [adding, setAdding] = useState(false);
  const fresh = useJustAdded();
  // the new token is shown once: in the window, in place of its form, until Done
  const { list, add: create, remove: revoke, error, setError } = useCrudList<S["DeployToken"], S["NewToken"]>(["deploy-tokens", base], `${base}/deploy_tokens`, (t) => {
    setCreated(t);
  });
  const close = () => {
    // the row came in behind the window: highlighted once it is in view
    if (created) fresh.mark(created.id);
    setAdding(false);
    setCreated(null);
    setName("");
    setError(null);
  };
  return (
    <ListSection
      id="deploy-tokens"
      title={title}
      description={description}
      add="Create a deploy token"
      result={created && <NewSecret token={created.token} username={created.username} />}
      resultTitle="Deploy token created"
      adding={adding}
      onClose={close}
      onAdd={() => {
        setCreated(null);
        setAdding(true);
      }}
      form={
        <form
          className="stack"
          aria-label="New deploy token"
          onSubmit={(e) => {
            e.preventDefault();
            create.mutate({ name, scopes });
          }}
        >
          <ErrorBox error={error} />
          <Field label="Name">
            <input value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <ScopePicker scopes={scopes} onChange={setScopes} options={["read_repository", "write_repository"]} />
          <FormActions label="Create deploy token" disabled={!name} busy={create.isPending} onCancel={close} />
        </form>
      }
    >
      {!adding && <ErrorBox error={error} />}
      <Q q={list}>
        {(tokens) =>
          tokens.length === 0 ? (
            <EmptyRow>No deploy tokens.</EmptyRow>
          ) : (
            <div className="list">
              {tokens.map((t) => (
                <div className={"item" + fresh.fresh(t.id)} key={t.id}>
                  <div className="grow">
                    <strong>{t.name}</strong> <code className="small">{t.username}</code>
                    <div className="small muted">
                      {t.scopes.includes("write_repository") ? "Can clone and push" : "Can clone"} ·{" "}
                      {t.expires_at ? `expires ${new Date(t.expires_at).toLocaleDateString([], { day: "numeric", month: "short", year: "numeric" })}` : "never expires"}
                    </div>
                  </div>
                  {t.revoked ? (
                    <StateBadge tone="neutral" icon={Ban}>
                      Revoked
                    </StateBadge>
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
    </ListSection>
  );
}
