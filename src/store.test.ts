import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ServerOverview } from "./types";

vi.mock("./api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./api")>();
  return { ...actual, listServers: vi.fn(async () => []) };
});

import * as api from "./api";
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
  usePanel.setState({ servers: [], loaded: false, error: null });
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
