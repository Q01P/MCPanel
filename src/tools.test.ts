import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./rpc", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./rpc")>()),
  rpc: vi.fn(),
}));

import { rpc } from "./rpc";
import {
  buildArguments,
  callBody,
  describeResult,
  formFromSchema,
  initialValues,
  useTools,
} from "./tools";
import { useWorkbench } from "./workbench";

const rpcMock = vi.mocked(rpc);

const ECHO_SCHEMA = {
  type: "object",
  properties: {
    message: { type: "string", description: "text to echo back" },
    repeat: { type: "integer", default: 1 },
    ratio: { type: "number" },
    shout: { type: "boolean" },
    tone: { type: "string", enum: ["plain", "friendly"] },
    nullable: { type: ["string", "null"] },
    extra: { type: "object" },
  },
  required: ["message"],
};

describe("formFromSchema", () => {
  it("maps each property to a field kind, honouring required", () => {
    const form = formFromSchema(ECHO_SCHEMA);
    expect(form.freeform).toBe(false);
    const kinds = Object.fromEntries(form.fields.map((f) => [f.name, f.kind]));
    expect(kinds).toEqual({
      message: "string",
      repeat: "integer",
      ratio: "number",
      shout: "boolean",
      tone: "enum",
      nullable: "string",
      extra: "json",
    });
    expect(form.fields.find((f) => f.name === "message")?.required).toBe(true);
    expect(form.fields.find((f) => f.name === "repeat")?.required).toBe(false);
    expect(form.fields.find((f) => f.name === "tone")?.options).toEqual(["plain", "friendly"]);
  });

  it("falls back to a freeform JSON object when there is nothing to build from", () => {
    expect(formFromSchema(undefined).freeform).toBe(true);
    expect(formFromSchema({ type: "object" }).freeform).toBe(true);
    expect(formFromSchema({ type: "string" }).freeform).toBe(true);
  });

  it("treats a mixed-type enum as JSON rather than a select of strings", () => {
    const form = formFromSchema({
      type: "object",
      properties: { level: { enum: [1, "two"] } },
    });
    expect(form.fields[0]?.kind).toBe("json");
  });
});

describe("initialValues", () => {
  it("seeds text from schema defaults and booleans as unchecked", () => {
    const values = initialValues(formFromSchema(ECHO_SCHEMA));
    expect(values.repeat).toBe("1");
    expect(values.shout).toBe(false);
    expect(values.message).toBe("");
  });
});

describe("buildArguments", () => {
  const form = formFromSchema(ECHO_SCHEMA);
  const base = { ...initialValues(form), message: "hi" };

  it("coerces text to the schema's types and omits empty optionals", () => {
    const built = buildArguments(form, { ...base, ratio: "0.5", tone: "plain", extra: '{"a":1}' });
    expect(built).toEqual({
      ok: true,
      arguments: { message: "hi", repeat: 1, ratio: 0.5, shout: false, tone: "plain", extra: { a: 1 } },
    });
  });

  it("reports a missing required field instead of sending an empty string", () => {
    const built = buildArguments(form, { ...base, message: "  " });
    expect(built).toEqual({ ok: false, errors: { message: "required" } });
  });

  it("rejects non-numbers and non-integers strictly", () => {
    const built = buildArguments(form, { ...base, repeat: "1.5", ratio: "12abc" });
    expect(built.ok).toBe(false);
    if (!built.ok) {
      expect(built.errors.repeat).toBe("must be an integer");
      expect(built.errors.ratio).toBe("must be a number");
    }
  });

  it("rejects malformed JSON in a json field", () => {
    const built = buildArguments(form, { ...base, extra: "{nope" });
    expect(built).toEqual({ ok: false, errors: { extra: "not valid JSON" } });
  });

  it("freeform: empty means no arguments, and only an object is accepted", () => {
    const freeform = formFromSchema(undefined);
    expect(buildArguments(freeform, {})).toEqual({ ok: true, arguments: {} });
    expect(buildArguments(freeform, { "*": '{"x":1}' })).toEqual({
      ok: true,
      arguments: { x: 1 },
    });
    expect(buildArguments(freeform, { "*": "[1]" })).toEqual({
      ok: false,
      errors: { "*": "arguments must be a JSON object" },
    });
  });
});

describe("describeResult", () => {
  it("renders text content as text and everything else as labelled JSON", () => {
    const view = describeResult({
      content: [
        { type: "text", text: "hello" },
        { type: "image", data: "AAAA", mimeType: "image/png" },
      ],
      structuredContent: { ok: true },
    });
    expect(view.isError).toBe(false);
    expect(view.blocks).toEqual([
      { id: 0, kind: "text", body: "hello" },
      {
        id: 1,
        kind: "json",
        label: "image (image/png)",
        body: JSON.stringify({ type: "image", data: "AAAA", mimeType: "image/png" }, null, 2),
      },
      {
        id: 2,
        kind: "json",
        label: "structuredContent",
        body: JSON.stringify({ ok: true }, null, 2),
      },
    ]);
  });

  it("carries the tool's isError flag", () => {
    expect(describeResult({ content: [{ type: "text", text: "nope" }], isError: true }).isError).toBe(
      true,
    );
  });

  it("shows a non-MCP result as JSON rather than nothing", () => {
    expect(describeResult({ weird: 1 }).blocks).toEqual([
      { id: 0, kind: "json", body: JSON.stringify({ weird: 1 }, null, 2) },
    ]);
    expect(describeResult(null).blocks[0]?.kind).toBe("json");
  });
});

