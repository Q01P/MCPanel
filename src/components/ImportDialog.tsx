import { type FormEvent, useCallback, useEffect, useRef, useState } from "react";
import {
  type Selection,
  candidateKey,
  commandLine,
  countCandidates,
  defaultSelection,
  mergeOutcomes,
  selectionByPath,
  summarize,
  toggle,
} from "../importing";
import { usePanel } from "../store";
import type { DiscoveredConfig, ImportCandidate, ImportOutcome } from "../types";

/** One importable entry: a checkbox plus everything that would surprise the
 * user if they only found out afterwards — a rename, where credentials go,
 * a placeholder they still have to fill in. */
function CandidateRow({
  config,
  candidate,
  selection,
  onToggle,
}: {
  config: DiscoveredConfig;
  candidate: ImportCandidate;
  selection: Selection;
  onToggle: (key: string) => void;
}) {
  const key = candidateKey(config.path, candidate.name);
  const envKeys = Object.keys(candidate.env);
  return (
    <li className="import-candidate">
      <label className="import-candidate-head">
        <input
          type="checkbox"
          checked={selection.has(key)}
          onChange={() => onToggle(key)}
        />
        <span className="import-candidate-name">{candidate.name}</span>
      </label>
      <span className="import-candidate-command">{commandLine(candidate)}</span>
      <div className="import-candidate-meta">
        {candidate.conflicts && (
          <span className="import-tag import-tag-warn">
            name already in use — will be imported as a copy
          </span>
        )}
        {candidate.secret_keys.length > 0 && (
          <span className="import-tag import-tag-secret">
            {candidate.secret_keys.join(", ")} → OS keyring
          </span>
        )}
        {envKeys.length > 0 && <span className="import-tag">env: {envKeys.join(", ")}</span>}
        {candidate.cwd && <span className="import-tag">cwd: {candidate.cwd}</span>}
      </div>
      {candidate.notes.map((note) => (
        <p className="import-note" key={note}>
          {note}
        </p>
      ))}
    </li>
  );
}

