import { AlertTriangle, Archive, Ban, Bell, CircleCheck, CircleX, Clock, Copy, GitBranch, GitMerge, Globe, HardDrive, Loader2, Lock, Milestone, Pencil, Plus, Shield, Star, Tag, Trash2, UserRound, type LucideIcon } from "lucide-react";
import { Badge, Card, ListHead, Section, StateBadge, type Tone } from "../components/ui";
import { StateMark, StateOf, issueLook, mrLook } from "../components/states";
import { LabelChip } from "../components/labels";
import { DecideCard } from "../components/decide";
import { StatusBadge, StatusIcon } from "./project/ci/Status";

/** Each tone, what it says, and its usual icon. */
const TONES: [Tone, string, LucideIcon][] = [
  ["ok", "It is fine", CircleCheck],
  ["danger", "It broke", CircleX],
  ["warn", "It waits", Clock],
  ["info", "It runs", Loader2],
  ["merged", "Merged", GitMerge],
  ["neutral", "Nothing to say", Ban],
];
const CI = ["success", "failed", "running", "pending", "scheduled", "manual", "canceled", "skipped", "created"];
const SEVERITIES = ["critical", "high", "medium", "low", "info"];
const cap = (s: string) => s[0].toUpperCase() + s.slice(1);

/** The UI convention on one page (`/-/ui`, linked from nowhere): every family, each in its one
 *  shape, to reuse rather than draw again. One shape per family, one color per meaning. */
