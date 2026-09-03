import { describeError, gatewayInfo } from "./api";
import type { GatewayInfo } from "./types";

// The gateway address and token are fixed for the app's lifetime — cache
// the promise so one invoke serves every send (and concurrent callers);
// drop it on failure so the next send retries.
let gateway: Promise<GatewayInfo> | undefined;
export function cachedGatewayInfo(): Promise<GatewayInfo> {
  gateway ??= gatewayInfo().catch((error: unknown) => {
    gateway = undefined;
    throw error;
  });
  return gateway;
}

/** Tests only: forget a cached gateway so a stubbed one can take over. */
export function resetGatewayCache(): void {
  gateway = undefined;
}

export interface RawReply {
  ok: boolean;
  status: number;
  text: string;
}

/** POST one JSON-RPC payload at a running server through the gateway and
 * hand back the body verbatim. The JSON workbench shows this as-is; typed
 * callers go through [`rpc`]. */
export async function postRaw(
  serverId: number,
  payload: unknown,
  timeoutS: number,
): Promise<RawReply> {
  const { url, token } = await cachedGatewayInfo();
  const res = await fetch(`${url}/mcp/${serverId}?timeout_s=${timeoutS}`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${token}`,
    },
    body: JSON.stringify(payload),
    // Client-side bound just above the server's own timeout: a dead
    // gateway or stalled connection must not pin `pending` forever.
    signal: AbortSignal.timeout((timeoutS + 15) * 1000),
  });
  return { ok: res.ok, status: res.status, text: await res.text() };
}

/** What a typed request came back as. The three cases mean different
 * things to a caller: a result to render, an error the *server* raised
 * (the tool exists but refused), or a failure to reach the server at all. */
export type RpcOutcome =
  | { kind: "result"; result: unknown }
  | { kind: "error"; code: number; message: string }
  | { kind: "transport"; message: string };

/** The gateway echoes whatever id we send; a fixed one is fine. */
const REQUEST_ID = 1;

export function envelope(method: string, params: unknown): Record<string, unknown> {
  return { jsonrpc: "2.0", id: REQUEST_ID, method, params };
}

export async function rpc(
  serverId: number,
  method: string,
  params: unknown,
  timeoutS: number,
): Promise<RpcOutcome> {
  let reply: RawReply;
  try {
    reply = await postRaw(serverId, envelope(method, params), timeoutS);
  } catch (error) {
    return { kind: "transport", message: describeError(error) };
  }

  let body: unknown;
  try {
    body = JSON.parse(reply.text);
  } catch {
    return { kind: "transport", message: `HTTP ${reply.status}: ${reply.text}` };
  }
  if (!reply.ok) {
    // AppError shape from the gateway: {code, message}.
    return { kind: "transport", message: `HTTP ${reply.status}: ${describeError(body)}` };
  }
  if (body && typeof body === "object" && "error" in body) {
    const error = (body as { error: { code?: unknown; message?: unknown } }).error;
    return {
      kind: "error",
      code: typeof error?.code === "number" ? error.code : 0,
      message: String(error?.message ?? "unknown error"),
    };
  }
  return { kind: "result", result: (body as { result?: unknown })?.result };
}
