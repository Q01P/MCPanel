import { create } from "zustand";
import type { AppEvent, LogStreamName } from "./types";

/** Where a line came from: a stdio pipe, or the MCP logging channel
 * (`notifications/message`), which well-behaved servers use instead of
 * printing to stderr. */
export type LogSource = LogStreamName | "mcp";

/** MCP logging levels (RFC 5424 names, as the spec lists them). */
export type LogLevel =
  | "debug"
  | "info"
  | "notice"
  | "warning"
  | "error"
  | "critical"
  | "alert"
  | "emergency";

export interface LogEntry {
  seq: number;
  kind: "line" | "gap";
  stream: LogSource;
  text: string;
  /** Only MCP log messages carry a level; stdio lines have none. */
  level?: LogLevel;
}

const LEVELS: readonly LogLevel[] = [
  "debug",
  "info",
  "notice",
  "warning",
  "error",
  "critical",
  "alert",
  "emergency",
];

/** `notifications/message` params → a log entry, or null for any other
 * notification. `data` is arbitrary JSON per spec; strings show as-is,
 * anything else is compact JSON so structured logs stay one line. */
export function logEntryFromNotification(
  payload: unknown,
): Omit<LogEntry, "seq" | "kind"> | null {
  if (!payload || typeof payload !== "object") return null;
  const { method, params } = payload as { method?: unknown; params?: unknown };
  if (method !== "notifications/message") return null;
  const p = (params && typeof params === "object" ? params : {}) as {
    level?: unknown;
    logger?: unknown;
    data?: unknown;
  };
  const level = LEVELS.includes(p.level as LogLevel) ? (p.level as LogLevel) : "info";
  const logger = typeof p.logger === "string" && p.logger !== "" ? `${p.logger}: ` : "";
  const data = typeof p.data === "string" ? p.data : JSON.stringify(p.data ?? null);
  return { stream: "mcp", level, text: `[${level}] ${logger}${data}` };
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
    let entry: LogEntry;
    switch (event.type) {
      case "log":
        entry = { seq: nextSeq++, kind: "line", stream: event.stream, text: event.line };
        break;
      case "log_gap":
        entry = {
          seq: nextSeq++,
          kind: "gap",
          stream: event.stream,
          text: `· ${event.dropped} ${event.stream} lines dropped under pressure ·`,
        };
        break;
      case "notification": {
        const parsed = logEntryFromNotification(event.payload);
        if (!parsed) return; // not a log message — other listeners own it
        entry = { seq: nextSeq++, kind: "line", ...parsed };
        break;
      }
      case "notification_gap":
        entry = {
          seq: nextSeq++,
          kind: "gap",
          stream: "mcp",
          text: `· ${event.dropped} notifications dropped under pressure ·`,
        };
        break;
      default:
        return;
    }
    if (dropped.has(event.server_id)) return;
    pending.push({ id: event.server_id, entry });

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
