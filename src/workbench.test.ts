import { describe, expect, it } from "vitest";
import {
  HISTORY_STORAGE_KEY,
  MAX_TIMEOUT_S,
  TEMPLATES,
  loadHistory,
  saveHistory,
  useWorkbench,
} from "./workbench";

describe("setTimeoutS clamping", () => {
  it("clamps to the gateway's 1..=300 range", () => {
    useWorkbench.getState().setTimeoutS(500);
    expect(useWorkbench.getState().timeoutS).toBe(MAX_TIMEOUT_S);

    useWorkbench.getState().setTimeoutS(-5);
    expect(useWorkbench.getState().timeoutS).toBe(1);

    useWorkbench.getState().setTimeoutS(0);
    expect(useWorkbench.getState().timeoutS).toBe(1);
  });

  it("rounds fractional input; null and NaN mean the server's own timeout", () => {
    useWorkbench.getState().setTimeoutS(5.6);
    expect(useWorkbench.getState().timeoutS).toBe(6);

    useWorkbench.getState().setTimeoutS(null);
    expect(useWorkbench.getState().timeoutS).toBeNull();
    useWorkbench.getState().setTimeoutS(Number.NaN);
    expect(useWorkbench.getState().timeoutS).toBeNull();
  });
});

describe("templates", () => {
  it("defaults the body to the tools/list template", () => {
    // The default is looked up by array position; this pins the intent so a
    // template reorder can't silently change it.
    expect(TEMPLATES[1]?.label).toBe("tools/list");
    expect(useWorkbench.getInitialState().body).toContain("tools/list");
  });

  it("every template is valid JSON-RPC", () => {
    for (const template of TEMPLATES) {
      const parsed = JSON.parse(template.body) as { jsonrpc: string; method: string };
      expect(parsed.jsonrpc).toBe("2.0");
      expect(parsed.method.length).toBeGreaterThan(0);
    }
  });
});

describe("send", () => {
  it("rejects invalid JSON before any network call", async () => {
    useWorkbench.setState({ serverId: 1, body: "{not json", pending: false });
    await useWorkbench.getState().send("srv");

    expect(useWorkbench.getState().response).toMatch(/^not valid JSON/);
    expect(useWorkbench.getState().pending).toBe(false);
  });

  it("does nothing without a selected server", async () => {
    useWorkbench.setState({ serverId: null, response: null });
    await useWorkbench.getState().send("srv");
    expect(useWorkbench.getState().response).toBeNull();
  });
});

describe("history persistence", () => {
  it("round-trips through localStorage, dropping malformed entries", () => {
    window.localStorage.removeItem(HISTORY_STORAGE_KEY);
    expect(loadHistory()).toEqual([]);

    const entry = { seq: 3, serverId: 1, serverName: "srv", body: "{}", at: "10:00:00" };
    saveHistory([entry]);
    expect(loadHistory()).toEqual([entry]);

    window.localStorage.setItem(
      HISTORY_STORAGE_KEY,
      JSON.stringify([entry, { seq: "nope" }, null, "junk"]),
    );
    expect(loadHistory()).toEqual([entry]);

    window.localStorage.setItem(HISTORY_STORAGE_KEY, "{not json");
    expect(loadHistory()).toEqual([]);
    window.localStorage.removeItem(HISTORY_STORAGE_KEY);
  });

  it("addHistory persists and clearHistory empties the store and storage", () => {
    useWorkbench.setState({ history: [] });
    useWorkbench.getState().addHistory(1, "srv", '{"method":"ping"}');
    expect(loadHistory()[0]?.body).toBe('{"method":"ping"}');

    useWorkbench.getState().clearHistory();
    expect(useWorkbench.getState().history).toEqual([]);
    expect(loadHistory()).toEqual([]);
  });
});