export function UiKit() {
  return (
    <div className="page stack ui-kit">
      <ListHead title="UI convention" sub="One shape per family, one color per meaning. Reuse these; do not draw new ones." />

      <Section title="States" description="Where a thing is: a pill tinted with its tone, its icon and its word (StateBadge), before a title. In a dense list, its icon alone in the same color (StateMark).">
        <div className="ui-kit-rows">
          <div className="ui-kit-row">
            {TONES.map(([tone, word, icon]) => (
              <StateBadge key={tone} tone={tone} icon={icon}>
                {word}
              </StateBadge>
            ))}
          </div>
          <div className="ui-kit-row">
            {[mrLook("opened"), mrLook("opened", true), mrLook("merged"), mrLook("closed"), issueLook("opened"), issueLook("closed")].map((l, i) => (
              <StateOf key={i} look={l} />
            ))}
          </div>
          <div className="ui-kit-row">
            {CI.map((s) => (
              <StatusBadge key={s} status={s} />
            ))}
          </div>
          <div className="ui-kit-row">
            {SEVERITIES.map((s) => (
              <span key={s} className={`sev sev-${s}`}>
                {cap(s)}
              </span>
            ))}
          </div>
          <div className="ui-kit-row">
            {[mrLook("opened"), mrLook("merged"), mrLook("closed"), issueLook("opened")].map((l, i) => (
              <StateMark key={i} look={l} />
            ))}
            {CI.map((s) => (
              <StatusIcon key={s} status={s} />
            ))}
          </div>
        </div>
      </Section>

      <Section title="Labels" description="What a team chose to sort things by: the one family in free colors, theirs.">
        <div className="ui-kit-row">
          <LabelChip label={{ name: "bug", color: "#d1242f", description: "" }} />
          <LabelChip label={{ name: "feature", color: "#1a7f37", description: "" }} />
          <LabelChip label={{ name: "documentation", color: "#0969da", description: "" }} />
          <LabelChip label={{ name: "priority::high", color: "#cf4a10", description: "" }} />
        </div>
      </Section>

      <Section title="Attributes" description="A property of a thing: its visibility, a role, what protects it, a kind. A fine grey outline (Badge, .pill), never colored.">
        <div className="ui-kit-row">
          <Badge>
            <Globe size={11} aria-hidden /> Public
          </Badge>
          <Badge>
            <Lock size={11} aria-hidden /> Private
          </Badge>
          <Badge>
            <UserRound size={11} aria-hidden /> owner
          </Badge>
          <Badge>
            <Star size={11} aria-hidden /> default
          </Badge>
          <Badge>
            <Shield size={11} aria-hidden /> protected
          </Badge>
          <Badge>
            <HardDrive size={11} aria-hidden /> LFS
          </Badge>
          <Badge>bot</Badge>
        </div>
      </Section>

      <Section title="References" description="A branch, a tag, a commit, a milestone: a grey chip in mono, clickable, the same on every page (.ref-chip, .sha, .milestone-chip).">
        <div className="ui-kit-row">
          <a className="ref-chip" href="#references">
            <GitBranch size={12} /> main
          </a>
          <a className="ref-chip small" href="#references">
            <GitBranch size={11} /> feature/discounts
          </a>
          <a className="ref-chip small" href="#references">
            <Tag size={11} /> v1.4.0
          </a>
          <a className="sha" href="#references">
            e37a083f
          </a>
          <a className="milestone-chip" href="#references">
            <Milestone size={12} /> v1.1
          </a>
        </div>
      </Section>

      <Section title="Counters" description="How many: a small grey number right after what it counts. Unread is the one counter in color.">
        <div className="ui-kit-row">
          <span>
            Open <span className="tab-count">9</span>
          </span>
          <span>
            Your projects <span className="section-count">8</span>
          </span>
          <span className="ui-kit-bell">
            <Bell size={17} aria-hidden />
            <span className="inbox-count">9</span>
          </span>
        </div>
      </Section>

      <Section title="Buttons" description="Primary: the one main action of a page or a section. Secondary: every other one. Ghost: an action in a list or a card. Danger: outlined in a list, solid only in the confirmation. An icon alone has its tooltip.">
        <div className="ui-kit-rows">
          <div className="ui-kit-row">
            <button type="button" className="btn primary">
              <Plus size={15} /> New merge request
            </button>
            <button type="button" className="btn">
              Schedules
            </button>
            <button type="button" className="btn ghost">
              Show done
            </button>
            <button type="button" className="btn danger">
              <Trash2 size={14} /> Delete branch
            </button>
            <button type="button" className="icon-btn" title="Edit this file" aria-label="Edit this file">
              <Pencil size={16} />
            </button>
          </div>
          <div className="ui-kit-row">
            <button type="button" className="btn small primary">
              Save
            </button>
            <button type="button" className="btn small">
              Compare
            </button>
            <button type="button" className="btn small ghost">
              Mark all read
            </button>
            <button type="button" className="btn small danger">
              Remove
            </button>
            <button type="button" className="btn small danger solid">
              Delete
            </button>
            <button type="button" className="icon-btn" title="Copy" aria-label="Copy">
              <Copy size={14} />
            </button>
          </div>
          <div className="ui-kit-row">
            <button type="button" className="btn small primary" disabled>
              Add member
            </button>
            <button type="button" className="btn small" disabled>
              Share
            </button>
          </div>
        </div>
      </Section>

      <Section title="Containers" description="A card holds content; a list is a card of rows; the dark panel says what waits for you, one per page at most, at the top. A banner is the whole page's state, an alert a form's result.">
        <div className="ui-kit-rows">
          <Card title="Test summary" actions={<button type="button" className="btn small ghost">Show all tests</button>}>
            10 tests · 0 failed · 9 passed · 1 skipped
          </Card>
          <DecideCard
            label="Example"
            eyebrow="Needs an administrator"
            title="1 thing needs you"
            sub="From the health checks."
            actions={
              <button type="button" className="btn run">
                Open Health
              </button>
            }
            rows={[{ key: "email", tone: "warn", text: "Email · not configured" }]}
          />
          <div className="banner info" role="note">
            <Loader2 size={16} className="spin" aria-hidden />
            <span className="grow small">A newer commit is being mapped: this is the map of the one before.</span>
          </div>
          <div className="banner" role="note">
            <AlertTriangle size={16} aria-hidden />
            <span className="grow small">This project's storage is 92 % full.</span>
          </div>
          <div className="banner neutral" role="note">
            <Archive size={16} aria-hidden />
            <span className="grow small">This project is archived: everything stays readable, nothing can change.</span>
          </div>
          <div className="banner danger" role="note">
            <CircleX size={16} aria-hidden />
            <span className="grow small">The commit could not be mapped.</span>
          </div>
          <div className="alert ok">
            <CircleCheck size={15} aria-hidden /> Saved.
          </div>
          <div className="alert error">
            <CircleX size={15} aria-hidden /> This name is taken: choose another.
          </div>
        </div>
      </Section>
    </div>
  );
}
