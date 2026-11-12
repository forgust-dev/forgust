import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Popover } from "radix-ui";
import { SmilePlus } from "lucide-react";
import { api, type S } from "../api/client";
import { useSession } from "../session";
import { useToast } from "../shell/toast";

export const EMOJI: Record<string, string> = {
  thumbsup: "👍",
  thumbsdown: "👎",
  laughing: "😄",
  tada: "🎉",
  confused: "😕",
  heart: "❤️",
  rocket: "🚀",
  eyes: "👀",
};

/**
 * Reactions of a merge request or issue and of all its comments, one request per
 * page (every bar on it shares the query).
 * `base`: `/projects/<id>/issues/<iid>` or `/projects/<id>/merge_requests/<iid>`.
 */
export function useReactions(base: string) {
  return useQuery({ queryKey: ["reactions", base], queryFn: () => api.get<S["Reactions"]>(`${base}/reactions`) });
}

/** The reactions on one thing, and a picker to add one (signed-in people). */
export function ReactionBar({ base, noteId }: { base: string; noteId?: number }) {
  const { user } = useSession();
  const qc = useQueryClient();
  const toast = useToast();
  const q = useReactions(base);
  const list = (noteId == null ? q.data?.target : q.data?.notes[String(noteId)]) ?? [];
  const toggle = async (name: string) => {
    try {
      await api.post(`${base}/reactions`, { name, note_id: noteId });
      qc.invalidateQueries({ queryKey: ["reactions", base] });
    } catch (e) {
      toast.error(e);
    }
  };
  if (!user && list.length === 0) return null;
  return (
    <div className="reactions" role="group" aria-label="Reactions">
      {list.map((r) => {
        const who = r.users.join(", ") + (r.count > r.users.length ? ` and ${r.count - r.users.length} more` : "");
        return (
          <button
            key={r.name}
            type="button"
            className={`reaction ${r.me ? "mine" : ""}`}
            aria-pressed={r.me}
            aria-label={`${r.name}: ${r.count}`}
            title={`${who} reacted with :${r.name}:`}
            disabled={!user}
            onClick={() => toggle(r.name)}
          >
            <span aria-hidden="true">{EMOJI[r.name]}</span> {r.count}
          </button>
        );
      })}
      {user && (
        <Popover.Root>
          <Popover.Trigger asChild>
            <button type="button" className="reaction add" aria-label="Add a reaction" title="Add a reaction">
              <SmilePlus size={14} />
            </button>
          </Popover.Trigger>
          <Popover.Portal>
            <Popover.Content className="reaction-picker" sideOffset={4} align="start">
              {Object.entries(EMOJI).map(([name, e]) => (
                <Popover.Close asChild key={name}>
                  <button type="button" aria-label={name} title={`:${name}:`} onClick={() => toggle(name)}>
                    {e}
                  </button>
                </Popover.Close>
              ))}
            </Popover.Content>
          </Popover.Portal>
        </Popover.Root>
      )}
    </div>
  );
}
