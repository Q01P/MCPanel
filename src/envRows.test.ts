import { describe, expect, it } from "vitest";
import { envFromRows, missingSecretValue, rowsFromEnv, secretsToStore } from "./envRows";

describe("rowsFromEnv", () => {
  it("maps plain values and secret markers to editable rows", () => {
    const rows = rowsFromEnv({
      PLAIN: { kind: "plain", value: "visible" },
      TOKEN: { kind: "secret" },
    });
    expect(rows).toEqual([
      { key: "PLAIN", secret: false, value: "visible", hasStored: false },
      { key: "TOKEN", secret: true, value: "", hasStored: true },
    ]);
  });
});

describe("envFromRows", () => {
  it("serializes rows back to config shape, secrets as markers only", () => {
    const env = envFromRows([
      { key: "PLAIN", secret: false, value: "visible", hasStored: false },
      { key: "TOKEN", secret: true, value: "hunter2", hasStored: false },
    ]);
    expect(env).toEqual({
      PLAIN: { kind: "plain", value: "visible" },
      TOKEN: { kind: "secret" },
    });
    // The typed secret value must never appear in the config payload.
    expect(JSON.stringify(env)).not.toContain("hunter2");
  });

  it("skips rows with blank keys and trims the rest", () => {
    const env = envFromRows([
      { key: "  ", secret: false, value: "ignored", hasStored: false },
      { key: " PAD ", secret: false, value: "v", hasStored: false },
    ]);
    expect(env).toEqual({ PAD: { kind: "plain", value: "v" } });
  });
});

describe("secretsToStore", () => {
  it("collects only secret rows with a typed value", () => {
    const stored = secretsToStore([
      { key: "KEEP", secret: true, value: "", hasStored: true },
      { key: "NEW", secret: true, value: "v1", hasStored: false },
      { key: "ROTATED", secret: true, value: "v2", hasStored: true },
      { key: "PLAIN", secret: false, value: "v3", hasStored: false },
    ]);
    expect(stored).toEqual([
      { key: "NEW", value: "v1" },
      { key: "ROTATED", value: "v2" },
    ]);
  });
});

describe("missingSecretValue", () => {
  it("flags a new secret with no value, ignores stored and blank-key rows", () => {
    expect(
      missingSecretValue([{ key: "NEW", secret: true, value: "", hasStored: false }]),
    ).toBe("NEW");
    expect(
      missingSecretValue([
        { key: "KEEP", secret: true, value: "", hasStored: true },
        { key: "", secret: true, value: "", hasStored: false },
      ]),
    ).toBeNull();
  });
});
