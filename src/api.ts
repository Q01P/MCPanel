import { invoke } from "@tauri-apps/api/core";
import type { GatewayInfo, NewServer, ServerOverview, ServerRecord } from "./types";

export const listServers = () => invoke<ServerOverview[]>("list_servers");
export const addServer = (server: NewServer) =>
  invoke<ServerRecord>("add_server", { new: server });
export const removeServer = (id: number) => invoke<void>("remove_server", { id });
export const startServer = (id: number) => invoke<void>("start_server", { id });
export const stopServer = (id: number) => invoke<void>("stop_server", { id });
export const gatewayInfo = () => invoke<GatewayInfo>("gateway_info");

/** Backend errors serialize as `{code, message}` (AppError). */
export function describeError(error: unknown): string {
  if (error && typeof error === "object" && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  return String(error);
}
