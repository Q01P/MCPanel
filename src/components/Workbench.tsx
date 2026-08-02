import { useEffect } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { json } from "@codemirror/lang-json";
import { usePanel } from "../store";
import { MAX_TIMEOUT_S, TEMPLATES, useWorkbench } from "../workbench";

const EXTENSIONS = [json()];

/**
 * The "Postman for MCP" part: hand-craft JSON-RPC, fire it at a running
 * server through the gateway, inspect the response. Requests are re-played
 * from history with a click.
 */
export function Workbench() {
  const servers = usePanel((s) => s.servers);
  const running = servers.filter((s) => s.status.state === "running");

  const serverId = useWorkbench((s) => s.serverId);
  const body = useWorkbench((s) => s.body);
  const response = useWorkbench((s) => s.response);
  const pending = useWorkbench((s) => s.pending);
  const history = useWorkbench((s) => s.history);
  const timeoutS = useWorkbench((s) => s.timeoutS);
  const setServer = useWorkbench((s) => s.setServer);
  const setBody = useWorkbench((s) => s.setBody);
  const setTimeoutS = useWorkbench((s) => s.setTimeoutS);
  const restore = useWorkbench((s) => s.restore);
  const send = useWorkbench((s) => s.send);

  // Selection follows reality: a stopped server can't receive requests.
  const target = running.find((s) => s.id === serverId) ?? running[0] ?? null;
  const targetId = target?.id ?? null;
  useEffect(() => {
    if (targetId !== serverId) setServer(targetId);
  }, [targetId, serverId, setServer]);

  return (
    <section className="workbench">
      <h2>JSON workbench</h2>

      <div className="workbench-toolbar">
        <select
          aria-label="target server"
          value={targetId ?? ""}
          onChange={(e) => setServer(e.target.value ? Number(e.target.value) : null)}
          disabled={running.length === 0}
        >
          {running.length === 0 ? (
            <option value="">no running servers</option>
          ) : (
            running.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))
          )}
        </select>

        <select
          aria-label="request template"
          value=""
          onChange={(e) => {
            const template = TEMPLATES.find((t) => t.label === e.target.value);
            if (template) setBody(template.body);
          }}
        >
          <option value="">templates…</option>
          {TEMPLATES.map((t) => (
            <option key={t.label} value={t.label}>
              {t.label}
            </option>
          ))}
        </select>

        <label className="timeout-field" title="per-request timeout (seconds)">
          timeout
          <input
            type="number"
            min={1}
            max={MAX_TIMEOUT_S}
            value={timeoutS}
            onChange={(e) => setTimeoutS(Number(e.target.value))}
          />
          s
        </label>

        <button
          type="button"
          className="send-button"
          disabled={target == null || pending}
          onClick={() => target && void send(target.name)}
        >
          {pending ? "sending…" : "send"}
        </button>
      </div>

      <div className="workbench-panes">
        <div className="workbench-editor">
          <CodeMirror
            value={body}
            height="220px"
            theme="dark"
            extensions={EXTENSIONS}
            onChange={setBody}
            basicSetup={{ foldGutter: false }}
          />
        </div>
        <pre className="workbench-response">
          {response ?? "response will appear here"}
        </pre>
      </div>

      {history.length > 0 && (
        <div className="workbench-history">
          <h3>history</h3>
          <ul>
            {history.map((entry) => (
              <li key={entry.seq}>
                <button type="button" onClick={() => restore(entry)} title={entry.body}>
                  <span className="history-time">{entry.at}</span>
                  <span className="history-server">{entry.serverName}</span>
                  <span className="history-preview">
                    {entry.body.replace(/\s+/g, " ").slice(0, 60)}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}