function ConfigSection({
  config,
  selection,
  onToggle,
}: {
  config: DiscoveredConfig;
  selection: Selection;
  onToggle: (key: string) => void;
}) {
  return (
    <section className="import-source">
      <h3>
        {config.client}
        <span className="import-source-path">{config.path}</span>
      </h3>
      {config.servers.length > 0 && (
        <ul className="import-candidates">
          {config.servers.map((candidate) => (
            <CandidateRow
              key={candidate.name}
              config={config}
              candidate={candidate}
              selection={selection}
              onToggle={onToggle}
            />
          ))}
        </ul>
      )}
      {config.skipped.length > 0 && (
        <ul className="import-skipped">
          {config.skipped.map((entry) => (
            <li key={entry.name}>
              <span className="import-skipped-name">{entry.name}</span>
              <span className="import-skipped-reason">{entry.reason}</span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** Import servers from other MCP clients' config files.
 *
 * Native `<dialog>` for the focus trap and Escape handling rather than a
 * hand-rolled modal. Selection is by (file, name): the backend re-reads the
 * file and does the actual work, so nothing here ever holds a credential. */
export function ImportDialog() {
  const open = usePanel((s) => s.importOpen);
  const setOpen = usePanel((s) => s.setImportOpen);
  const discoverImports = usePanel((s) => s.discoverImports);
  const readImportFile = usePanel((s) => s.readImportFile);
  const runImport = usePanel((s) => s.runImport);

  const ref = useRef<HTMLDialogElement>(null);
  const [configs, setConfigs] = useState<DiscoveredConfig[]>([]);
  const [selection, setSelection] = useState<Selection>(new Set<string>());
  const [scanning, setScanning] = useState(false);
  const [importing, setImporting] = useState(false);
  const [outcome, setOutcome] = useState<ImportOutcome | null>(null);
  const [manualPath, setManualPath] = useState("");

  const scan = useCallback(async () => {
    setScanning(true);
    const found = await discoverImports();
    if (found) {
      setConfigs(found);
      setSelection(defaultSelection(found));
    }
    setScanning(false);
  }, [discoverImports]);

  // Each opening starts from a fresh scan: configs change outside the app,
  // and a stale preview would import the wrong thing.
  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    if (open && !dialog.open) {
      dialog.showModal();
      setOutcome(null);
      void scan();
    } else if (!open && dialog.open) {
      dialog.close();
    }
  }, [open, scan]);

  const addManualPath = async (event: FormEvent) => {
    event.preventDefault();
    const path = manualPath.trim();
    if (path === "") return;
    const config = await readImportFile(path);
    if (!config) return;
    setManualPath("");
    // Replace an existing entry for the same file so re-adding refreshes it.
    const next = [...configs.filter((existing) => existing.path !== config.path), config];
    setConfigs(next);
    setSelection((current) => {
      const merged = new Set(current);
      for (const candidate of config.servers) {
        if (!candidate.conflicts) merged.add(candidateKey(config.path, candidate.name));
      }
      return merged;
    });
  };

  const submit = async () => {
    const groups = selectionByPath(configs, selection);
    if (groups.length === 0 || importing) return;
    setImporting(true);
    setOutcome(null);
    const outcomes: ImportOutcome[] = [];
    for (const group of groups) {
      const result = await runImport(group.path, group.names);
      // A failed call already surfaced in the error banner; keep whatever
      // the other files managed rather than discarding the whole run.
      if (result) outcomes.push(result);
    }
    const merged = mergeOutcomes(outcomes);
    setOutcome(merged);
    setImporting(false);
    // Imported names now conflict; re-scan so the preview tells the truth.
    if (merged.imported.length > 0) await scan();
  };

  const selectedCount = selectionByPath(configs, selection).reduce(
    (total, group) => total + group.names.length,
    0,
  );
  const total = countCandidates(configs);

  return (
    <dialog ref={ref} className="import-dialog" onClose={() => setOpen(false)}>
      <header className="import-header">
        <h2>Import servers</h2>
        <button type="button" onClick={() => setOpen(false)} aria-label="close">
          ×
        </button>
      </header>

      <p className="import-intro">
        Servers already configured in your other MCP clients. Credentials go straight from
        their config file into the OS keyring — they are never copied into MCPanel's own
        config.
      </p>

      {scanning && <p className="empty">scanning…</p>}

      {!scanning && configs.length === 0 && (
        <p className="empty">
          No client configs found. Add one by path below if yours lives somewhere else.
        </p>
      )}

      {configs.map((config) => (
        <ConfigSection
          key={config.path}
          config={config}
          selection={selection}
          onToggle={(key) => setSelection((current) => toggle(current, key))}
        />
      ))}

      <form className="import-manual" onSubmit={(event) => void addManualPath(event)}>
        <label htmlFor="import-path">Add a config file by path</label>
        <div className="import-manual-row">
          <input
            id="import-path"
            type="text"
            value={manualPath}
            placeholder="/path/to/mcp.json"
            onChange={(event) => setManualPath(event.target.value)}
          />
          <button type="submit" disabled={manualPath.trim() === ""}>
            read
          </button>
        </div>
      </form>

      <div className="import-footer">
        {outcome && (
          <div className="import-outcome" role="status">
            <p>{summarize(outcome)}</p>
            {outcome.failed.length > 0 && (
              <ul className="import-failures">
                {outcome.failed.map((failure) => (
                  <li key={failure.name}>
                    <span className="import-skipped-name">{failure.name}</span>
                    <span className="import-skipped-reason">{failure.reason}</span>
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}

        <footer className="import-actions">
          <span className="import-count">
            {total === 0 ? "nothing to import" : `${selectedCount} of ${total} selected`}
          </span>
          <button type="button" onClick={() => setOpen(false)}>
            close
          </button>
          <button
            type="button"
            className="import-submit"
            disabled={selectedCount === 0 || importing}
            onClick={() => void submit()}
          >
            {importing ? "importing…" : "import selected"}
          </button>
        </footer>
      </div>
    </dialog>
  );
}
