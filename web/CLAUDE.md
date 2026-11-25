# The web UI

Forgust's UI follows one convention: **one shape per family, one color per meaning**, so
that a glance tells what a thing is. The `/-/ui` page (`src/pages/UiKit.tsx`, linked from
nowhere) shows every family with the real components. Look at it before drawing anything,
reuse what it shows, and change it too when a family changes.

## Families

- **State**, where a thing is (open, merged, running, failed): a pill tinted with its tone,
  with its icon and its word. Use `StateBadge`, or `StateOf` with `mrLook`/`issueLook`
  (`src/components/states.tsx`). In a dense list, use `StateMark`, the icon alone in the same
  color. CI has `StatusBadge`/`StatusIcon`. The older classes `.mr-state`, `.status-badge`,
  `.thread-state` and `.instance-status` draw the same pill.
- **Label**, what a team sorts things by: the one family in free colors, the team's own
  (`LabelChip`).
- **Attribute**, a property (visibility, a role, protected, default, a kind): a fine grey
  outline, never colored (`Badge`, `.pill`). If it changes over time, it is a state.
- **Reference**, a branch, a tag, a commit or a milestone: a grey mono chip, the same on every
  page (`.ref-chip`, `.sha`, `.milestone-chip`).
- **Counter**, how many: a small grey number right after what it counts (`.tab-count`,
  `.section-count`). The unread counter (`.inbox-count`) is the one in color.

## Colors

Each tone has one meaning on every page (`Tone` in `src/components/ui.tsx`). Its token in
`src/styles.css` has a `-soft` background and a dark value:

- `ok`, green `--ok`: it is fine (open, passed, enabled).
- `danger`, red `--danger`: it broke (failed, rejected).
- `warn`, amber `--warn`: it waits or needs care (pending, manual, needs a rebase).
- `info`, blue `--info`: it runs (running, being mapped), or something to know.
- `merged`, violet `--merged`: merged.
- `neutral`, grey: nothing to say (closed, canceled, skipped, draft). Every attribute is grey
  too.

Orange (`--accent`) means interaction: links, focus, what is selected or current, and unread.
It is never a state, an attribute, a count, progress or decoration. A few marks keep the logo's
orange on purpose: the logo, the contribution heatmap, a project's tile without a picture,
folder icons (they open) and the page-loading bar (it answers a click). Progress takes the tone
of what it counts: green for what is done and fine (the readiness ring), blue for what runs (an
import).

A tone has one color per surface: the page tokens (`--ok`, `--danger`…) on the page, `--emph-*`
on a dark panel (lighter on purpose, like its text), `--chart-*` only inside charts. Text on a
filled tone uses `--on-tone` (or `--accent-text` on orange), never `#fff`. README alerts keep
GitHub's colors, and a job log keeps its terminal palette.

The sidebar, the top bar and the job list are painted `--bg-side`, which in light is the color
of `--bg-hover`: a hover there takes `--side-hover`, or it does not show.

Never put a hex color in a component. Data that carries its own color is the exception: a label
and its swatches, code highlighting, the Smart View's block kinds and logos.

A spinner (`Loader2`) always turns, with `className="spin"`. `StateBadge` adds it on its own.

## Buttons

- `btn primary`: the one main action of a page or a section. It is black on light and white
  on dark. Disabled, it turns to a disabled secondary's grey on its own. When it is disabled
  because it is working, give it `aria-busy={busy}`, so it keeps its color, dimmed.
- `btn`: every other action. `btn ghost`: an action inside a list or a card.
- `btn danger`: outlined in a list. `btn danger solid` only in the confirmation.
  `link-btn danger` is a word that destroys something, such as deleting a comment.
- `icon-btn`: an icon alone, always with a `title` and an `aria-label`.
- In a dark panel, use `btn run`, the primary inverted.
- Buttons do not scale or move when pressed.

## Containers

- `Card` holds content. A list is a card of rows.
- `DecideCard` is the dark panel that says what waits for you. A page has at most one, at the
  top.
- `.banner` is the state of the whole page, in the tone of what it says: amber by default,
  `info`, `neutral` or `danger`.
- `.alert ok|error|warn` is a form's result, under the form. Errors use `ErrorBox`.

Check new UI in light and dark, at desktop width and at 390 px.
