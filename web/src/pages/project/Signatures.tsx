import { useQuery } from "@tanstack/react-query";
import { api, type S } from "../../api/client";
import type { Ctx } from "./Project";

/** Signature checks per commit (unsigned commits are absent), for lists. */
export function useSignatures(ctx: Ctx, shas: string[]) {
  const q = useQuery({
    queryKey: ["signatures", ctx.p.id, shas.join(",")],
    queryFn: () => api.post<Record<string, S["Verification"]>>(`/projects/${ctx.p.id}/repository/signatures`, { shas }),
    enabled: shas.length > 0 && ctx.p.can_read_code,
    staleTime: 60_000,
  });
  return q.data ?? {};
}

export const SIGNATURE_TEXT: Record<string, string> = {
  verified: "This commit was signed with a verified key of its committer.",
  unverified: "Signed, but the committer email is not the key owner's confirmed address.",
  unknown_key: "Signed with a key no user here has added as a signing key.",
  bad_signature: "The signature does not match the commit.",
  unsupported: "Signed with an X.509 certificate, which Forgust does not check.",
};

/** "Verified" / "Unverified" next to a signed commit. */
export function SignatureBadge({ v, className }: { v?: S["Verification"]; className?: string }) {
  if (!v) return null;
  const ok = v.verification_status === "verified";
  const who = v.signer ? ` Key of @${v.signer}.` : "";
  // signed in, the hover cards show what it means (HoverCards reads `data-signature`
  // and takes the title away); signed out, there are none, and the title says it
  return (
    <span
      className={`badge ${ok ? "ok" : "warn"} sig-badge${className ? ` ${className}` : ""}`}
      data-signature={JSON.stringify(v)}
      tabIndex={0}
      title={`${v.signature_type} signature. ${SIGNATURE_TEXT[v.verification_status] ?? ""}${who}${v.key_fingerprint ? ` ${v.key_fingerprint}` : ""}`}
    >
      {ok ? "Verified" : "Unverified"}
    </span>
  );
}
