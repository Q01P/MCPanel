import { useEffect } from "react";
import { ExportDialog } from "./components/ExportDialog";
import { ImportDialog } from "./components/ImportDialog";
import { LogViewer } from "./components/LogViewer";
import { ServerForm } from "./components/ServerForm";
import { ServerList } from "./components/ServerList";
import { Workbench } from "./components/Workbench";
import { connectEvents } from "./events";
import { useLogs } from "./logs";
import { usePrompts } from "./prompts";
import { useResources } from "./resources";
import { usePanel } from "./store";
import { useTools } from "./tools";

export default function App() {
  const load = usePanel((s) => s.load);
  const applyEvent = usePanel((s) => s.applyEvent);
  const ingest = useLogs((s) => s.ingest);
  const applyToolsEvent = useTools((s) => s.applyEvent);
  const applyResourcesEvent = useResources((s) => s.applyEvent);
  const applyPromptsEvent = usePrompts((s) => s.applyEvent);
  const error = usePanel((s) => s.error);
  const setImportOpen = usePanel((s) => s.setImportOpen);
  const setExportOpen = usePanel((s) => s.setExportOpen);
  const haveServers = usePanel((s) => s.servers.length > 0);
  const clearError = usePanel((s) => s.clearError);

  useEffect(() => {
    void load();
    return connectEvents(
      (event) => {
        applyEvent(event);
        ingest(event);
        applyToolsEvent(event);
        applyResourcesEvent(event);
        applyPromptsEvent(event);
      },
      // Every `ready` (first connect and reconnects) resyncs the list:
      // statuses that changed while the stream was down never replay.
      () => void load(),
    );
  }, [load, applyEvent, ingest, applyToolsEvent, applyResourcesEvent, applyPromptsEvent]);

  return (
    <main className="panel">
      <header className="panel-header">
        <h1>MCPanel</h1>
        <span className="tagline">local MCP servers, under control</span>
        <button type="button" className="import-button" onClick={() => setImportOpen(true)}>
          Import…
        </button>
        <button
          type="button"
          className="import-button export-button"
          onClick={() => setExportOpen(true)}
          disabled={!haveServers}
          title={haveServers ? "write these servers as a client config" : "nothing to export yet"}
        >
          Export…
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
      <ExportDialog />
    </main>
  );
}