describe("callBody", () => {
  it("is the JSON-RPC envelope the raw workbench can replay", () => {
    const parsed = JSON.parse(callBody("echo", { message: "hi" })) as {
      method: string;
      params: { name: string; arguments: { message: string } };
    };
    expect(parsed.method).toBe("tools/call");
    expect(parsed.params).toEqual({ name: "echo", arguments: { message: "hi" } });
  });
});

describe("useTools store", () => {
  beforeEach(() => {
    rpcMock.mockReset();
    useTools.setState({ ...useTools.getInitialState() });
    useWorkbench.setState({ history: [] });
  });

  it("follows nextCursor across pages and stops when it is absent", async () => {
    rpcMock
      .mockResolvedValueOnce({
        kind: "result",
        result: { tools: [{ name: "a" }], nextCursor: "p2" },
      })
      .mockResolvedValueOnce({ kind: "result", result: { tools: [{ name: "b" }] } });

    await useTools.getState().load(7);

    expect(useTools.getState().tools.map((t) => t.name)).toEqual(["a", "b"]);
    expect(rpcMock).toHaveBeenCalledTimes(2);
    expect(rpcMock.mock.calls[1]?.[2]).toEqual({ cursor: "p2" });
    expect(useTools.getState().loading).toBe(false);
  });

  it("surfaces a list failure and leaves no stale tools", async () => {
    rpcMock.mockResolvedValueOnce({ kind: "transport", message: "HTTP 404: server not found" });
    await useTools.getState().load(7);
    expect(useTools.getState().loadError).toMatch(/404/);
    expect(useTools.getState().tools).toEqual([]);
  });

  it("drops a reply that arrives after the target changed", async () => {
    let resolveFirst: (v: Awaited<ReturnType<typeof rpc>>) => void = () => {};
    rpcMock.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveFirst = resolve;
        }),
    );
    const first = useTools.getState().load(7);
    // Switch away before the first list comes back.
    rpcMock.mockResolvedValueOnce({ kind: "result", result: { tools: [{ name: "fresh" }] } });
    await useTools.getState().load(8);
    resolveFirst({ kind: "result", result: { tools: [{ name: "stale" }] } });
    await first;

    expect(useTools.getState().serverId).toBe(8);
    expect(useTools.getState().tools.map((t) => t.name)).toEqual(["fresh"]);
  });

  it("validates before calling and never hits the network with bad input", async () => {
    useTools.setState({
      serverId: 7,
      tools: [{ name: "echo", inputSchema: ECHO_SCHEMA }],
    });
    useTools.getState().select("echo");
    await useTools.getState().call("srv", 30);

    expect(rpcMock).not.toHaveBeenCalled();
    expect(useTools.getState().fieldErrors).toEqual({ message: "required" });
  });

  it("calls with coerced arguments, renders the result, and records history", async () => {
    useTools.setState({
      serverId: 7,
      tools: [{ name: "echo", inputSchema: ECHO_SCHEMA }],
    });
    useTools.getState().select("echo");
    useTools.getState().setValue("message", "hi");
    useTools.getState().setValue("repeat", "2");
    rpcMock.mockResolvedValueOnce({
      kind: "result",
      result: { content: [{ type: "text", text: "hi hi" }] },
    });

    await useTools.getState().call("srv", 30);

    expect(rpcMock).toHaveBeenCalledWith(
      7,
      "tools/call",
      { name: "echo", arguments: { message: "hi", repeat: 2, shout: false } },
      30,
    );
    expect(useTools.getState().result?.blocks).toEqual([
      { id: 0, kind: "text", body: "hi hi" },
    ]);
    const entry = useWorkbench.getState().history[0];
    expect(entry?.serverName).toBe("srv");
    expect(entry?.body).toContain('"tools/call"');
  });

  it("distinguishes a server-side error from a transport failure", async () => {
    useTools.setState({ serverId: 7, tools: [{ name: "t" }] });
    useTools.getState().select("t");

    rpcMock.mockResolvedValueOnce({ kind: "error", code: -32602, message: "bad params" });
    await useTools.getState().call("srv", 30);
    expect(useTools.getState().callError).toBe("server error -32602: bad params");

    rpcMock.mockResolvedValueOnce({ kind: "transport", message: "request failed" });
    await useTools.getState().call("srv", 30);
    expect(useTools.getState().callError).toBe("request failed");
  });
});
