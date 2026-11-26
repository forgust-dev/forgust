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

A tone has one color per theme (`--ok`, `--danger`…), charts included: one green and one red
on light, one of each on dark. On the dark panel, in light, `--emph-*` takes the dark theme's
color, as its text does. Text on a filled tone uses `--on-tone` (or `--accent-text` on orange),
never `#fff`. README alerts keep GitHub's colors, and a job log keeps its terminal palette, whose
base green and red are the site's. A dot that stands for a state or a series (a stage, a
legend) is round and 8 px (`.dot`): at 125 or 150 %, 9 or 10 px fall on half pixels and come
out uneven.

The sidebar, the top bar and the job list are painted `--bg-side`, which in light is the color
of `--bg-hover`: a hover there takes `--side-hover`, or it does not show.
On a card, a hovered row takes `--row-hover`: in light, `--bg-subtle` is almost the page's
color, and the card seemed to fade.

Never put a hex color in a component. Data that carries its own color is the exception: a label
and its swatches, code highlighting, the Smart View's block kinds and logos.

A spinner (`Loader2`) always turns, with `className="spin"`. `StateBadge` adds it on its own.

A date is a `DateField` (typed, or picked on a calendar), never `type="date"` and its browser
picker; a password a `PasswordInput`, with its Show button.

## Sizes

Buttons, fields and menus share one height, from tokens in `src/styles.css`: `--control-h`
(30 px), `--control-h-sm` (26 px, `btn small`) and `--control-h-lg` (34 px). On a touch screen
they are 36 px. Button text is 13 px, a field's label 13 px, what is typed 14 px. A control
next to another takes its height from the same token, so they line up; never write a height.

The one main action of a page is a step taller (`btn large`): the buttons of its head
(`.list-head`, on its own), a creation page's submit and its Cancel (`.form-actions`, on its
own, or `large`). Merge has its own full-width button in its box (`.btn.block`). Everything else
is the usual size: Save in a section, a window's buttons.

## Buttons

- `btn primary`: the one main action of a page or a section. It is black on light and white
  on dark. Disabled, it keeps its color, faded, like every disabled button. When it is
  disabled because it is working, also give it `aria-busy={busy}`, so assistive tech says so.
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

## Settings

`components/settings.tsx` holds their rules; `/-/ui` shows them.

- A settings page is sections: a title and one sentence on the left (`Section`), the settings on
  the right. Each section has an id listed in `pages/project/settingsPages.ts`
  (`SETTINGS_SECTIONS`): the Ctrl+K palette finds it, and `#id` scrolls to it.
- A form of several fields has one Save (`SaveRow`, values in `useSaved`): grey until something
  changes, then "Saved" beside it. Leaving the page with changes not saved asks first.
- A setting that is one switch saves as it is flipped (`SettingSwitch`); if turning it off undoes
  something, it asks first.
- A feature is turned on by a switch; its options show once it is on, and are still sent as they
  are while it is off.
- A list shows what is there first, or one line when nothing is (`EmptyRow`). Its button, under
  the section's description, opens its form in a window (`ListSection`, see Adding).
- A field is as wide as what it holds (440 px, 160 for a number), not the whole column.
- A rule says in words what it lets happen ("Maintainers can push and merge · no force push").
- What cannot be undone lightly is in the page's Danger zone, at the bottom: one row per action,
  with what it does. Deleting asks to type the name.

## Adding

What is created gets its own page afterwards? It is created on a page (`…/new`): an issue, a
merge request, a milestone, a release, a file, a project, a group. Its button says "New …".

A row of a list (a protected branch, a key, a token, a webhook, a variable, a schedule, a
label, a branch, a member…) is added, and changed, in a window (`FormDialog`,
`components/FormDialog.tsx`). Its button says what it does with a verb, not "New": "Add a
label", "Protect a branch", "Create a token". The window:

- has the button's words as its title, then the fields, then `FormActions`: Cancel and what it
  does. It starts on its first field; Enter sends it.
- closes with Cancel, Esc or a click outside, except that a click outside does nothing once
  something was typed.
- shows a secret shown once (a token, a runner's) in place of its form, as `result`; only Done
  closes it then, and the secret is never left on the page.
- shows the popups of its fields above it (`InWindow`, read by `controls.tsx`).

The row added comes in highlighted, then fades back (`useJustAdded`: `mark` its key, give each
row `fresh(key)`).

What is added inside a form being filled stays in it: a variable of a pipeline to run, a link
of a release, a project to import. So do comments and reactions.

## Numbers

A breakdown (what takes a project's storage) is a bar per part, the largest first, in one
grey: a part is not a state. The parts that hold nothing are one line.

Check new UI in light and dark, at desktop width and at 390 px.
