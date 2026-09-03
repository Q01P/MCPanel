import { create } from "zustand";
import { describeError } from "./api";
import { postRaw } from "./rpc";

export interface HistoryEntry {
  seq: number;
  serverId: number;
  serverName: string;
  body: string;
  at: string; // HH:MM:SS
}

const HISTORY_CAP = 20;
/** Gateway cap on `?timeout_s=` — mirrored from the backend. */
export const MAX_TIMEOUT_S = 300;
export const DEFAULT_TIMEOUT_S = 30;
let nextSeq = 0;

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

/** Which face of the workbench is showing: the tools browser, or the raw
 * JSON-RPC editor it can hand requests to. */
export type WorkbenchMode = "tools" | "raw";

interface WorkbenchState {
  serverId: number | null;
  mode: WorkbenchMode;
  body: string;
  response: string | null;
  pending: boolean;
  /** Per-request timeout in seconds; slow tools are the point, not an error. */
  timeoutS: number;
  history: HistoryEntry[];
  setServer: (id: number | null) => void;
  setMode: (mode: WorkbenchMode) => void;
  setBody: (body: string) => void;
  setTimeoutS: (seconds: number) => void;
  restore: (entry: HistoryEntry) => void;
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
  timeoutS: DEFAULT_TIMEOUT_S,
  history: [],

  setServer: (id) => set({ serverId: id }),
  setMode: (mode) => set({ mode }),
  setBody: (body) => set({ body }),
  setTimeoutS: (seconds) =>
    set({ timeoutS: Math.min(Math.max(Math.round(seconds) || 1, 1), MAX_TIMEOUT_S) }),
  restore: (entry) => set({ serverId: entry.serverId, body: entry.body }),

  addHistory: (serverId, serverName, body) =>
    set({
      history: [
        { seq: nextSeq++, serverId, serverName, body, at: new Date().toLocaleTimeString() },
        ...get().history,
      ].slice(0, HISTORY_CAP),
    }),

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
