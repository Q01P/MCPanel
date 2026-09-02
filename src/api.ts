import { invoke } from "@tauri-apps/api/core";
import type {
  DiscoveredConfig,
  GatewayInfo,
  ImportOutcome,
  NewServer,
  ServerOverview,
  ServerRecord,
} from "./types";

export const listServers = () => invoke<ServerOverview[]>("list_servers");
export const addServer = (server: NewServer) =>
  invoke<ServerRecord>("add_server", { new: server });
export const updateServer = (record: ServerRecord) =>
  invoke<void>("update_server", { record });
export const setServerSecret = (id: number, key: string, value: string) =>
  invoke<void>("set_server_secret", { id, key, value });
export const deleteServerSecret = (id: number, key: string) =>
  invoke<void>("delete_server_secret", { id, key });
export const removeServer = (id: number) => invoke<void>("remove_server", { id });
export const startServer = (id: number) => invoke<void>("start_server", { id });
export const stopServer = (id: number) => invoke<void>("stop_server", { id });
export const gatewayInfo = () => invoke<GatewayInfo>("gateway_info");

export const discoverImports = () => invoke<DiscoveredConfig[]>("discover_imports");
export const readImportConfig = (path: string) =>
  invoke<DiscoveredConfig>("read_import_config", { path });
export const importServers = (path: string, names: string[]) =>
  invoke<ImportOutcome>("import_servers", { path, names });

/** Backend errors serialize as `{code, message}` (AppError). */
export function describeError(error: unknown): string {
  if (error && typeof error === "object" && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  return String(error);
}
