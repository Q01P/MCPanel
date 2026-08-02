import { describe, expect, it } from "vitest";
import { parseAppEvent } from "./events";

describe("parseAppEvent", () => {
  it("parses a well-formed frame", () => {
    expect(parseAppEvent('{"type":"lagged","missed":3}')).toEqual({
      type: "lagged",
      missed: 3,
    });
  });

  it("returns null for truncated JSON instead of throwing", () => {
    expect(parseAppEvent('{"type":"log","server_id":')).toBeNull();
  });

  it("returns null when the discriminant is missing or wrong", () => {
    expect(parseAppEvent('{"missed":3}')).toBeNull();
    expect(parseAppEvent('{"type":42}')).toBeNull();
    expect(parseAppEvent("null")).toBeNull();
    expect(parseAppEvent('"just a string"')).toBeNull();
  });
});
