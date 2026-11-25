// Form controls on Base UI primitives (keyboard, focus and ARIA handled there),
// styled by `styles.css` (`.select`, `.switch`, `.segmented`, `.choices`, `.dropdown`).
import { createContext, useContext, type CSSProperties, type ReactElement, type ReactNode } from "react";
import { Combobox as BCombobox } from "@base-ui/react/combobox";
import { Menu as BMenu } from "@base-ui/react/menu";
import { Popover as BPopover } from "@base-ui/react/popover";
import { Radio } from "@base-ui/react/radio";
import { RadioGroup } from "@base-ui/react/radio-group";
import { Select as BSelect } from "@base-ui/react/select";
import { Switch as BSwitch } from "@base-ui/react/switch";
import { Check, ChevronDown } from "lucide-react";

/** Inside a window (FormDialog): the popups its fields open show above it. */
export const InWindow = createContext(false);
/** A popup's layer: above the window it was opened from, if any (styles.css, `.positioner`). */
const usePositioner = () => (useContext(InWindow) ? "positioner over-window" : "positioner");

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
  const layer = usePositioner();
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
        <BSelect.Positioner className={layer} side="bottom" align="start" sideOffset={4} alignItemWithTrigger={false}>
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
 * options, the arrows move, Enter takes the highlighted one (the first match).
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
  const layer = usePositioner();
  // the items are the options themselves: the box shows the chosen one's label,
  // the filter matches labels; a value no option has shows as it is
  const chosen = options.find((o) => o.value === value) ?? { value, label: value };
  return (
    <BCombobox.Root
      items={options}
      value={chosen}
      onValueChange={(o) => o && onChange(o.value)}
      itemToStringLabel={(o) => o.label}
      isItemEqualToValue={(a, b) => a.value === b.value}
      // the first match is highlighted as you type: Enter takes it
      autoHighlight
    >
      <BCombobox.Trigger className="select" aria-label={label} style={style}>
        <BCombobox.Value>{(o: { label: string } | null) => <span className="select-value">{o?.label}</span>}</BCombobox.Value>
        <BCombobox.Icon className="select-icon">
          <ChevronDown size={14} />
        </BCombobox.Icon>
      </BCombobox.Trigger>
      <BCombobox.Portal>
        <BCombobox.Positioner className={layer} side="bottom" align="start" sideOffset={4}>
          <BCombobox.Popup className="select-menu combo" aria-label={label}>
            <BCombobox.Input type="search" placeholder="Filter…" aria-label={`Filter: ${label}`} />
            <BCombobox.Empty className="combo-empty">Nothing matches</BCombobox.Empty>
            <BCombobox.List className="combo-list">
              {(o: { value: string; label: string }) => (
                <BCombobox.Item key={o.value} value={o} className="select-item">
                  <BCombobox.ItemIndicator className="select-check">
                    <Check size={14} />
                  </BCombobox.ItemIndicator>
                  {o.label}
                </BCombobox.Item>
              )}
            </BCombobox.List>
          </BCombobox.Popup>
        </BCombobox.Positioner>
      </BCombobox.Portal>
    </BCombobox.Root>
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
  const at = options.findIndex((o) => o.value === value);
  return (
    <RadioGroup
      className="segmented"
      aria-label={label}
      value={value}
      onValueChange={(v) => onChange(v as T)}
      // where the pill of the chosen segment stands; it slides to the next one (styles.css)
      style={{ "--n": options.length, "--at": Math.max(at, 0) } as CSSProperties}
      data-none={at < 0 ? "" : undefined}
    >
      {options.map((o) => (
        <Radio.Root key={o.value} value={o.value} className="segment" disabled={o.disabled} title={typeof o.hint === "string" ? o.hint : undefined}>
          {/* the label in bold, hidden, holds the width: choosing a segment does not resize the control */}
          <span className="segment-width" aria-hidden="true">
            {o.label}
          </span>
          <span className="segment-label">{o.label}</span>
        </Radio.Root>
      ))}
    </RadioGroup>
  );
}

/** Radio cards: a few options, each with a line of detail under its name. */
export function Choices<T extends string>({ value, onChange, options, label }: { value: T; onChange: (v: T) => void; options: (Option & { value: T })[]; label: string }) {
  return (
    <RadioGroup className="choices" aria-label={label} value={value} onValueChange={(v) => onChange(v as T)}>
      {options.map((o) => (
        <Radio.Root key={o.value} value={o.value} className="choice" disabled={o.disabled}>
          <span className="choice-label">{o.label}</span>
          {o.hint && <span className="choice-hint">{o.hint}</span>}
        </Radio.Root>
      ))}
    </RadioGroup>
  );
}

export type MenuItem = { label: ReactNode; icon?: ReactNode; onSelect: () => void; danger?: boolean; disabled?: boolean } | "separator";

/** A dropdown menu opened by `trigger` (a button, or a component that passes its props and ref to one). */
export function Menu({ trigger, items, align = "end" }: { trigger: ReactElement; items: MenuItem[]; align?: "start" | "end" }) {
  const layer = usePositioner();
  return (
    <BMenu.Root modal={false}>
      <BMenu.Trigger render={trigger} />
      <BMenu.Portal>
        <BMenu.Positioner className={layer} align={align} sideOffset={6}>
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
  const layer = usePositioner();
  return (
    <BPopover.Root open={open} onOpenChange={onOpenChange && ((o) => onOpenChange(o))}>
      <BPopover.Trigger render={trigger} />
      <BPopover.Portal>
        <BPopover.Positioner className={layer} align={align} sideOffset={6}>
          <BPopover.Popup className="popover" aria-label={label}>
            {children}
          </BPopover.Popup>
        </BPopover.Positioner>
      </BPopover.Portal>
    </BPopover.Root>
  );
}
