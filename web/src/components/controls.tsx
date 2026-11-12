// Form controls on Radix primitives (keyboard, focus and ARIA handled there),
// styled by `styles.css` (`.select`, `.switch`, `.segmented`, `.dropdown`).
import type { CSSProperties, ReactNode } from "react";
import { DropdownMenu, Popover as RPopover, Select as RSelect, Switch as RSwitch, ToggleGroup } from "radix-ui";
import { Check, ChevronDown } from "lucide-react";

export type Option = { value: string; label: ReactNode; hint?: ReactNode; disabled?: boolean; group?: string };

// Radix reserves "" for "no value"; options may still use it.
const EMPTY = "\u0000empty";
const enc = (v: string) => (v === "" ? EMPTY : v);
const dec = (v: string) => (v === EMPTY ? "" : v);

function item(o: Option) {
  return (
    <RSelect.Item key={o.value} value={enc(o.value)} className="select-item" disabled={o.disabled}>
      <RSelect.ItemIndicator className="select-check">
        <Check size={14} />
      </RSelect.ItemIndicator>
      <span className="select-text">
        <RSelect.ItemText>{o.label}</RSelect.ItemText>
        {o.hint && <span className="select-hint">{o.hint}</span>}
      </span>
    </RSelect.Item>
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
    <RSelect.Root value={enc(value)} onValueChange={(v) => onChange(dec(v))} disabled={disabled}>
      <RSelect.Trigger className="select" aria-label={label} id={id} style={style}>
        <span className="select-value">
          <RSelect.Value placeholder={placeholder} />
        </span>
        <RSelect.Icon className="select-icon">
          <ChevronDown size={14} />
        </RSelect.Icon>
      </RSelect.Trigger>
      <RSelect.Portal>
        <RSelect.Content className="select-menu" position="popper" sideOffset={4} align="start">
          <RSelect.Viewport>
            {groups(options).map(([group, opts], gi) =>
              group ? (
                <RSelect.Group key={group}>
                  {gi > 0 && <RSelect.Separator className="dropdown-sep" />}
                  <RSelect.Label className="select-label">{group}</RSelect.Label>
                  {opts.map(item)}
                </RSelect.Group>
              ) : (
                opts.map(item)
              ),
            )}
          </RSelect.Viewport>
        </RSelect.Content>
      </RSelect.Portal>
    </RSelect.Root>
  );
}

/** An on/off switch with its label. */
export function Switch({ checked, onChange, children, disabled }: { checked: boolean; onChange: (v: boolean) => void; children: ReactNode; disabled?: boolean }) {
  return (
    <label className="switch-row">
      <RSwitch.Root className="switch" checked={checked} onCheckedChange={onChange} disabled={disabled}>
        <RSwitch.Thumb className="switch-thumb" />
      </RSwitch.Root>
      <span>{children}</span>
    </label>
  );
}

/** One choice among a few, shown side by side. */
export function Segmented<T extends string>({ value, onChange, options, label }: { value: T; onChange: (v: T) => void; options: (Option & { value: T })[]; label: string }) {
  return (
    <ToggleGroup.Root
      type="single"
      className="segmented"
      aria-label={label}
      value={value}
      // a click on the chosen one would clear it: keep a value
      onValueChange={(v) => v && onChange(v as T)}
    >
      {options.map((o) => (
        <ToggleGroup.Item key={o.value} value={o.value} className="segment" disabled={o.disabled} title={typeof o.hint === "string" ? o.hint : undefined}>
          {o.label}
        </ToggleGroup.Item>
      ))}
    </ToggleGroup.Root>
  );
}

export type MenuItem = { label: ReactNode; icon?: ReactNode; onSelect: () => void; danger?: boolean; disabled?: boolean } | "separator";

/** A dropdown menu opened by `trigger` (a button). */
export function Menu({ trigger, items, align = "end" }: { trigger: ReactNode; items: MenuItem[]; align?: "start" | "end" }) {
  return (
    <DropdownMenu.Root modal={false}>
      <DropdownMenu.Trigger asChild>{trigger}</DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content className="dropdown" align={align} sideOffset={6}>
          {items.map((it, i) =>
            it === "separator" ? (
              <DropdownMenu.Separator key={i} className="dropdown-sep" />
            ) : (
              <DropdownMenu.Item key={i} className={`dropdown-item ${it.danger ? "danger" : ""}`} onSelect={it.onSelect} disabled={it.disabled}>
                {it.icon}
                {it.label}
              </DropdownMenu.Item>
            ),
          )}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}

/** A floating panel opened by `trigger` (a button). */
export function Popover({
  trigger,
  children,
  align = "end",
  label,
  open,
  onOpenChange,
}: {
  trigger: ReactNode;
  children: ReactNode;
  align?: "start" | "end";
  label?: string;
  /** Controlled: set both to open and close it from the content. */
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}) {
  return (
    <RPopover.Root open={open} onOpenChange={onOpenChange}>
      <RPopover.Trigger asChild>{trigger}</RPopover.Trigger>
      <RPopover.Portal>
        <RPopover.Content className="popover" align={align} sideOffset={6} aria-label={label}>
          {children}
        </RPopover.Content>
      </RPopover.Portal>
    </RPopover.Root>
  );
}
