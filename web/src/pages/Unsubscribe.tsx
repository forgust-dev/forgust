import { useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { BellOff } from "lucide-react";
import { api, ApiError, type S } from "../api/client";
import { ErrorBox, Q } from "../components/ui";

/** From an email's footer: stop one project's emails, without signing in. */
export function UnsubscribePage() {
  const [params] = useSearchParams();
  // only ids and a URL-safe signature: nothing that could point the request elsewhere
  const [u, p, sig] = [params.get("u") ?? "", params.get("p") ?? "", params.get("s") ?? ""];
  const valid = /^\d+$/.test(u) && /^\d+$/.test(p) && /^[A-Za-z0-9_-]+$/.test(sig);
  const path = `/unsubscribe/${encodeURIComponent(u)}/${encodeURIComponent(p)}/${encodeURIComponent(sig)}`;
  const q = useQuery({ queryKey: ["unsubscribe", path], queryFn: () => api.get<S["Unsubscription"]>(path), retry: false, enabled: valid });
  const [done, setDone] = useState(false);
  const [error, setError] = useState<unknown>(null);
  return (
    <div className="page narrow">
      <div className="card stack" style={{ padding: 24, maxWidth: 520, margin: "40px auto" }}>
        <h1 style={{ margin: 0, fontSize: "1.3rem" }}>
          <BellOff size={18} /> Stop emails
        </h1>
        {!valid || (q.error instanceof ApiError && q.error.status === 404) ? (
          <p>This link is not valid any more.</p>
        ) : (
          <Q q={q}>
            {(u) =>
              done || u.level === "disabled" ? (
                <p>
                  You will not get emails about <strong>{u.project}</strong> any more. You can change this in your{" "}
                  <Link to="/profile/notifications">notification settings</Link> or with the bell on the project's page.
                </p>
              ) : (
                <>
                  <p>
                    Stop all emails about <strong>{u.project}</strong>? Other projects are not affected.
                  </p>
                  <ErrorBox error={error} />
                  <div>
                    <button
                      className="btn primary"
                      onClick={async () => {
                        try {
                          await api.post(path);
                          setDone(true);
                        } catch (e) {
                          setError(e);
                        }
                      }}
                    >
                      Stop emails about {u.project}
                    </button>
                  </div>
                </>
              )
            }
          </Q>
        )}
      </div>
    </div>
  );
}
