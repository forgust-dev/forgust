// Form controls on Base UI primitives (keyboard, focus and ARIA handled there),
// styled by `styles.css` (`.select`, `.switch`, `.segmented`, `.dropdown`).
import { useState, type CSSProperties, type ReactElement, type ReactNode } from "react";
import { Menu as BMenu } from "@base-ui/react/menu";
import { Popover as BPopover } from "@base-ui/react/popover";
import { Radio } from "@base-ui/react/radio";
import { RadioGroup } from "@base-ui/react/radio-group";
import { Select as BSelect } from "@base-ui/react/select";
import { Switch as BSwitch } from "@base-ui/react/switch";
import { Check, ChevronDown } from "lucide-react";

export type Option = { value: string; label: ReactNode; hint?: ReactNode; disabled?: boolean; group?: string };

function item(o: Option) {
  return (
    <BSelect.Item key={o.value} value={o.value} className="select-item" disabled={o.disabled}>
      <BSelect.ItemIndicator className="select-check">
        <Check size={14} />
      </BSelect.ItemIndicator>
      <span className="select-text">
        <BSelect.ItemText>{o.label}</BSelect.ItemText>
        {o.hint && <span className="select-hint">{o.hint}</span>}
      </span>
    </BSelect.Item>
  );
}

/** Consecutive options of the same `group`. */
function groups(options: Option[]): [string | undefined, Option[]][] {
  const out: [string | undefined, Option[]][] = [];
  for (const o of options) {
    const last = out[out.length - 1];
    if (last && last[0] === o.group) last[1].push(o);
    else out.push([o.group, [o]]);
  }
  return out;
}

/** A select box. `label` names it when no `<label>` wraps it. */
export function Select({
  value,
  onChange,
  options,
  label,
  placeholder,
  id,
  style,
  disabled,
}: {
  value: string;
  onChange: (v: string) => void;
  options: Option[];
  label?: string;
  placeholder?: string;
  id?: string;
  style?: CSSProperties;
  disabled?: boolean;
}) {
  return (
    // `items` gives the box the chosen option's label; "" is a value like any other
    <BSelect.Root value={value} onValueChange={(v) => onChange(v ?? "")} items={options.map((o) => ({ value: o.value, label: o.label }))} disabled={disabled}>
      <BSelect.Trigger className="select" aria-label={label} id={id} style={style}>
        <BSelect.Value className="select-value" placeholder={placeholder} />
        <BSelect.Icon className="select-icon">
          <ChevronDown size={14} />
        </BSelect.Icon>
      </BSelect.Trigger>
      <BSelect.Portal>
        {/* under the box, as a list (not over it, with the chosen option on the box) */}
        <BSelect.Positioner className="positioner" side="bottom" align="start" sideOffset={4} alignItemWithTrigger={false}>
          <BSelect.Popup className="select-menu">
            {groups(options).map(([group, opts], gi) =>
              group ? (
                <BSelect.Group key={group}>
                  {gi > 0 && <BSelect.Separator className="dropdown-sep" />}
                  <BSelect.GroupLabel className="select-label">{group}</BSelect.GroupLabel>
                  {opts.map(item)}
                </BSelect.Group>
              ) : (
                opts.map(item)
              ),
            )}
          </BSelect.Popup>
        </BSelect.Positioner>
      </BSelect.Portal>
    </BSelect.Root>
  );
}

/**
 * A select box with a filter: for lists too long to scan. Typing narrows the
 * options, the arrows move, Enter takes the highlighted one.
 */
