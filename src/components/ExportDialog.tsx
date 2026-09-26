import { type FormEvent, useCallback, useEffect, useRef, useState } from "react";
import * as api from "../api";
import { usePanel } from "../store";
import type { ExportFlavor, ExportOutcome } from "../types";

const FLAVORS: { id: ExportFlavor; label: string }[] = [
  { id: "mcp_servers", label: "Claude Desktop / Claude Code / Cursor / Windsurf (mcpServers)" },
  { id: "vs_code", label: "VS Code (servers)" },
];

/** Export servers as another client's config.
 *
 * The inverse of the import dialog, with the same rule: credentials stay in
 * the OS keyring unless the user explicitly asks for them in the output.
 * Nothing here writes into a client's live file — the text is copied, or
 * saved to a *new* file the user names, and merged by hand. */
export function ExportDialog() {
  const open = usePanel((s) => s.exportOpen);
  const setOpen = usePanel((s) => s.setExportOpen);
  const servers = usePanel((s) => s.servers);

  const ref = useRef<HTMLDialogElement>(null);
  const textRef = useRef<HTMLTextAreaElement>(null);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [flavor, setFlavor] = useState<ExportFlavor>("mcp_servers");
  const [includeSecrets, setIncludeSecrets] = useState(false);
  const [outcome, setOutcome] = useState<ExportOutcome | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [savePath, setSavePath] = useState("");
  const [status, setStatus] = useState<string | null>(null);

  // Each opening starts from "everything selected, secrets withheld".
  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    if (open && !dialog.open) {
      setSelected(new Set(servers.map((s) => s.id)));
      setIncludeSecrets(false);
      setOutcome(null);
      setError(null);
      setStatus(null);
      dialog.showModal();
    } else if (!open && dialog.open) {
      dialog.close();
    }
  }, [open, servers]);

  const generate = useCallback(async () => {
    const ids = [...selected];
    if (ids.length === 0) {
      setOutcome(null);
      return;
    }
    try {
      setOutcome(await api.exportServers({ ids, flavor, include_secrets: includeSecrets }));
      setError(null);
    } catch (failure) {
      setOutcome(null);
      setError(api.describeError(failure));
    }
  }, [selected, flavor, includeSecrets]);

  useEffect(() => {
    if (open) void generate();
  }, [open, generate]);

  const toggle = (id: number) =>
    setSelected((current) => {
      const next = new Set(current);
      if (!next.delete(id)) next.add(id);
      return next;
    });

  const copy = async () => {
    if (!outcome) return;
    try {
      await navigator.clipboard.writeText(outcome.text);
      setStatus("copied to the clipboard");
    } catch {
      textRef.current?.select();
      setStatus("clipboard unavailable — the text is selected, press copy");
    }
  };

  const save = async (event: FormEvent) => {
    event.preventDefault();
    const path = savePath.trim();
    if (!outcome || path === "") return;
    try {
      await api.writeExportFile(path, outcome.text);
      setStatus(`saved to ${path}`);
      setError(null);
    } catch (failure) {
      setError(api.describeError(failure));
    }
  };

  const secretCount = servers
    .filter((s) => selected.has(s.id))
    .reduce(
      (total, s) => total + Object.values(s.env).filter((v) => v.kind === "secret").length,
      0,
    );

  return (
    <dialog ref={ref} className="import-dialog" onClose={() => setOpen(false)}>
      <header className="import-header">
        <h2>Export servers</h2>
        <button type="button" onClick={() => setOpen(false)} aria-label="close">
          ×
        </button>
      </header>

      <p className="import-intro">
        The selected servers in the shape other MCP clients read. Paste it into a client's
        config, or save it as a new file and merge it in — MCPanel never edits another app's
        config for you. Auto-start, timeout, and restart settings are MCPanel's own and are
        left out.
      </p>

      <ul className="export-servers">
        {servers.map((server) => (
          <li key={server.id}>
            <label>
              <input
                type="checkbox"
                checked={selected.has(server.id)}
                onChange={() => toggle(server.id)}
              />
              {server.name}
            </label>
          </li>
        ))}
      </ul>

      <div className="export-options">
        <label>
          format
          <select
            aria-label="config format"
            value={flavor}
            onChange={(e) => setFlavor(e.target.value as ExportFlavor)}
          >
            {FLAVORS.map((f) => (
              <option key={f.id} value={f.id}>
                {f.label}
              </option>
            ))}
          </select>
        </label>
        <label title="resolve credentials from the OS keyring into the text">
          <input
            type="checkbox"
            checked={includeSecrets}
            disabled={secretCount === 0}
            onChange={(e) => setIncludeSecrets(e.target.checked)}
          />
          include secret values{secretCount > 0 ? ` (${secretCount})` : ""}
        </label>
      </div>

      {includeSecrets && (
        <p className="export-warning" role="alert">
          The text below contains real credentials in plain text. Anything you paste or save
          it into holds them unprotected.
        </p>
      )}
      {outcome && outcome.placeholders.length > 0 && (
        <p className="import-note">
          {outcome.placeholders.join(", ")} written as <code>{"$"}{"{KEY}"}</code> placeholders
          — fill them in on the other side, or tick "include secret values".
        </p>
      )}

      <textarea
        ref={textRef}
        className="export-text"
        aria-label="exported config"
        readOnly
        spellCheck={false}
        value={outcome?.text ?? (selected.size === 0 ? "select at least one server" : "")}
      />

      {error && (
        <p className="tools-call-error" role="alert">
          {error}
        </p>
      )}
      {status && (
        <p className="export-status" role="status">
          {status}
        </p>
      )}

      <form className="import-manual" onSubmit={(event) => void save(event)}>
        <label htmlFor="export-path">Save as a new file (existing files are never overwritten)</label>
        <div className="import-manual-row">
          <input
            id="export-path"
            type="text"
            value={savePath}
            placeholder="/path/to/new-mcp.json"
            onChange={(event) => setSavePath(event.target.value)}
          />
          <button type="submit" disabled={!outcome || savePath.trim() === ""}>
            save
          </button>
        </div>
      </form>

      <div className="import-footer">
        <footer className="import-actions">
          <span className="import-count">
            {selected.size} of {servers.length} selected
          </span>
          <button type="button" onClick={() => setOpen(false)}>
            close
          </button>
          <button
            type="button"
            className="import-submit"
            disabled={!outcome}
            onClick={() => void copy()}
          >
            copy
          </button>
        </footer>
      </div>
    </dialog>
  );
}
