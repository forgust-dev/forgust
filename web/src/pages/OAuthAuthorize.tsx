import { useState } from "react";
import { useSearchParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { api, type S } from "../api/client";
import { useSession } from "../session";
import { ErrorBox, Q } from "../components/ui";

// ADR-0025: an MCP client (an AI assistant) asks to act as you. The
// server checked the client and its redirect address; you allow reading, and
// optionally changes, or deny.

const FIELDS = ["client_id", "redirect_uri", "response_type", "code_challenge", "code_challenge_method", "scope"] as const;

export function OAuthAuthorize() {
  const [params] = useSearchParams();
  const { user } = useSession();
  const request = Object.fromEntries(FIELDS.map((k) => [k, params.get(k) ?? ""])) as Record<(typeof FIELDS)[number], string>;
  const info = useQuery({
    queryKey: ["oauth-authorize", params.toString()],
    queryFn: () => api.get<S["AuthorizeInfo"]>(`/oauth/authorize?${new URLSearchParams(request)}`),
  });
  const [write, setWrite] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const answer = async (allow: boolean) => {
    setBusy(true);
    try {
      const r = await api.post<S["AuthorizeRedirect"]>("/oauth/authorize", { ...request, state: params.get("state"), allow, write: allow && (write ?? false) });
      window.location.assign(r.redirect);
    } catch (e) {
      setError(e);
      setBusy(false);
    }
  };
  return (
    <div className="page narrow auth-page">
      <img src="/favicon.svg" width={36} height={36} alt="" className="auth-logo" />
      <Q q={info}>
        {(i) => (
          <div className="stack">
            <h1>Allow {i.client_name}?</h1>
            <div className="alert warn">
              An application calling itself <strong>{i.client_name}</strong> (the name is its own claim, not checked by Forgust) sends you back to{" "}
              <strong>
                <code>{i.redirect_to}</code>
              </strong>{" "}
              afterwards. Continue only if you started this from that application.
            </div>
            <p>
              It wants to use Forgust as <strong>@{user?.username}</strong>: read the projects, code, issues, merge requests and pipelines you can see.
            </p>
            <label className="check">
              {/* changes are opt-in, whatever the application asked for */}
              <input type="checkbox" checked={write ?? false} onChange={(e) => setWrite(e.target.checked)} />
              <span>
                Also let it make changes as you{i.write_requested ? " (it asks for this)" : ""}{" "}
                <span className="muted small">— comment, open and update issues and merge requests, push commits, run pipelines (through MCP only)</span>
              </span>
            </label>
            <p className="muted small" style={{ margin: 0 }}>
              It gets a token listed under Profile → Access tokens; revoke it there to end its access. Only allow assistants you trust: what it reads may
              include private code.
            </p>
            <ErrorBox error={error} />
            <div className="row" style={{ gap: 8 }}>
              <button className="btn primary" disabled={busy} onClick={() => answer(true)}>
                Allow
              </button>
              <button className="btn" disabled={busy} onClick={() => answer(false)}>
                Deny
              </button>
            </div>
          </div>
        )}
      </Q>
    </div>
  );
}
