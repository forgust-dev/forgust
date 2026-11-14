import { useEffect, useRef } from "react";
import { useQueryClient } from "@tanstack/react-query";
import type { S } from "../api/client";

/** A server event (see `events.rs`): `push`, `merge_request`, `pipeline`, `job`, `job_trace`, ... */
export type LiveEvent = {
  type: string;
  /** The project it happened in (none for the person's own events, see `UserEvent`). */
  project_id?: number;
  pipeline_id?: number;
  job_id?: number;
  iid?: number;
  status?: string;
  trace_size?: number;
};

/** What only the signed-in person hears: a notification that reached their
 * inbox (`screen`: they want it shown as it arrives), or their inbox changed
 * otherwise (read in another tab). */
export type UserEvent = { type: "notification"; screen: boolean; notification: S["Notification"] } | { type: "inbox" };

type Listener = (ev: LiveEvent) => void;

// One SSE stream per tab, for every project the open views watch and the
// signed-in person's own events (browsers allow ~6 connections per site). It
// reconnects when the set changes.
const listeners = new Map<number, Set<Listener>>();
const userListeners = new Set<(ev: UserEvent) => void>();
let source: EventSource | null = null;
let connectedFor = "";
let timer: number | null = null;

function reconnect() {
  timer = null;
  const ids = [...listeners.entries()].filter(([, s]) => s.size > 0).map(([id]) => id).sort((a, b) => a - b);
  const projects = ids.join(",");
  const key = `${userListeners.size ? "me" : ""}:${projects}`;
  if (key === connectedFor) return;
  source?.close();
  source = null;
  connectedFor = key;
  if ((!ids.length && !userListeners.size) || typeof EventSource === "undefined") return;
  source = new EventSource(`/api/v1/events?projects=${projects}`);
  source.onmessage = (m) => {
    try {
      const ev = JSON.parse(m.data) as LiveEvent | UserEvent;
      if (ev.type === "notification" || ev.type === "inbox") userListeners.forEach((f) => f(ev as UserEvent));
      else if ((ev as LiveEvent).project_id != null) listeners.get((ev as LiveEvent).project_id!)?.forEach((f) => f(ev as LiveEvent));
    } catch {
      /* keep-alive or malformed */
    }
  };
}

function schedule() {
  if (timer == null) timer = window.setTimeout(reconnect, 30);
}

/** Call `handler` for every event of `projectId` while mounted. */
export function useProjectEvents(projectId: number | undefined, handler: Listener) {
  const ref = useRef(handler);
  ref.current = handler;
  useEffect(() => {
    if (!projectId) return;
    const f: Listener = (ev) => ref.current(ev);
    if (!listeners.has(projectId)) listeners.set(projectId, new Set());
    listeners.get(projectId)!.add(f);
    schedule();
    return () => {
      listeners.get(projectId)?.delete(f);
      schedule();
    };
  }, [projectId]);
}

/** Call `handler` for what reaches the signed-in person (their inbox changed). */
export function useUserEvents(handler: (ev: UserEvent) => void) {
  const ref = useRef(handler);
  ref.current = handler;
  useEffect(() => {
    const f = (ev: UserEvent) => ref.current(ev);
    userListeners.add(f);
    schedule();
    return () => {
      userListeners.delete(f);
      schedule();
    };
  }, []);
}

/**
 * Refetch this project's CI views when a pipeline or job changes state
 * (bursts are coalesced; log growth is `job_trace` and only log viewers read it).
 */
export function useCiLive(pid: number) {
  const qc = useQueryClient();
  useProjectEvents(pid, (ev) => {
    // one timer per project, whatever the number of views using this: a burst
    // refetches each CI view once (not once per mounted hook)
    if ((ev.type !== "pipeline" && ev.type !== "job") || ciTimers.has(pid)) return;
    ciTimers.set(
      pid,
      window.setTimeout(() => {
        ciTimers.delete(pid);
        for (const key of ["pipelines", "pipeline", "job", "commit-statuses", "mr-pipelines"]) qc.invalidateQueries({ queryKey: [key, pid] });
      }, 250),
    );
  });
}
const ciTimers = new Map<number, number>();
