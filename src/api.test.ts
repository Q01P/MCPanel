import { describe, expect, it } from "vitest";
import { describeError } from "./api";

describe("describeError", () => {
  it("extracts message from AppError-shaped objects", () => {
    expect(describeError({ code: "db", message: "disk I/O error" })).toBe(
      "disk I/O error",
    );
  });

  it("uses the message of Error instances", () => {
    expect(describeError(new Error("boom"))).toBe("boom");
  });

  it("stringifies primitives", () => {
    expect(describeError("plain text")).toBe("plain text");
  });
});
