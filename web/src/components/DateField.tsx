import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { CalendarDays, ChevronLeft, ChevronRight } from "lucide-react";
import { Popover } from "./controls";

/**
 * A date as the API writes it, "YYYY-MM-DD", read and written from its parts: a Date parsed
 * from that string is midnight UTC, a day off in some time zones.
 */
type Ymd = { y: number; m: number; d: number };

const pad = (n: number) => String(n).padStart(2, "0");
const iso = ({ y, m, d }: Ymd) => `${y}-${pad(m)}-${pad(d)}`;
const daysIn = (y: number, m: number) => new Date(y, m, 0).getDate();
const same = (a: Ymd | null, b: Ymd) => !!a && a.y === b.y && a.m === b.m && a.d === b.d;
const local = ({ y, m, d }: Ymd) => new Date(y, m - 1, d);
const ymd = (t: Date): Ymd => ({ y: t.getFullYear(), m: t.getMonth() + 1, d: t.getDate() });
const shift = (x: Ymd, days: number) => ymd(new Date(x.y, x.m - 1, x.d + days));
/** The same day a month away, or that month's last when it is shorter. */
function addMonths(x: Ymd, n: number): Ymd {
  const first = new Date(x.y, x.m - 1 + n, 1);
  const y = first.getFullYear();
  const m = first.getMonth() + 1;
  return { y, m, d: Math.min(x.d, daysIn(y, m)) };
}
/** Weeks start on Monday, as ISO dates do: 0 is Monday, 6 Sunday. */
const weekday = (x: Ymd) => (local(x).getDay() + 6) % 7;
const WEEKDAYS = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

function parse(s: string): Ymd | null {
  const r = /^(\d{4})-(\d{2})-(\d{2})$/.exec(s.trim());
  if (!r) return null;
  const x = { y: Number(r[1]), m: Number(r[2]), d: Number(r[3]) };
  return x.m >= 1 && x.m <= 12 && x.d >= 1 && x.d <= daysIn(x.y, x.m) ? x : null;
}

/** A month to pick a day in. Arrows move a day or a week, Page Up and Down a month. */
function Calendar({ value, onPick }: { value: Ymd | null; onPick: (d: Ymd) => void }) {
  const [focus, setFocus] = useState<Ymd>(value ?? ymd(new Date()));
  // the keyboard moves focus with the day; the month buttons leave it where it is
  const keyed = useRef(true);
  const grid = useRef<HTMLDivElement>(null);
  const today = ymd(new Date());
  const first = { y: focus.y, m: focus.m, d: 1 };
  const start = shift(first, -weekday(first));
  const weeks = Array.from({ length: 6 }, (_, w) => Array.from({ length: 7 }, (_, i) => shift(start, w * 7 + i)));
  const title = local(first).toLocaleDateString([], { month: "long", year: "numeric" });

  useEffect(() => {
    if (keyed.current) grid.current?.querySelector<HTMLButtonElement>('button[tabindex="0"]')?.focus();
  }, [focus]);

  const keys = (e: KeyboardEvent) => {
    const step = ({ ArrowLeft: -1, ArrowRight: 1, ArrowUp: -7, ArrowDown: 7 } as Record<string, number>)[e.key];
    const next =
      step != null
        ? shift(focus, step)
        : e.key === "PageUp"
          ? addMonths(focus, -1)
          : e.key === "PageDown"
            ? addMonths(focus, 1)
            : e.key === "Home"
              ? shift(focus, -weekday(focus))
              : e.key === "End"
                ? shift(focus, 6 - weekday(focus))
                : null;
    if (!next) return;
    e.preventDefault();
    keyed.current = true;
    setFocus(next);
  };
  const month = (n: number) => {
    keyed.current = false;
    setFocus(addMonths(focus, n));
  };

  return (
    <div className="calendar">
      <div className="calendar-head">
        <button type="button" className="icon-btn" aria-label="Previous month" onClick={() => month(-1)}>
          <ChevronLeft size={16} />
        </button>
        <span className="calendar-title" aria-live="polite">
          {title}
        </span>
        <button type="button" className="icon-btn" aria-label="Next month" onClick={() => month(1)}>
          <ChevronRight size={16} />
        </button>
      </div>
      <div className="calendar-grid" role="grid" aria-label={title} ref={grid} onKeyDown={keys}>
        <div className="calendar-row" role="row">
          {WEEKDAYS.map((w) => (
            <span key={w} className="calendar-wd" role="columnheader">
              {w}
            </span>
          ))}
        </div>
        {weeks.map((week, w) => (
          <div key={w} className="calendar-row" role="row">
            {week.map((d) => (
              <span key={iso(d)} role="gridcell" aria-selected={same(value, d)}>
                <button
                  type="button"
                  tabIndex={same(focus, d) ? 0 : -1}
                  className={`calendar-day${d.m !== focus.m ? " out" : ""}${same(today, d) ? " today" : ""}${same(value, d) ? " on" : ""}`}
                  aria-current={same(today, d) ? "date" : undefined}
                  aria-label={local(d).toLocaleDateString([], { weekday: "long", day: "numeric", month: "long", year: "numeric" })}
                  onClick={() => onPick(d)}
                >
                  {d.d}
                </button>
              </span>
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}

/**
 * A date: typed (YYYY-MM-DD) or picked on a calendar. `value` and `onChange` speak the API's
 * "YYYY-MM-DD", "" for none. A typed date counts once it is whole; an emptied field clears the
 * date when you leave it or press Enter, so retyping a date does not clear it on the way.
 */
export function DateField({ value, onChange, label }: { value: string; onChange: (v: string) => void; label?: string }) {
  const [text, setText] = useState(value);
  const [open, setOpen] = useState(false);
  // a date set from elsewhere (picked, saved) shows as it is
  useEffect(() => setText(value), [value]);
  const type = (t: string) => {
    setText(t);
    const d = parse(t);
    if (d && iso(d) !== value) onChange(iso(d));
  };
  const leave = () => {
    if (text.trim() === "") {
      if (value) onChange("");
    } else if (!parse(text)) setText(value);
  };
  // Enter sends the form with what the field shows: an emptied date first cleared, the form
  // sent once that is rendered, by its button (a disabled one sends nothing, as Enter would);
  // a date that is not one yet sends nothing
  const enter = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key !== "Enter") return;
    if (text.trim() === "") {
      if (!value) return;
      e.preventDefault();
      onChange("");
      const submit = e.currentTarget.form?.querySelector<HTMLElement>('button:not([type]), button[type="submit"], input[type="submit"]');
      window.setTimeout(() => submit?.click());
    } else if (!parse(text)) e.preventDefault();
  };
  const pick = (v: string) => {
    onChange(v);
    setOpen(false);
  };
  return (
    <span className="date-field">
      <input
        type="text"
        autoComplete="off"
        placeholder="YYYY-MM-DD"
        aria-label={label}
        aria-invalid={text.trim() !== "" && !parse(text) ? true : undefined}
        value={text}
        onChange={(e) => type(e.target.value)}
        onBlur={leave}
        onKeyDown={enter}
      />
      <Popover
        open={open}
        onOpenChange={setOpen}
        label="Calendar"
        trigger={
          <button type="button" className="icon-btn" aria-label="Calendar" title="Pick a date">
            <CalendarDays size={15} />
          </button>
        }
      >
        <Calendar value={parse(value)} onPick={(d) => pick(iso(d))} />
        {value && (
          <div className="calendar-foot">
            <button type="button" className="link-btn small" onClick={() => pick("")}>
              Clear
            </button>
          </div>
        )}
      </Popover>
    </span>
  );
}
