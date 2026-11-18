import type { CSSProperties, ReactNode } from "react";
import { CheckCircle2, Circle, CircleAlert, XCircle } from "lucide-react";

/** How a check or an item stands: passes, needs attention, blocks, or just is. */
export type Tone = "ok" | "warn" | "bad" | "none";

/**
 * How much of something is done, as a ring around the count: a merge request's
 * checks, a milestone's issues. `mini` is the small one of a list row, on a light
 * surface, without the count inside.
 */
export function Ring({ done, total, label, title, mini }: { done: number; total: number; label?: string; title?: string; mini?: boolean }) {
  const size = mini ? 28 : 56;
  const stroke = mini ? 4 : 6;
  const r = mini ? 10 : 22;
  const c = size / 2;
  const around = 2 * Math.PI * r;
  const filled = total === 0 ? 0 : (Math.min(done, total) / total) * around;
  return (
    <svg className={`ready-ring${mini ? " mini" : ""}`} width={size} height={size} viewBox={`0 0 ${size} ${size}`} role="img" aria-label={title ?? `${done} of ${total}`}>
      <circle className="track" cx={c} cy={c} r={r} fill="none" strokeWidth={stroke} />
      {filled > 0 && (
        <circle
          className="done"
          cx={c}
          cy={c}
          r={r}
          fill="none"
          strokeWidth={stroke}
          strokeLinecap="round"
          strokeDasharray={around}
          // fills from empty on arrival, and moves when the count changes (styles.css)
          style={{ strokeDashoffset: around - filled, "--around": `${around}px` } as CSSProperties}
          transform={`rotate(-90 ${c} ${c})`}
        />
      )}
      {!mini && (
        <text x={c} y={c + 5} textAnchor="middle" aria-hidden="true">
          {label ?? `${done}/${total}`}
        </text>
      )}
    </svg>
  );
}

export function ToneIcon({ tone }: { tone: Tone }) {
  if (tone === "ok") return <CheckCircle2 size={16} className="check-icon ok" aria-label="done" />;
  if (tone === "bad") return <XCircle size={16} className="check-icon bad" aria-label="blocks" />;
  if (tone === "warn") return <CircleAlert size={16} className="check-icon warn" aria-label="needs attention" />;
  return <Circle size={16} className="check-icon none" aria-hidden="true" />;
}

/** One line of a decide card: what blocks stands out, what passes steps back,
 *  and an `item` (a thing to look at) reads plainly. */
export type DecideRow = { key: string; tone?: Tone | "item"; icon?: ReactNode; text: ReactNode; aside?: ReactNode };

/**
 * The page's emphasized card, for what there is to decide or act on (as a merge
 * request's readiness, or the manual job a pipeline waits on): what and why on
 * the left with the one action, the items it is about on the right. A page has
 * one at most, and none when nothing waits.
 */
export function DecideCard({
  label,
  eyebrow,
  late,
  ring,
  title,
  sub,
  actions,
  rowsTitle,
  rows,
  foot,
}: {
  label: string;
  eyebrow?: ReactNode;
  /** the eyebrow says something is late or failing: in the card's red */
  late?: boolean;
  ring?: ReactNode;
  title: ReactNode;
  sub?: ReactNode;
  actions?: ReactNode;
  rowsTitle?: ReactNode;
  rows?: DecideRow[];
  foot?: ReactNode;
}) {
  const side = (rows?.length ?? 0) > 0 || foot != null;
  return (
    <section className="decide-card" aria-label={label}>
      <div className="decide-main">
        {eyebrow && <span className={`eyebrow${late ? " bad" : ""}`}>{eyebrow}</span>}
        <div className="decide-head">
          {ring}
          <div className="decide-title">
            <h2>{title}</h2>
            {sub && <p>{sub}</p>}
          </div>
        </div>
        {actions && <div className="decide-actions">{actions}</div>}
      </div>
      {side && (
        <div className="decide-side">
          {rowsTitle != null && <p className="decide-label">{rowsTitle}</p>}
          {rows && rows.length > 0 && (
            <ul className="checks">
              {rows.map((r) => (
                <li key={r.key} className={r.tone ?? "item"}>
                  {r.icon ?? <ToneIcon tone={r.tone && r.tone !== "item" ? r.tone : "none"} />}
                  <span className="decide-text">{r.text}</span>
                  {r.aside != null && <span className="decide-aside">{r.aside}</span>}
                </li>
              ))}
            </ul>
          )}
          {foot != null && <p className="decide-foot">{foot}</p>}
        </div>
      )}
    </section>
  );
}
