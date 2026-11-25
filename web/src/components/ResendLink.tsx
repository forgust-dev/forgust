import { useState } from "react";
import { api } from "../api/client";
import { useToast } from "../shell/toast";

/** Send the confirmation link again (`path`: for another address than the account's own). */
export function ResendLink({
  path = "/user/email/resend",
  className = "link-btn",
}: {
  path?: string;
  className?: string;
}) {
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  return (
    <button
      type="button"
      className={className}
      disabled={busy}
      onClick={async () => {
        setBusy(true);
        try {
          const r = await api.post<{ sent_to: string }>(path);
          toast.ok(`Link sent to ${r.sent_to}`);
        } catch (e) {
          toast.error(e);
        } finally {
          setBusy(false);
        }
      }}
    >
      Send the link again
    </button>
  );
}
