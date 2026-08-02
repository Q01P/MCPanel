import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ServerOverview } from "./types";

vi.mock("./api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./api")>();
  return {
    ...actual,
    listServers: vi.fn(async () => []),
    addServer: vi.fn(async () => ({ id: 42 }) as never),
    updateServer: vi.fn(async () => {}),
    setServerSecret: vi.fn(async () => {}),
    removeServer: vi.fn(async () => {}),
  };
});

import * as api from "./api";
import { resetLogBatching, useLogs } from "./logs";
import { usePanel } from "./store";

const overview = (id: number): ServerOverview => ({
  id,
  name: `srv-${id}`,
  command: "true",
  args: [],
  env: {},
  cwd: null,
  auto_start: false,
  status: { state: "stopped" },
});

beforeEach(() => {
  vi.useFakeTimers();
  vi.mocked(api.listServers).mockClear();
  vi.mocked(api.listServers).mockResolvedValue([]);
  vi.mocked(api.addServer).mockClear();
  vi.mocked(api.removeServer).mockClear();
  resetLogBatching();
  useLogs.setState({ byServer: {}, selected: null, laggedMissed: 0 });
  usePanel.setState({ servers: [], loaded: false, loadFailed: false, error: null });
});

afterEach(() => {
  vi.useRealTimers();
});

describe("applyEvent", () => {
  it("patches only the matching server's status", () => {
    usePanel.setState({ servers: [overview(1), overview(2)] });

    usePanel.getState().applyEvent({
      type: "status_changed",
      server_id: 2,
      status: { state: "running" },
    });

    const [first, second] = usePanel.getState().servers;
    expect(first?.status).toEqual({ state: "stopped" });
    expect(second?.status).toEqual({ state: "running" });
  });

  it("ignores log events entirely", () => {
    usePanel.setState({ servers: [overview(1)] });
    usePanel.getState().applyEvent({
      type: "log",
      server_id: 1,
      stream: "stdout",
      line: "noise",
    });
    expect(usePanel.getState().servers[0]?.status).toEqual({ state: "stopped" });
  });
});

describe("mutations report success", () => {
  it("add resolves the created record so callers can chain secrets onto it", async () => {
    const record = await usePanel.getState().add({
      name: "fresh",
      command: "true",
      args: [],
      env: {},
      cwd: null,
      auto_start: false,
    });
    expect(record?.id).toBe(42);
  });

  it("add resolves null and keeps an error when the backend rejects", async () => {
    vi.mocked(api.addServer).mockRejectedValueOnce({
      code: "conflict",
      message: "name taken",
    });

    const record = await usePanel.getState().add({
      name: "dupe",
      command: "true",
      args: [],
      env: {},
      cwd: null,
      auto_start: false,
    });

    expect(record).toBeNull();
    expect(usePanel.getState().error).toBe("name taken");
  });

  it("setSecret failure surfaces the error and resolves false", async () => {
    vi.mocked(api.setServerSecret).mockRejectedValueOnce({
      code: "keyring",
      message: "store locked",
    });
    const ok = await usePanel.getState().setSecret(1, "TOKEN", "v");
    expect(ok).toBe(false);
    expect(usePanel.getState().error).toBe("store locked");
  });

  it("remove drops the server's log bucket on success", async () => {
    useLogs.setState({ byServer: { 7: [] }, selected: 7, laggedMissed: 0 });

    const ok = await usePanel.getState().remove(7);

    expect(ok).toBe(true);
    expect(useLogs.getState().byServer[7]).toBeUndefined();
    expect(useLogs.getState().selected).toBeNull();
  });

  it("remove keeps the log bucket when the backend rejects", async () => {
    vi.mocked(api.removeServer).mockRejectedValueOnce({
      code: "db",
      message: "locked",
    });
    useLogs.setState({ byServer: { 7: [] }, selected: 7, laggedMissed: 0 });

    const ok = await usePanel.getState().remove(7);

    expect(ok).toBe(false);
    expect(useLogs.getState().byServer[7]).toEqual([]);
  });
});

describe("load failure", () => {
  it("is distinguishable from an empty server list", async () => {
    vi.mocked(api.listServers).mockRejectedValueOnce({
      code: "db",
      message: "cannot open database",
    });

    await usePanel.getState().load();

    expect(usePanel.getState().loaded).toBe(true);
    expect(usePanel.getState().loadFailed).toBe(true);
    expect(usePanel.getState().error).toBe("cannot open database");

    // A later successful load clears the failure flag.
    await usePanel.getState().load();
    expect(usePanel.getState().loadFailed).toBe(false);
  });
});

describe("resync", () => {
  it("debounces a burst of lagged markers into one list fetch", async () => {
    usePanel.getState().applyEvent({ type: "lagged", missed: 1 });
    usePanel.getState().applyEvent({ type: "lagged", missed: 2 });
    usePanel.getState().applyEvent({ type: "lagged", missed: 3 });

    expect(api.listServers).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(250);
    expect(api.listServers).toHaveBeenCalledTimes(1);
  });
});
