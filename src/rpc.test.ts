import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./api")>()),
  gatewayInfo: vi.fn(async () => ({ url: "http://127.0.0.1:1", token: "tok" })),
}));

import { effectiveTimeoutS, envelope, resetGatewayCache, rpc } from "./rpc";
import { usePanel } from "./store";

const fetchMock = vi.fn<typeof fetch>();

function reply(status: number, body: unknown) {
  fetchMock.mockResolvedValueOnce(
    new Response(typeof body === "string" ? body : JSON.stringify(body), { status }),
  );
}

beforeEach(() => {
  resetGatewayCache();
  fetchMock.mockReset();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("rpc", () => {
  it("posts a JSON-RPC envelope with the bearer token and timeout", async () => {
    reply(200, { jsonrpc: "2.0", id: 1, result: { ok: true } });

    const outcome = await rpc(4, "tools/list", {}, 12);

    expect(outcome).toEqual({ kind: "result", result: { ok: true } });
    const [url, init] = fetchMock.mock.calls[0] ?? [];
    expect(url).toBe("http://127.0.0.1:1/mcp/4?timeout_s=12");
    const headers = init?.headers as Record<string, string> | undefined;
    expect(headers?.Authorization).toBe("Bearer tok");
    expect(JSON.parse(String(init?.body))).toEqual(envelope("tools/list", {}));
  });

  it("omits the timeout query when deferring to the server's own", async () => {
    reply(200, { jsonrpc: "2.0", id: 1, result: {} });
    await rpc(4, "ping", {}, null);
    expect(fetchMock.mock.calls[0]?.[0]).toBe("http://127.0.0.1:1/mcp/4");
  });

  it("derives the client-side bound from the server's timeout when none is given", () => {
    usePanel.setState({
      servers: [
        {
          id: 4,
          name: "slow",
          command: "x",
          args: [],
          env: {},
          cwd: null,
          auto_start: false,
          request_timeout_s: 900,
          restart_on_crash: false,
          status: { state: "running" },
          handshake: null,
        },
      ],
    });
    expect(effectiveTimeoutS(4, null)).toBe(900);
    expect(effectiveTimeoutS(4, 12)).toBe(12);
    expect(effectiveTimeoutS(5, null)).toBe(30);
    usePanel.setState({ servers: [] });
  });

  it("classifies a JSON-RPC error envelope as the server's error", async () => {
    reply(200, { jsonrpc: "2.0", id: 1, error: { code: -32601, message: "method not found" } });
    expect(await rpc(4, "nope", {}, 5)).toEqual({
      kind: "error",
      code: -32601,
      message: "method not found",
    });
  });

  it("classifies an HTTP error as transport, carrying the gateway's message", async () => {
    reply(404, { code: "server_not_found", message: "server not found: 4" });
    expect(await rpc(4, "ping", {}, 5)).toEqual({
      kind: "transport",
      message: "HTTP 404: server not found: 4",
    });
  });

  it("classifies a network failure as transport", async () => {
    fetchMock.mockRejectedValueOnce(new TypeError("Failed to fetch"));
    expect(await rpc(4, "ping", {}, 5)).toEqual({
      kind: "transport",
      message: "Failed to fetch",
    });
  });

  it("does not mistake a non-JSON body for a result", async () => {
    reply(502, "bad gateway");
    const outcome = await rpc(4, "ping", {}, 5);
    expect(outcome.kind).toBe("transport");
    expect(outcome.kind === "transport" && outcome.message).toContain("502");
  });
});