export function Combobox({
  value,
  onChange,
  options,
  label,
  style,
}: {
  value: string;
  onChange: (v: string) => void;
  options: { value: string; label: string }[];
  label: string;
  style?: CSSProperties;
}) {
  const [open, setOpen] = useState(false);
  const [text, setText] = useState("");
  const [active, setActive] = useState(0);
  const term = text.trim().toLowerCase();
  const shown = options.filter((o) => o.label.toLowerCase().includes(term));
  const pick = (v: string) => {
    onChange(v);
    setOpen(false);
  };
  return (
    <Popover
      open={open}
      onOpenChange={(o) => {
        setOpen(o);
        setText("");
        setActive(0);
      }}
      align="start"
      label={label}
      trigger={
        <button type="button" className="select" role="combobox" aria-label={label} aria-expanded={open} style={style}>
          <span className="select-value">{options.find((o) => o.value === value)?.label ?? value}</span>
          <span className="select-icon">
            <ChevronDown size={14} />
          </span>
        </button>
      }
    >
      <div className="combo">
        <input
          type="search"
          autoFocus
          value={text}
          placeholder="Filter…"
          aria-label={`Filter: ${label}`}
          onChange={(e) => {
            setText(e.target.value);
            setActive(0);
          }}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown" || e.key === "ArrowUp") {
              e.preventDefault();
              setActive((a) => Math.max(0, Math.min(shown.length - 1, a + (e.key === "ArrowDown" ? 1 : -1))));
            } else if (e.key === "Enter" && shown[active]) {
              e.preventDefault();
              pick(shown[active].value);
            }
          }}
        />
        <div className="combo-list" role="listbox" aria-label={label}>
          {shown.map((o, i) => (
            <div
              key={o.value}
              role="option"
              aria-selected={o.value === value}
              data-highlighted={i === active ? "" : undefined}
              ref={i === active ? (el) => el?.scrollIntoView({ block: "nearest" }) : undefined}
              className="select-item"
              onMouseMove={() => setActive(i)}
              onClick={() => pick(o.value)}
            >
              {o.value === value && (
                <span className="select-check">
                  <Check size={14} />
                </span>
              )}
              {o.label}
            </div>
          ))}
          {shown.length === 0 && <div className="combo-empty">Nothing matches</div>}
        </div>
      </div>
    </Popover>
  );
}

/** An on/off switch with its label. */
export function Switch({ checked, onChange, children, disabled }: { checked: boolean; onChange: (v: boolean) => void; children: ReactNode; disabled?: boolean }) {
  return (
    <label className="switch-row">
      <BSwitch.Root className="switch" checked={checked} onCheckedChange={(v) => onChange(v)} disabled={disabled}>
        <BSwitch.Thumb className="switch-thumb" />
      </BSwitch.Root>
      <span>{children}</span>
    </label>
  );
}

/** One choice among a few, shown side by side: radios (one is always chosen). */
export function Segmented<T extends string>({ value, onChange, options, label }: { value: T; onChange: (v: T) => void; options: (Option & { value: T })[]; label: string }) {
  return (
    <RadioGroup className="segmented" aria-label={label} value={value} onValueChange={(v) => onChange(v as T)}>
      {options.map((o) => (
        <Radio.Root key={o.value} value={o.value} className="segment" disabled={o.disabled} title={typeof o.hint === "string" ? o.hint : undefined}>
          {o.label}
        </Radio.Root>
      ))}
    </RadioGroup>
  );
}

export type MenuItem = { label: ReactNode; icon?: ReactNode; onSelect: () => void; danger?: boolean; disabled?: boolean } | "separator";

/** A dropdown menu opened by `trigger` (a button, or a component that passes its props and ref to one). */
export function Menu({ trigger, items, align = "end" }: { trigger: ReactElement; items: MenuItem[]; align?: "start" | "end" }) {
  return (
    <BMenu.Root modal={false}>
      <BMenu.Trigger render={trigger} />
      <BMenu.Portal>
        <BMenu.Positioner className="positioner" align={align} sideOffset={6}>
          <BMenu.Popup className="dropdown">
            {items.map((it, i) =>
              it === "separator" ? (
                <BMenu.Separator key={i} className="dropdown-sep" />
              ) : (
                <BMenu.Item key={i} className={`dropdown-item ${it.danger ? "danger" : ""}`} onClick={() => it.onSelect()} disabled={it.disabled}>
                  {it.icon}
                  {it.label}
                </BMenu.Item>
              ),
            )}
          </BMenu.Popup>
        </BMenu.Positioner>
      </BMenu.Portal>
    </BMenu.Root>
  );
}

/** A floating panel opened by `trigger` (a button, or a component that passes its props and ref to one). */
export function Popover({
  trigger,
  children,
  align = "end",
  label,
  open,
  onOpenChange,
}: {
  trigger: ReactElement;
  children: ReactNode;
  align?: "start" | "end";
  label?: string;
  /** Controlled: set both to open and close it from the content. */
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}) {
  return (
    <BPopover.Root open={open} onOpenChange={onOpenChange && ((o) => onOpenChange(o))}>
      <BPopover.Trigger render={trigger} />
      <BPopover.Portal>
        <BPopover.Positioner className="positioner" align={align} sideOffset={6}>
          <BPopover.Popup className="popover" aria-label={label}>
            {children}
          </BPopover.Popup>
        </BPopover.Positioner>
      </BPopover.Portal>
    </BPopover.Root>
  );
}
