import { create } from "zustand";
import { describeError, gatewayInfo } from "./api";

export interface HistoryEntry {
  seq: number;
  serverId: number;
  serverName: string;
  body: string;
  at: string; // HH:MM:SS
}

const HISTORY_CAP = 20;
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

interface WorkbenchState {
  serverId: number | null;
  body: string;
  response: string | null;
  pending: boolean;
  history: HistoryEntry[];
  setServer: (id: number | null) => void;
  setBody: (body: string) => void;
  restore: (entry: HistoryEntry) => void;
  send: (serverName: string) => Promise<void>;
}

export const useWorkbench = create<WorkbenchState>((set, get) => ({
  serverId: null,
  body: TEMPLATES[1].body, // tools/list — the most useful first probe
  response: null,
  pending: false,
  history: [],

  setServer: (id) => set({ serverId: id }),
  setBody: (body) => set({ body }),
  restore: (entry) => set({ serverId: entry.serverId, body: entry.body }),

  send: async (serverName) => {
    const { serverId, body, history } = get();
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
      const { url, token } = await gatewayInfo();
      const res = await fetch(`${url}/mcp/${serverId}`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${token}`,
        },
        body: JSON.stringify(payload),
      });
      const text = await res.text();
      let pretty = text;
      try {
        pretty = JSON.stringify(JSON.parse(text), null, 2);
      } catch {
        // non-JSON body (shouldn't happen) — show raw
      }
      set({
        response: res.ok ? pretty : `HTTP ${res.status}\n${pretty}`,
        history: [
          {
            seq: nextSeq++,
            serverId,
            serverName,
            body,
            at: new Date().toLocaleTimeString(),
          },
          ...history,
        ].slice(0, HISTORY_CAP),
      });
    } catch (error) {
      set({ response: `request failed: ${describeError(error)}` });
    } finally {
      set({ pending: false });
    }
  },
}));
