import { AuditEvents } from "../../components/AuditEvents";
import { ListHead } from "../../components/ui";
import type { Ctx } from "./Project";

/** Who changed what in a project, and when (Maintainers and above). */
export function ProjectAuditLog({ ctx }: { ctx: Ctx }) {
  return (
    <div className="stack">
      <ListHead title="Audit log" sub="Who changed what in this project, and when." />
      {/* the project endpoint returns 100 events a call */}
      <AuditEvents path={`/projects/${ctx.p.id}/audit`} perPage={100} />
    </div>
  );
}
