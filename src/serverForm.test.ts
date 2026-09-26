import { describe, expect, it } from "vitest";
import { MAX_SERVER_TIMEOUT_S, parseTimeout } from "./components/ServerForm";

describe("parseTimeout", () => {
  it("maps blank to the default, whole seconds in range to a number, and the rest to invalid", () => {
    expect(parseTimeout("")).toBeNull();
    expect(parseTimeout("  ")).toBeNull();
    expect(parseTimeout("45")).toBe(45);
    expect(parseTimeout(String(MAX_SERVER_TIMEOUT_S))).toBe(MAX_SERVER_TIMEOUT_S);
    expect(parseTimeout("0")).toBeUndefined();
    expect(parseTimeout("1.5")).toBeUndefined();
    expect(parseTimeout("-3")).toBeUndefined();
    expect(parseTimeout(String(MAX_SERVER_TIMEOUT_S + 1))).toBeUndefined();
    expect(parseTimeout("30s")).toBeUndefined();
  });
});
