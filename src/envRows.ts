import type { EnvValue } from "./types";

/** One editable row of the server form's env table. */
export interface EnvRow {
  key: string;
  secret: boolean;
  /** For secret rows an empty value means "keep the stored credential". */
  value: string;
  /** A credential already exists in the OS store for this key. */
  hasStored: boolean;
}

export function rowsFromEnv(env: Record<string, EnvValue>): EnvRow[] {
  return Object.entries(env).map(([key, value]) =>
    value.kind === "secret"
      ? { key, secret: true, value: "", hasStored: true }
      : { key, secret: false, value: value.value, hasStored: false },
  );
}

/** Serialize rows back to the config shape. Secret values never appear here —
 * the env map carries only the marker; values go through set_server_secret. */
export function envFromRows(rows: EnvRow[]): Record<string, EnvValue> {
  const env: Record<string, EnvValue> = {};
  for (const row of rows) {
    const key = row.key.trim();
    if (!key) continue;
    env[key] = row.secret ? { kind: "secret" } : { kind: "plain", value: row.value };
  }
  return env;
}

/** Secret rows whose typed value must be written to the credential store —
 * blank secret rows keep whatever is already stored. */
export function secretsToStore(rows: EnvRow[]): { key: string; value: string }[] {
  return rows
    .filter((row) => row.secret && row.key.trim() !== "" && row.value !== "")
    .map((row) => ({ key: row.key.trim(), value: row.value }));
}

/** A secret row that has neither a stored credential nor a typed value can
 * never resolve at spawn — reject it at the form instead of at start time. */
export function missingSecretValue(rows: EnvRow[]): string | null {
  const missing = rows.find(
    (row) => row.secret && row.key.trim() !== "" && !row.hasStored && row.value === "",
  );
  return missing ? missing.key.trim() : null;
}
