import { useEffect } from "react";
import { ImportDialog } from "./components/ImportDialog";
import { LogViewer } from "./components/LogViewer";
import { ServerForm } from "./components/ServerForm";
import { ServerList } from "./components/ServerList";
import { Workbench } from "./components/Workbench";
import { connectEvents } from "./events";
import { useLogs } from "./logs";
import { usePanel } from "./store";

export default function App() {
  const load = usePanel((s) => s.load);
  const applyEvent = usePanel((s) => s.applyEvent);
  const ingest = useLogs((s) => s.ingest);
  const error = usePanel((s) => s.error);
  const setImportOpen = usePanel((s) => s.setImportOpen);
  const clearError = usePanel((s) => s.clearError);

  useEffect(() => {
    void load();
    return connectEvents(
      (event) => {
        applyEvent(event);
        ingest(event);
      },
      // Every `ready` (first connect and reconnects) resyncs the list:
      // statuses that changed while the stream was down never replay.
      () => void load(),
    );
  }, [load, applyEvent, ingest]);

  return (
    <main className="panel">
      <header className="panel-header">
        <h1>MCPanel</h1>
        <span className="tagline">local MCP servers, under control</span>
        <button type="button" className="import-button" onClick={() => setImportOpen(true)}>
          Import…
        </button>
      </header>

      {error && (
        <div className="error-banner" role="alert">
          <span>{error}</span>
          <button type="button" onClick={clearError} aria-label="dismiss">
            ×
          </button>
        </div>
      )}

      <ServerList />
      <LogViewer />
      <Workbench />
      <ServerForm />
      <ImportDialog />
    </main>
  );
}
