// Mirrors the backend's serde shapes exactly (state.rs, db/mod.rs).

export type EnvValue = { kind: "plain"; value: string } | { kind: "secret" };

export type ServerStatus =
  | { state: "stopped" | "starting" | "initializing" | "running" }
  | { state: "errored"; message: string };

export interface ServerRecord {
  id: number;
  name: string;
  command: string;
  args: string[];
  env: Record<string, EnvValue>;
  cwd: string | null;
  auto_start: boolean;
}

export interface ServerOverview extends ServerRecord {
  status: ServerStatus;
}

export type NewServer = Omit<ServerRecord, "id">;

export type LogStreamName = "stdout" | "stderr";

export type AppEvent =
  | { type: "status_changed"; server_id: number; status: ServerStatus }
  | { type: "log"; server_id: number; stream: LogStreamName; line: string }
  | { type: "log_gap"; server_id: number; stream: LogStreamName; dropped: number }
  | { type: "notification"; server_id: number; payload: unknown }
  // Notifications lost to backpressure on the backend's advisory channel.
  | { type: "notification_gap"; server_id: number; dropped: number }
  // Synthetic gateway marker: this SSE subscriber fell behind the broadcast.
  | { type: "lagged"; missed: number };

export interface GatewayInfo {
  url: string;
  token: string;
}

// Import from other MCP clients' config files (backend: import.rs).

export interface ImportCandidate {
  name: string;
  command: string;
  args: string[];
  cwd: string | null;
  /** Plain environment variables. Never contains credential values. */
  env: Record<string, string>;
  /** Credential-looking keys, names only — values stay backend-side and go
   * straight to the OS keyring at import. */
  secret_keys: string[];
  notes: string[];
  /** A server of this name exists already; importing will suffix it. */
  conflicts: boolean;
}

export interface SkippedEntry {
  name: string;
  reason: string;
}

export interface DiscoveredConfig {
  client: string;
  path: string;
  servers: ImportCandidate[];
  skipped: SkippedEntry[];
}

export interface ImportedServer {
  id: number;
  /** The name actually created — may differ from `source_name`. */
  name: string;
  source_name: string;
  secrets_stored: number;
}

export interface FailedImport {
  name: string;
  reason: string;
}

export interface ImportOutcome {
  imported: ImportedServer[];
  failed: FailedImport[];
}
