import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./rpc", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./rpc")>()),
  rpc: vi.fn(),
}));

import { rpc } from "./rpc";
import {
  describeContents,
  expandTemplate,
  itemKey,
  itemLabel,
  templateVariables,
  useResources,
} from "./resources";
import { useWorkbench } from "./workbench";

const rpcMock = vi.mocked(rpc);

describe("templateVariables / expandTemplate", () => {
  it("extracts names across operators, modifiers, and groups", () => {
    expect(templateVariables("file:///{+path}/{name}.{ext}{?q,limit*}{#frag:3}")).toEqual([
      "path",
      "name",
      "ext",
      "q",
      "limit",
      "frag",
    ]);
    expect(templateVariables("no-vars://x")).toEqual([]);
  });

  it("expands simple, reserved, path, and query forms; empty values vanish", () => {
    const values = { path: "a/b c", name: "n", q: "x y", limit: "" };
    expect(expandTemplate("db://{+path}/{name}{?q,limit}", values)).toBe("db://a/b%20c/n?q=x%20y");
    expect(expandTemplate("db://{path}", values)).toBe("db://a%2Fb%20c");
    expect(expandTemplate("db://root{/path}", values)).toBe("db://root/a%2Fb%20c");
    expect(expandTemplate("db://{name}{#q}", values)).toBe("db://n#x%20y");
    expect(expandTemplate("db://{missing}", {})).toBe("db://");
  });
});

describe("describeContents", () => {
  it("renders text, pretty-prints JSON mime types, and summarizes blobs", () => {
    const views = describeContents({
      contents: [
        { uri: "a://t", mimeType: "text/plain", text: "hello" },
        { uri: "a://j", mimeType: "application/json", text: '{"k":1}' },
        { uri: "a://bad", mimeType: "application/json", text: "{nope" },
        { uri: "a://b", mimeType: "image/png", blob: "AAAA" },
        { uri: "a://weird" },
      ],
    });
    expect(views.map((v) => v.kind)).toEqual(["text", "json", "text", "blob", "json"]);
    expect(views[1]?.body).toBe('{\n  "k": 1\n}');
    expect(views[2]?.body).toBe("{nope");
    expect(views[3]?.body).toContain("3 bytes");
    expect(views[3]?.body).toContain("image/png");
  });

  it("shows a non-MCP result as JSON and an empty list as a note", () => {
    expect(describeContents({ other: 1 })[0]?.kind).toBe("json");
    expect(describeContents({ contents: [] })[0]?.body).toBe("(no contents)");
  });
});

describe("itemKey / itemLabel", () => {
  it("keys resources and templates apart and prefers title, name, then uri", () => {
    const resource = { kind: "resource" as const, resource: { uri: "x://1", name: "one" } };
    const template = { kind: "template" as const, template: { uriTemplate: "x://{id}" } };
    expect(itemKey(resource)).toBe("r:x://1");
    expect(itemKey(template)).toBe("t:x://{id}");
    expect(itemLabel(resource)).toBe("one");
    expect(itemLabel(template)).toBe("x://{id}");
    expect(itemLabel({ kind: "resource", resource: { uri: "u", name: "n", title: "T" } })).toBe("T");
  });
});

describe("useResources store", () => {
  beforeEach(() => {
    rpcMock.mockReset();
    useResources.setState({ ...useResources.getInitialState() });
    useWorkbench.setState({ history: [] });
  });

  it("lists resources and templates together, treating a -32601 on templates as none", async () => {
    rpcMock
      .mockResolvedValueOnce({ kind: "result", result: { resources: [{ uri: "a://1" }] } })
      .mockResolvedValueOnce({ kind: "error", code: -32601, message: "Method not found" });
    await useResources.getState().load(3);
    expect(useResources.getState().items).toEqual([{ kind: "resource", resource: { uri: "a://1" } }]);
    expect(useResources.getState().loadError).toBeNull();

    rpcMock
      .mockResolvedValueOnce({ kind: "result", result: { resources: [] } })
      .mockResolvedValueOnce({
        kind: "result",
        result: { resourceTemplates: [{ uriTemplate: "a://{id}" }] },
      });
    await useResources.getState().load(4);
    expect(useResources.getState().items[0]?.kind).toBe("template");
  });

  it("reads the selected resource, records history, and expands a template first", async () => {
    rpcMock
      .mockResolvedValueOnce({
        kind: "result",
        result: { resources: [{ uri: "a://1" }] },
      })
      .mockResolvedValueOnce({
        kind: "result",
        result: { resourceTemplates: [{ uriTemplate: "a://{id}" }] },
      });
    await useResources.getState().load(3);

    useResources.getState().select("r:a://1");
    expect(useResources.getState().targetUri()).toBe("a://1");
    rpcMock.mockResolvedValueOnce({
      kind: "result",
      result: { contents: [{ uri: "a://1", text: "body" }] },
    });
    await useResources.getState().read("srv", 30);
    expect(rpcMock).toHaveBeenLastCalledWith(3, "resources/read", { uri: "a://1" }, 30);
    expect(useResources.getState().contents?.[0]?.body).toBe("body");
    expect(useWorkbench.getState().history[0]?.body).toContain("resources/read");

    useResources.getState().select("t:a://{id}");
    expect(useResources.getState().contents).toBeNull();
    expect(useResources.getState().targetUri()).toBe("a://");
    useResources.getState().setValue("id", "42");
    rpcMock.mockResolvedValueOnce({ kind: "error", code: -32002, message: "not found" });
    await useResources.getState().read("srv", 30);
    expect(rpcMock).toHaveBeenLastCalledWith(3, "resources/read", { uri: "a://42" }, 30);
    expect(useResources.getState().readError).toMatch(/-32002/);
  });

  it("re-lists on resources/list_changed for the shown server only", async () => {
    rpcMock
      .mockResolvedValueOnce({ kind: "result", result: { resources: [{ uri: "a://1" }] } })
      .mockResolvedValueOnce({ kind: "result", result: { resourceTemplates: [] } });
    await useResources.getState().load(3);
    useResources.getState().select("r:a://1");

    useResources.getState().applyEvent({
      type: "notification",
      server_id: 9,
      payload: { method: "notifications/resources/list_changed" },
    });
    expect(rpcMock).toHaveBeenCalledTimes(2);

    rpcMock
      .mockResolvedValueOnce({ kind: "result", result: { resources: [{ uri: "a://2" }] } })
      .mockResolvedValueOnce({ kind: "result", result: { resourceTemplates: [] } });
    useResources.getState().applyEvent({
      type: "notification",
      server_id: 3,
      payload: { method: "notifications/resources/list_changed" },
    });
    await vi.waitFor(() =>
      expect(useResources.getState().items.map(itemKey)).toEqual(["r:a://2"]),
    );
    expect(useResources.getState().selected).toBeNull();
  });
});
