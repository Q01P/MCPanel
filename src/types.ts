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
  | { type: "notification"; server_id: number; payload: unknown };

export interface GatewayInfo {
  url: string;
  token: string;
}
