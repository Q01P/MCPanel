import { gatewayInfo } from "./api";
import type { AppEvent } from "./types";

/**
 * Subscribe to the gateway's SSE stream. The token travels as a query param
 * because EventSource cannot set headers. Reconnects with a fixed backoff;
 * returns a disposer.
 */
export function connectEvents(onEvent: (event: AppEvent) => void): () => void {
  let source: EventSource | null = null;
  let retryTimer: number | undefined;
  let closed = false;

  const open = async () => {
    try {
      const { url, token } = await gatewayInfo();
      if (closed) return;
      source = new EventSource(`${url}/sse?token=${token}`);
      source.addEventListener("app", (message) => {
        onEvent(JSON.parse((message as MessageEvent<string>).data) as AppEvent);
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
    if (!closed) retryTimer = window.setTimeout(open, 2000);
  };

  void open();
  return () => {
    closed = true;
    window.clearTimeout(retryTimer);
    source?.close();
  };
}
