import { gatewayInfo } from "./api";
import type { AppEvent } from "./types";

/** Parse an SSE frame defensively: a malformed or truncated frame (or a
 * backend schema drift) must not throw inside the event listener — the
 * exception would vanish without reconnecting or surfacing anywhere. */
export function parseAppEvent(data: string): AppEvent | null {
  try {
    const value: unknown = JSON.parse(data);
    if (
      value !== null &&
      typeof value === "object" &&
      typeof (value as { type?: unknown }).type === "string"
    ) {
      return value as AppEvent;
    }
  } catch {
    // fall through to the null below
  }
  return null;
}

/**
 * Subscribe to the gateway's SSE stream. The token travels as a query param
 * because EventSource cannot set headers. Reconnects with a fixed backoff;
 * returns a disposer.
 *
 * `onReady` fires on the gateway's `ready` event — first connect and every
 * reconnect — so callers can resync state that events emitted during a gap
 * would otherwise have carried.
 */
export function connectEvents(
  onEvent: (event: AppEvent) => void,
  onReady?: () => void,
): () => void {
  let source: EventSource | null = null;
  let retryTimer: number | undefined;
  let closed = false;

  const open = async () => {
    try {
      const { url, token } = await gatewayInfo();
      if (closed) return;
      source = new EventSource(`${url}/sse?token=${encodeURIComponent(token)}`);
      source.addEventListener("ready", () => onReady?.());
      source.addEventListener("app", (message) => {
        const event = parseAppEvent((message as MessageEvent<string>).data);
        if (event) onEvent(event);
      });
      source.onerror = () => {
        source?.close();
        scheduleRetry();
      };
    } catch {
      scheduleRetry();
    }
  };

  const scheduleRetry = () => {
    // One pending retry at a time: onerror can fire repeatedly on a dead
    // source, and an open() failure schedules too.
    if (closed || retryTimer !== undefined) return;
    retryTimer = window.setTimeout(() => {
      retryTimer = undefined;
      void open();
    }, 2000);
  };

  void open();
  return () => {
    closed = true;
    window.clearTimeout(retryTimer);
    source?.close();
  };
}
