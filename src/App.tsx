import { useEffect } from "react";
import { AddServerForm } from "./components/AddServerForm";
import { LogViewer } from "./components/LogViewer";
import { ServerList } from "./components/ServerList";
import { connectEvents } from "./events";
import { useLogs } from "./logs";
import { usePanel } from "./store";

export default function App() {
  const load = usePanel((s) => s.load);
  const applyEvent = usePanel((s) => s.applyEvent);
  const ingest = useLogs((s) => s.ingest);
  const error = usePanel((s) => s.error);
  const clearError = usePanel((s) => s.clearError);

  useEffect(() => {
    void load();
    return connectEvents((event) => {
      applyEvent(event);
      ingest(event);
    });
  }, [load, applyEvent, ingest]);

  return (
    <main className="panel">
      <header className="panel-header">
        <h1>MCPanel</h1>
        <span className="tagline">local MCP servers, under control</span>
      </header>

      {error && (
        <div className="error-banner" role="alert">
          <span>{error}</span>
          <button onClick={clearError} aria-label="dismiss">
            ×
          </button>
        </div>
      )}

      <ServerList />
      <LogViewer />
      <AddServerForm />
    </main>
  );
}
