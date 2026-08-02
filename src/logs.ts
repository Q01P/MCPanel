import { create } from "zustand";
import type { AppEvent, LogStreamName } from "./types";

export interface LogEntry {
  seq: number;
  kind: "line" | "gap";
  stream: LogStreamName;
  text: string;
}

/** Ring-buffer cap per server — the backend already bounds the flow; this
 * bounds what the webview retains. */
export const LOG_CAP = 5000;

let nextSeq = 0;
let pending: { id: number; entry: LogEntry }[] = [];
let flushTimer: number | undefined;
/** Ids whose buckets were dropped. A removed server's final stop events
 * cross the wire *after* the drop, and in-flight entries sit in `pending` —
 * without a tombstone either path would silently recreate the bucket and
 * leak it for the session. Server ids are AUTOINCREMENT and never reused,
 * so tombstones can be permanent. */
const dropped = new Set<number>();

/** Test-only: the batching state above outlives the store between test
 * cases; reset it so a pending flush can't leak across tests. */
export function resetLogBatching() {
  window.clearTimeout(flushTimer);
  flushTimer = undefined;
  pending = [];
  nextSeq = 0;
  dropped.clear();
}

interface LogsState {
  byServer: Record<number, LogEntry[]>;
  selected: number | null;
  /** Total events this SSE subscriber missed (gateway `lagged` markers). */
  laggedMissed: number;
  select: (id: number | null) => void;
  drop: (id: number) => void;
  ingest: (event: AppEvent) => void;
}

export const useLogs = create<LogsState>((set, get) => ({
  byServer: {},
  selected: null,
  laggedMissed: 0,

  select: (id) => set({ selected: id }),

  drop: (id) => {
    dropped.add(id);
    pending = pending.filter((p) => p.id !== id);
    const { [id]: _removed, ...rest } = get().byServer;
    set({
      byServer: rest,
      selected: get().selected === id ? null : get().selected,
    });
  },

  // Lines arrive at whatever rate servers produce them; appends are batched
  // on a 100ms timer so the store updates (and re-renders) stay bounded.
  ingest: (event) => {
    if (event.type === "lagged") {
      set({ laggedMissed: get().laggedMissed + event.missed });
      return;
    }
    if (event.type !== "log" && event.type !== "log_gap") return;
    if (dropped.has(event.server_id)) return;

    pending.push({
      id: event.server_id,
      entry:
        event.type === "log"
          ? { seq: nextSeq++, kind: "line", stream: event.stream, text: event.line }
          : {
              seq: nextSeq++,
              kind: "gap",
              stream: event.stream,
              text: `· ${event.dropped} ${event.stream} lines dropped under pressure ·`,
            },
    });

    if (flushTimer === undefined) {
      flushTimer = window.setTimeout(() => {
        flushTimer = undefined;
        const batch = pending;
        pending = [];
        set((state) => {
          const grouped = new Map<number, LogEntry[]>();
          for (const { id, entry } of batch) {
            const bucket = grouped.get(id);
            if (bucket) {
              bucket.push(entry);
            } else {
              grouped.set(id, [entry]);
            }
          }
          const byServer = { ...state.byServer };
          for (const [id, entries] of grouped) {
            const merged = [...(byServer[id] ?? []), ...entries];
            byServer[id] =
              merged.length > LOG_CAP ? merged.slice(merged.length - LOG_CAP) : merged;
          }
          return { byServer };
        });
      }, 100);
    }
  },
}));
