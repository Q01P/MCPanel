import { describe, expect, it } from "vitest";
import {
  candidateKey,
  commandLine,
  countCandidates,
  defaultSelection,
  mergeOutcomes,
  selectionByPath,
  summarize,
  toggle,
} from "./importing";
import type { DiscoveredConfig, ImportCandidate, ImportOutcome } from "./types";

function candidate(name: string, overrides: Partial<ImportCandidate> = {}): ImportCandidate {
  return {
    name,
    command: "npx",
    args: [],
    cwd: null,
    env: {},
    secret_keys: [],
    notes: [],
    conflicts: false,
    ...overrides,
  };
}

function config(path: string, servers: ImportCandidate[]): DiscoveredConfig {
  return { client: "Test", path, servers, skipped: [] };
}

function outcome(overrides: Partial<ImportOutcome> = {}): ImportOutcome {
  return { imported: [], failed: [], ...overrides };
}

const imported = (source_name: string, name = source_name, secrets_stored = 0) => ({
  id: 1,
  name,
  source_name,
  secrets_stored,
});

describe("defaultSelection", () => {
  it("checks everything that would not collide", () => {
    const configs = [config("/a.json", [candidate("files"), candidate("memory")])];
    expect(defaultSelection(configs)).toEqual(
      new Set([candidateKey("/a.json", "files"), candidateKey("/a.json", "memory")]),
    );
  });

  it("leaves a conflicting name unchecked so the rename is deliberate", () => {
    const configs = [
      config("/a.json", [candidate("files", { conflicts: true }), candidate("memory")]),
    ];
    expect(defaultSelection(configs)).toEqual(new Set([candidateKey("/a.json", "memory")]));
  });
});

describe("selection keys", () => {
  it("keeps same-named servers in different files apart", () => {
    const configs = [
      config("/a.json", [candidate("files")]),
      config("/b.json", [candidate("files")]),
    ];
    const selection = defaultSelection(configs);
    expect(selection.size).toBe(2);
    expect(selectionByPath(configs, selection)).toEqual([
      { path: "/a.json", names: ["files"] },
      { path: "/b.json", names: ["files"] },
    ]);
  });

  it("cannot be spoofed by a name that looks like a key", () => {
    // Two different (path, name) pairs must never collapse onto one key,
    // or checking one entry would silently import another.
    expect(candidateKey("/a.json", "b")).not.toBe(candidateKey('/a.json", "b', ""));
  });

  it("toggles a key off and back on", () => {
    const key = candidateKey("/a.json", "files");
    const once = toggle(new Set([key]), key);
    expect(once.has(key)).toBe(false);
    expect(toggle(once, key).has(key)).toBe(true);
  });

  it("drops files with nothing selected", () => {
    const configs = [
      config("/a.json", [candidate("files")]),
      config("/b.json", [candidate("memory")]),
    ];
    const selection = new Set([candidateKey("/b.json", "memory")]);
    expect(selectionByPath(configs, selection)).toEqual([{ path: "/b.json", names: ["memory"] }]);
  });

  it("counts candidates across files", () => {
    expect(
      countCandidates([
        config("/a.json", [candidate("one"), candidate("two")]),
        config("/b.json", [candidate("three")]),
      ]),
    ).toBe(3);
  });
});

describe("summarize", () => {
  it("reports an empty run without claiming success", () => {
    expect(summarize(outcome())).toBe("Nothing was imported.");
  });

  it("counts imports and failures", () => {
    const text = summarize(
      outcome({
        imported: [imported("files"), imported("memory")],
        failed: [{ name: "broken", reason: "no command" }],
      }),
    );
    expect(text).toContain("Imported 2 servers.");
    expect(text).toContain("1 server could not be imported.");
  });

  it("never hides a rename", () => {
    expect(summarize(outcome({ imported: [imported("files", "files (2)")] }))).toContain(
      '"files" as "files (2)"',
    );
  });

  it("says where credentials went", () => {
    expect(summarize(outcome({ imported: [imported("gh", "gh", 2)] }))).toContain(
      "Moved 2 credentials into the OS keyring.",
    );
  });

  it("stays silent about credentials when there were none", () => {
    expect(summarize(outcome({ imported: [imported("files")] }))).not.toContain("keyring");
  });
});

describe("mergeOutcomes", () => {
  it("concatenates per-file results", () => {
    const merged = mergeOutcomes([
      outcome({ imported: [imported("a")] }),
      outcome({ failed: [{ name: "b", reason: "no command" }] }),
    ]);
    expect(merged.imported).toHaveLength(1);
    expect(merged.failed).toHaveLength(1);
  });
});

describe("commandLine", () => {
  it("joins the command with its arguments", () => {
    expect(commandLine(candidate("files", { args: ["-y", "@mcp/fs"] }))).toBe("npx -y @mcp/fs");
  });
});
