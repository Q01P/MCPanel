import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./rpc", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./rpc")>()),
  rpc: vi.fn(),
}));

import { buildArguments, describePromptResult, getBody, usePrompts } from "./prompts";
import { rpc } from "./rpc";
import { useWorkbench } from "./workbench";

const rpcMock = vi.mocked(rpc);

const PROMPT = {
  name: "summarize",
  description: "Summarize text",
  arguments: [
    { name: "text", required: true },
    { name: "style", description: "tone" },
  ],
};

describe("buildArguments", () => {
  it("omits empty optionals and rejects empty required arguments", () => {
    expect(buildArguments(PROMPT, { text: "hi", style: "" })).toEqual({
      ok: true,
      arguments: { text: "hi" },
    });
    expect(buildArguments(PROMPT, { style: "terse" })).toEqual({
      ok: false,
      errors: { text: "required" },
    });
    expect(buildArguments({ name: "bare" }, {})).toEqual({ ok: true, arguments: {} });
  });
});

describe("describePromptResult", () => {
  it("renders text messages as text and other content as labelled JSON", () => {
    const view = describePromptResult({
      description: "d",
      messages: [
        { role: "user", content: { type: "text", text: "hello" } },
        { role: "assistant", content: { type: "image", data: "AA", mimeType: "image/png" } },
      ],
    });
    expect(view.description).toBe("d");
    expect(view.messages).toEqual([
      { id: 0, role: "user", kind: "text", body: "hello" },
      {
        id: 1,
        role: "assistant",
        kind: "json",
        label: "image",
        body: JSON.stringify({ type: "image", data: "AA", mimeType: "image/png" }, null, 2),
      },
    ]);
  });

  it("falls back to raw JSON for a non-prompt shape", () => {
    expect(describePromptResult({ weird: true }).messages[0]?.kind).toBe("json");
  });
});

describe("getBody", () => {
  it("is the JSON-RPC envelope the raw workbench can replay", () => {
    const parsed = JSON.parse(getBody("summarize", { text: "x" }));
    expect(parsed.method).toBe("prompts/get");
    expect(parsed.params).toEqual({ name: "summarize", arguments: { text: "x" } });
  });
});

describe("usePrompts store", () => {
  beforeEach(() => {
    rpcMock.mockReset();
    usePrompts.setState({ ...usePrompts.getInitialState() });
    useWorkbench.setState({ history: [] });
  });

  it("lists, validates before getting, then gets and records history", async () => {
    rpcMock.mockResolvedValueOnce({ kind: "result", result: { prompts: [PROMPT] } });
    await usePrompts.getState().load(5);
    expect(usePrompts.getState().prompts.map((p) => p.name)).toEqual(["summarize"]);

    usePrompts.getState().select("summarize");
    await usePrompts.getState().get("srv", 30);
    expect(usePrompts.getState().fieldErrors).toEqual({ text: "required" });
    expect(rpcMock).toHaveBeenCalledTimes(1);

    usePrompts.getState().setValue("text", "hello");
    expect(usePrompts.getState().fieldErrors).toEqual({});
    rpcMock.mockResolvedValueOnce({
      kind: "result",
      result: { messages: [{ role: "user", content: { type: "text", text: "Summarize: hello" } }] },
    });
    await usePrompts.getState().get("srv", 30);
    expect(rpcMock).toHaveBeenLastCalledWith(
      5,
      "prompts/get",
      { name: "summarize", arguments: { text: "hello" } },
      30,
    );
    expect(usePrompts.getState().result?.messages[0]?.body).toBe("Summarize: hello");
    expect(useWorkbench.getState().history[0]?.body).toContain("prompts/get");
  });

  it("re-lists on prompts/list_changed, clearing a vanished selection", async () => {
    rpcMock.mockResolvedValueOnce({ kind: "result", result: { prompts: [PROMPT] } });
    await usePrompts.getState().load(5);
    usePrompts.getState().select("summarize");

    rpcMock.mockResolvedValueOnce({ kind: "result", result: { prompts: [{ name: "other" }] } });
    usePrompts.getState().applyEvent({
      type: "notification",
      server_id: 5,
      payload: { method: "notifications/prompts/list_changed" },
    });
    await vi.waitFor(() =>
      expect(usePrompts.getState().prompts.map((p) => p.name)).toEqual(["other"]),
    );
    expect(usePrompts.getState().selected).toBeNull();
  });
});
