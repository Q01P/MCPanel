import { create } from "zustand";
import { describeError } from "./api";
import { MAX_TIMEOUT_S, postRaw } from "./rpc";

export interface HistoryEntry {
  seq: number;
  serverId: number;
  serverName: string;
  body: string;
  at: string; // HH:MM:SS
}

const HISTORY_CAP = 50;
/** localStorage key; bump when the entry shape changes. */
export const HISTORY_STORAGE_KEY = "mcpanel.history.v1";
export { MAX_TIMEOUT_S };

/** History survives restarts: the edit → restart → retest loop is the
 * whole point, and losing every request on relaunch was the complaint.
 * Storage failures (private mode, quota) are swallowed — history is a
 * convenience, never a reason to fail a send. */
export function loadHistory(): HistoryEntry[] {
  try {
    const raw = window.localStorage.getItem(HISTORY_STORAGE_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter(
        (entry): entry is HistoryEntry =>
          !!entry &&
          typeof entry === "object" &&
          typeof (entry as HistoryEntry).seq === "number" &&
          typeof (entry as HistoryEntry).serverId === "number" &&
          typeof (entry as HistoryEntry).serverName === "string" &&
          typeof (entry as HistoryEntry).body === "string" &&
          typeof (entry as HistoryEntry).at === "string",
      )
      .slice(0, HISTORY_CAP);
  } catch {
    return [];
  }
}

export function saveHistory(history: HistoryEntry[]): void {
  try {
    window.localStorage.setItem(HISTORY_STORAGE_KEY, JSON.stringify(history));
  } catch {
    // Not persisted this time; the in-memory list still works.
  }
}

/** The backend's built-in per-request timeout, shown as the placeholder
 * for a server that has not set its own. */
export const DEFAULT_TIMEOUT_S = 30;
const initialHistory = loadHistory();
let nextSeq = initialHistory.reduce((max, entry) => Math.max(max, entry.seq + 1), 0);

/** Request templates — the "Postman" starting points. `id` is a placeholder;
 * the gateway re-correlates and echoes it back. */
export const TEMPLATES: { label: string; body: string }[] = [
  {
    label: "ping",
    body: `{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "ping",
  "params": {}
}`,
  },
  {
    label: "tools/list",
    body: `{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/list",
  "params": {}
}`,
  },
  {
    label: "tools/call",
    body: `{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "tool-name",
    "arguments": {}
  }
}`,
  },
  {
    label: "resources/list",
    body: `{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "resources/list",
  "params": {}
}`,
  },
  {
    label: "prompts/list",
    body: `{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "prompts/list",
  "params": {}
}`,
  },
];

/** Which face of the workbench is showing: one of the browsers, or the
 * raw JSON-RPC editor they can hand requests to. */
export type WorkbenchMode = "tools" | "resources" | "prompts" | "raw";

interface WorkbenchState {
  serverId: number | null;
  mode: WorkbenchMode;
  body: string;
  response: string | null;
  pending: boolean;
  /** Per-request timeout override in seconds; null defers to the target
   * server's own setting. Slow tools are the point, not an error. */
  timeoutS: number | null;
  history: HistoryEntry[];
  setServer: (id: number | null) => void;
  setMode: (mode: WorkbenchMode) => void;
  setBody: (body: string) => void;
  setTimeoutS: (seconds: number | null) => void;
  restore: (entry: HistoryEntry) => void;
  /** Restore and send in one go — a one-click re-run. Targets the server
   * by its current id; a stopped one fails the send like any other. */
  rerun: (entry: HistoryEntry) => Promise<void>;
  clearHistory: () => void;
  /** Record a request so it can be replayed from the raw editor — the tools
   * browser records its calls here too, as the JSON-RPC they amount to. */
  addHistory: (serverId: number, serverName: string, body: string) => void;
  send: (serverName: string) => Promise<void>;
}

export const useWorkbench = create<WorkbenchState>((set, get) => ({
  serverId: null,
  mode: "tools",
  body: TEMPLATES[1].body, // tools/list — the most useful first probe
  response: null,
  pending: false,
  timeoutS: null,
  history: initialHistory,

  setServer: (id) => set({ serverId: id }),
  setMode: (mode) => set({ mode }),
  setBody: (body) => set({ body }),
  setTimeoutS: (seconds) =>
    set({
      timeoutS:
        seconds == null || !Number.isFinite(seconds)
          ? null
          : Math.min(Math.max(Math.round(seconds) || 1, 1), MAX_TIMEOUT_S),
    }),
  restore: (entry) => set({ serverId: entry.serverId, body: entry.body }),

  rerun: async (entry) => {
    get().restore(entry);
    set({ mode: "raw" });
    await get().send(entry.serverName);
  },

  clearHistory: () => {
    set({ history: [] });
    saveHistory([]);
  },

  addHistory: (serverId, serverName, body) => {
    const history = [
      { seq: nextSeq++, serverId, serverName, body, at: new Date().toLocaleTimeString() },
      ...get().history,
    ].slice(0, HISTORY_CAP);
    set({ history });
    saveHistory(history);
  },

  send: async (serverName) => {
    const { serverId, body, timeoutS } = get();
    if (serverId == null || get().pending) return;

    let payload: unknown;
    try {
      payload = JSON.parse(body);
    } catch (error) {
      set({ response: `not valid JSON: ${describeError(error)}` });
      return;
    }

    set({ pending: true, response: null });
    try {
      const { ok, status, text } = await postRaw(serverId, payload, timeoutS);
      let pretty = text;
      try {
        pretty = JSON.stringify(JSON.parse(text), null, 2);
      } catch {
        // non-JSON body (shouldn't happen) — show raw
      }
      set({ response: ok ? pretty : `HTTP ${status}\n${pretty}` });
      get().addHistory(serverId, serverName, body);
    } catch (error) {
      set({ response: `request failed: ${describeError(error)}` });
    } finally {
      set({ pending: false });
    }
  },
}));
