import { useEffect, useRef } from "react";
import { useQueryClient } from "@tanstack/react-query";

/** A server event (see `events.rs`): `push`, `merge_request`, `pipeline`, `job`, `job_trace`, ... */
export type LiveEvent = {
  type: string;
  project_id: number;
  pipeline_id?: number;
  job_id?: number;
  iid?: number;
  status?: string;
  trace_size?: number;
};

type Listener = (ev: LiveEvent) => void;

// One SSE stream per tab, for every project the open views watch (browsers
// allow ~6 connections per site). It reconnects when the set changes.
const listeners = new Map<number, Set<Listener>>();
let source: EventSource | null = null;
let connectedFor = "";
let timer: number | null = null;

function reconnect() {
  timer = null;
  const ids = [...listeners.entries()].filter(([, s]) => s.size > 0).map(([id]) => id).sort((a, b) => a - b);
  const key = ids.join(",");
  if (key === connectedFor) return;
  source?.close();
  source = null;
  connectedFor = key;
  if (!ids.length || typeof EventSource === "undefined") return;
  source = new EventSource(`/api/v1/events?projects=${key}`);
  source.onmessage = (m) => {
    try {
      const ev = JSON.parse(m.data) as LiveEvent;
      listeners.get(ev.project_id)?.forEach((f) => f(ev));
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
