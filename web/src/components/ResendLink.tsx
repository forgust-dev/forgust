import { useState } from "react";
import { api } from "../api/client";
import { useToast } from "../shell/toast";

/** Send the confirmation link again. */
export function ResendLink() {
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  return (
    <button
      type="button"
      className="link-btn"
      disabled={busy}
      onClick={async () => {
        setBusy(true);
        try {
          const r = await api.post<{ sent_to: string }>("/user/email/resend");
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

