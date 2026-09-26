import { describe, expect, it } from "vitest";
import {
  capabilityFlag,
  capabilityNames,
  describeHandshake,
  hasCapability,
  serverIdentity,
} from "./handshake";
import type { ServerHandshake } from "./types";

const handshake = (over: Partial<ServerHandshake> = {}): ServerHandshake => ({
  protocol_version: "2025-06-18",
  capabilities: { prompts: {}, tools: { listChanged: true }, experimental: {} },
  server_info: { name: "mock-mcp-server", version: "0.1.0" },
  ...over,
});

describe("capabilityNames", () => {
  it("lists known capabilities in spec order, then the rest", () => {
    expect(capabilityNames(handshake())).toEqual(["tools", "prompts", "experimental"]);
  });

  it("copes with a missing capabilities object", () => {
    expect(capabilityNames(handshake({ capabilities: undefined as never }))).toEqual([]);
  });
});

describe("hasCapability / capabilityFlag", () => {
  it("reads presence and sub-flags", () => {
    expect(hasCapability(handshake(), "tools")).toBe(true);
    expect(hasCapability(handshake(), "resources")).toBe(false);
    expect(hasCapability(null, "tools")).toBe(false);
    expect(capabilityFlag(handshake(), "tools", "listChanged")).toBe(true);
    expect(capabilityFlag(handshake(), "prompts", "listChanged")).toBe(false);
    expect(capabilityFlag(null, "tools", "listChanged")).toBe(false);
  });
});

describe("serverIdentity", () => {
  it("joins name and version, tolerating either missing", () => {
    expect(serverIdentity(handshake())).toBe("mock-mcp-server 0.1.0");
    expect(serverIdentity(handshake({ server_info: { name: "solo" } }))).toBe("solo");
    expect(serverIdentity(handshake({ server_info: { version: "2" } }))).toBe("2");
    expect(serverIdentity(handshake({ server_info: {} }))).toBeNull();
    expect(serverIdentity(handshake({ server_info: { name: 7 } }))).toBeNull();
  });
});

describe("describeHandshake", () => {
  it("builds the row's meta line", () => {
    expect(describeHandshake(handshake())).toBe(
      "mock-mcp-server 0.1.0 · MCP 2025-06-18 · tools, prompts, experimental",
    );
  });

  it("says so when nothing is advertised", () => {
    expect(describeHandshake(handshake({ capabilities: {}, server_info: {} }))).toBe(
      "MCP 2025-06-18 · no capabilities advertised",
    );
  });
});
