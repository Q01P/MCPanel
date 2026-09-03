import type { DiscoveredConfig, ImportCandidate, ImportOutcome } from "./types";

/** Selection identity. A name is only unique *within* one config file, so
 * the key pairs it with the path. JSON-encoding the pair keeps that mapping
 * injective — no separator a path or a server name could itself contain. */
export const candidateKey = (path: string, name: string) => JSON.stringify([path, name]);

export type Selection = ReadonlySet<string>;

/** Everything that wouldn't collide starts checked: the common case is
 * "import all of this", while a conflicting name would be silently renamed,
 * so that one asks for a deliberate click. */
export function defaultSelection(configs: DiscoveredConfig[]): Set<string> {
  const selected = new Set<string>();
  for (const config of configs) {
    for (const candidate of config.servers) {
      if (!candidate.conflicts) selected.add(candidateKey(config.path, candidate.name));
    }
  }
  return selected;
}

export function toggle(selection: Selection, key: string): Set<string> {
  const next = new Set(selection);
  if (!next.delete(key)) next.add(key);
  return next;
}

/** The selection regrouped into one backend call per file. Files with
 * nothing selected are dropped rather than sent as empty imports. */
export function selectionByPath(
  configs: DiscoveredConfig[],
  selection: Selection,
): { path: string; names: string[] }[] {
  return configs
    .map((config) => ({
      path: config.path,
      names: config.servers
        .map((candidate) => candidate.name)
        .filter((name) => selection.has(candidateKey(config.path, name))),
    }))
    .filter((group) => group.names.length > 0);
}

export function countCandidates(configs: DiscoveredConfig[]): number {
  return configs.reduce((total, config) => total + config.servers.length, 0);
}

/** Merge per-file outcomes into one, preserving order. */
export function mergeOutcomes(outcomes: ImportOutcome[]): ImportOutcome {
  return {
    imported: outcomes.flatMap((outcome) => outcome.imported),
    failed: outcomes.flatMap((outcome) => outcome.failed),
  };
}

const plural = (n: number, noun: string) => `${n} ${noun}${n === 1 ? "" : "s"}`;

/** One honest sentence about what happened — including renames, which the
 * user never asked for and must not discover by accident. */
export function summarize(outcome: ImportOutcome): string {
  const { imported, failed } = outcome;
  if (imported.length === 0 && failed.length === 0) return "Nothing was imported.";

  const parts: string[] = [];
  if (imported.length > 0) parts.push(`Imported ${plural(imported.length, "server")}.`);
  if (failed.length > 0) parts.push(`${plural(failed.length, "server")} could not be imported.`);

  const renamed = imported.filter((server) => server.name !== server.source_name);
  if (renamed.length > 0) {
    const list = renamed.map((s) => `"${s.source_name}" as "${s.name}"`).join(", ");
    parts.push(`Renamed to avoid a clash: ${list}.`);
  }
  const secrets = imported.reduce((total, server) => total + server.secrets_stored, 0);
  if (secrets > 0) {
    parts.push(`Moved ${plural(secrets, "credential")} into the OS keyring.`);
  }
  return parts.join(" ");
}

/** The command as it will run, for the preview line. */
export function commandLine(candidate: ImportCandidate): string {
  return [candidate.command, ...candidate.args].join(" ");
}
