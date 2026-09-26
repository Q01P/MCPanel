import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AppEvent } from "./types";
import { LOG_CAP, logEntryFromNotification, resetLogBatching, useLogs } from "./logs";

const line = (id: number, text: string): AppEvent => ({
  type: "log",
  server_id: id,
  stream: "stdout",
  line: text,
});

beforeEach(() => {
  vi.useFakeTimers();
  resetLogBatching();
  useLogs.setState({ byServer: {}, selected: null, laggedMissed: 0 });
});

afterEach(() => {
  vi.useRealTimers();
});

describe("useLogs.ingest", () => {
  it("batches appends on the flush timer instead of per event", () => {
    useLogs.getState().ingest(line(1, "first"));
    useLogs.getState().ingest(line(1, "second"));
    expect(useLogs.getState().byServer[1]).toBeUndefined();

    vi.advanceTimersByTime(100);
    const entries = useLogs.getState().byServer[1];
    expect(entries?.map((e) => e.text)).toEqual(["first", "second"]);
  });

  it("caps each server's buffer at LOG_CAP, keeping the newest lines", () => {
    for (let i = 0; i < LOG_CAP + 10; i++) {
      useLogs.getState().ingest(line(1, `l${i}`));
    }
    vi.advanceTimersByTime(100);

    const entries = useLogs.getState().byServer[1];
    expect(entries).toHaveLength(LOG_CAP);
    expect(entries?.[0]?.text).toBe("l10");
    expect(entries?.at(-1)?.text).toBe(`l${LOG_CAP + 9}`);
  });

  it("renders log_gap events as gap markers", () => {
    useLogs.getState().ingest({
      type: "log_gap",
      server_id: 1,
      stream: "stderr",
      dropped: 42,
    });
    vi.advanceTimersByTime(100);

    const entry = useLogs.getState().byServer[1]?.[0];
    expect(entry?.kind).toBe("gap");
    expect(entry?.text).toContain("42");
  });

  it("turns notifications/message into a levelled mcp entry, ignoring other notifications", () => {
    useLogs.getState().ingest({
      type: "notification",
      server_id: 1,
      payload: {
        jsonrpc: "2.0",
        method: "notifications/message",
        params: { level: "warning", logger: "db", data: "slow query" },
      },
    });
    useLogs.getState().ingest({
      type: "notification",
      server_id: 1,
      payload: { jsonrpc: "2.0", method: "notifications/tools/list_changed" },
    });
    vi.advanceTimersByTime(100);

    const entries = useLogs.getState().byServer[1];
    expect(entries).toHaveLength(1);
    expect(entries?.[0]).toMatchObject({
      kind: "line",
      stream: "mcp",
      level: "warning",
      text: "[warning] db: slow query",
    });
  });

  it("renders notification_gap events as gap markers", () => {
    useLogs.getState().ingest({ type: "notification_gap", server_id: 1, dropped: 9 });
    vi.advanceTimersByTime(100);
    const entry = useLogs.getState().byServer[1]?.[0];
    expect(entry?.kind).toBe("gap");
    expect(entry?.stream).toBe("mcp");
    expect(entry?.text).toContain("9 notifications");
  });

  it("accumulates lagged markers without touching the buffers", () => {
    useLogs.getState().ingest({ type: "lagged", missed: 3 });
    useLogs.getState().ingest({ type: "lagged", missed: 4 });
    expect(useLogs.getState().laggedMissed).toBe(7);
    expect(useLogs.getState().byServer).toEqual({});
  });
});

describe("useLogs.drop", () => {
  it("removes the bucket and clears a matching selection", () => {
    useLogs.getState().ingest(line(1, "kept"));
    vi.advanceTimersByTime(100);
    useLogs.getState().select(1);

    useLogs.getState().drop(1);
    expect(useLogs.getState().byServer[1]).toBeUndefined();
    expect(useLogs.getState().selected).toBeNull();
  });

  it("leaves an unrelated selection alone", () => {
    useLogs.getState().select(2);
    useLogs.getState().drop(1);
    expect(useLogs.getState().selected).toBe(2);
  });

  it("discards in-flight pending entries for the dropped server", () => {
    useLogs.getState().ingest(line(1, "in flight"));
    useLogs.getState().drop(1); // before the 100ms flush fires
    vi.advanceTimersByTime(100);
    expect(useLogs.getState().byServer[1]).toBeUndefined();
  });

  it("ignores late events for a dropped server instead of resurrecting it", () => {
    useLogs.getState().ingest(line(1, "before"));
    vi.advanceTimersByTime(100);
    useLogs.getState().drop(1);

    // The removed server's final stop output arrives after the drop.
    useLogs.getState().ingest(line(1, "posthumous"));
    vi.advanceTimersByTime(100);
    expect(useLogs.getState().byServer[1]).toBeUndefined();
  });
});

describe("logEntryFromNotification", () => {
  it("serializes structured data compactly and defaults an unknown level to info", () => {
    const entry = logEntryFromNotification({
      method: "notifications/message",
      params: { level: "loud", data: { ok: true, n: 2 } },
    });
    expect(entry).toEqual({ stream: "mcp", level: "info", text: '[info] {"ok":true,"n":2}' });
  });

  it("copes with missing params and rejects non-log methods", () => {
    expect(logEntryFromNotification({ method: "notifications/message" })).toEqual({
      stream: "mcp",
      level: "info",
      text: "[info] null",
    });
    expect(logEntryFromNotification({ method: "notifications/progress" })).toBeNull();
    expect(logEntryFromNotification("junk")).toBeNull();
    expect(logEntryFromNotification(null)).toBeNull();
  });
});
