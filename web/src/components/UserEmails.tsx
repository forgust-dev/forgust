import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CircleCheck, Clock } from "lucide-react";
import { api, timeAgo, type S } from "../api/client";
import { useSession } from "../session";
import { useAct } from "../shell/toast";
import { ResendLink } from "./ResendLink";
import { useCrudList } from "./Tokens";
import { ErrorBox, Field, PasswordInput, Q, StateBadge } from "./ui";
import { EmptyRow, ListSection } from "./settings";
import { FormActions, useJustAdded } from "./FormDialog";

/** How an address got confirmed, for its state's tooltip. */
const CONFIRMED_BY: Record<string, string> = {
  link: "confirmed from its inbox",
  admin: "confirmed by an administrator",
  auto: "confirmed as it was added (this instance trusts its people's addresses)",
};

/** An address's state: confirmed (trusted, when the instance confirmed it as it was added), or
 *  waiting to be. */
function State({ e }: { e: S["UserEmail"] }) {
  return e.confirmed_at ? (
    <StateBadge tone="ok" icon={CircleCheck} title={CONFIRMED_BY[e.confirmed_by ?? ""]}>
      {e.confirmed_by === "auto" ? "Trusted" : "Confirmed"}
    </StateBadge>
  ) : (
    <StateBadge tone="warn" icon={Clock}>
      Not confirmed
    </StateBadge>
  );
}

/** Your other addresses (Your settings > Profile), a section of their own: commits made with a
 *  confirmed one show as yours. */
export function OtherEmails() {
  const { emailEnabled } = useSession();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [msg, setMsg] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const fresh = useJustAdded();
  const close = () => {
    setAdding(false);
    setEmail("");
    setPassword("");
    setError(null);
  };
  const { list, add, remove, error, setError } = useCrudList<
    S["UserEmail"],
    S["UserEmail"]
  >(["user-emails"], "/user/emails", (row) => {
    fresh.mark(row.id);
    close();
    // what happens next, under the list
    setMsg(
      row.confirmed_at
        ? `${row.email} is confirmed.`
        : emailEnabled
          ? `We sent a confirmation link to ${row.email}.`
          : `${row.email} is added: an administrator confirms it (this instance sends no email).`,
    );
  });
  return (
    <ListSection
      title="Other email addresses"
      description="Commits made with a confirmed one show as yours, and imported contributions under it become yours. Emails still go to your address above."
      add="Add an address"
      adding={adding}
      onAdd={() => {
        setMsg(null);
        setAdding(true);
      }}
      onClose={close}
      form={
        <form
          className="stack"
          onSubmit={(e) => {
            e.preventDefault();
            add.mutate({ email, current_password: password });
          }}
        >
          <ErrorBox error={error} />
          <Field
            label="Email address"
            hint={
              emailEnabled
                ? "A link is sent to it: the address counts once you open it."
                : "This instance sends no email: an administrator confirms it, unless the instance trusts the addresses you add."
            }
          >
            <input
              type="email"
              autoComplete="email"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              placeholder="you@work.example"
            />
          </Field>
          {!emailEnabled && (
            <Field
              label="Current password"
              hint="Asked when the address is confirmed as you add it (single sign-on accounts: leave empty within 10 minutes of signing in)"
            >
              <PasswordInput
                autoComplete="current-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </Field>
          )}
          <FormActions label="Add address" disabled={!email.trim()} busy={add.isPending} onCancel={close} />
        </form>
      }
    >
      {msg && <div className="alert ok">{msg}</div>}
      {!adding && <ErrorBox error={error} />}
      <Q q={list}>
        {(rows) =>
          rows.length === 0 ? (
            <EmptyRow>No other addresses yet.</EmptyRow>
          ) : (
            <div className="list">
              {rows.map((e) => (
                <div className={"item" + fresh.fresh(e.id)} key={e.id}>
                  <div className="grow" style={{ minWidth: 0 }}>
                    <strong className="ellipsis">{e.email}</strong>
                  </div>
                  <State e={e} />
                  <span className="small faint nowrap hide-narrow">
                    added {timeAgo(e.created_at)}
                  </span>
                  {!e.confirmed_at && emailEnabled && (
                    <ResendLink
                      path={`/user/emails/${e.id}/resend`}
                      className="btn small"
                    />
                  )}
                  <button
                    type="button"
                    className="btn small danger"
                    onClick={() => remove.mutate(e.id)}
                  >
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

/** Someone's other addresses, in the administrator's dialog: confirm one you know is theirs, or remove it. */
export function AdminUserEmails({ user }: { user: S["AdminUser"] }) {
  const qc = useQueryClient();
  const key = ["admin-user-emails", user.id];
  const path = `/admin/users/${user.id}/emails`;
  const list = useQuery({
    queryKey: key,
    queryFn: () => api.get<S["UserEmail"][]>(path),
  });
  // the users list counts what waits for confirmation
  const act = useAct(() => {
    qc.invalidateQueries({ queryKey: key });
    qc.invalidateQueries({ queryKey: ["admin-users"] });
  });
  return (
    <div className="stack">
      <p style={{ margin: 0 }}>
        Commits, imported contributions and signed commits made with a confirmed
        one count as {user.username}'s, and code owners listed by it. Confirm
        only an address you know is theirs.
      </p>
      <Q q={list}>
        {(rows) =>
          rows.length === 0 ? (
            <p className="muted small" style={{ margin: 0 }}>
              No other addresses.
            </p>
          ) : (
            <div className="list">
              {rows.map((e) => (
                <div className="item" key={e.id}>
                  <div className="grow ellipsis" style={{ minWidth: 0 }}>
                    {e.email}
                  </div>
                  <State e={e} />
                  {!e.confirmed_at && (
                    <button
                      type="button"
                      className="btn small"
                      onClick={() =>
                        act(
                          () => api.post(`${path}/${e.id}/confirm`),
                          `${e.email} confirmed`,
                        )
                      }
                    >
                      Confirm
                    </button>
                  )}
                  <button
                    type="button"
                    className="btn small danger"
                    onClick={() =>
                      act(
                        () => api.del(`${path}/${e.id}`),
                        `${e.email} removed`,
                      )
                    }
                  >
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
