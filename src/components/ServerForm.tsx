import { type FormEvent, useEffect, useRef, useState } from "react";
import {
  type EnvRow,
  envFromRows,
  missingSecretValue,
  rowsFromEnv,
  secretsToStore,
} from "../envRows";
import { usePanel } from "../store";

/** Naive whitespace split — quoting support can come with the workbench. */
function parseArgs(raw: string): string[] {
  return raw.trim().length === 0 ? [] : raw.trim().split(/\s+/);
}

/** Mirrors the backend ceiling on a configured per-server timeout. */
export const MAX_SERVER_TIMEOUT_S = 3600;

/** Blank means "use the default" (null); otherwise a whole number of
 * seconds within the backend's range, or undefined when it isn't one. */
export function parseTimeout(raw: string): number | null | undefined {
  const text = raw.trim();
  if (text === "") return null;
  if (!/^\d+$/.test(text)) return undefined;
  const seconds = Number(text);
  return seconds >= 1 && seconds <= MAX_SERVER_TIMEOUT_S ? seconds : undefined;
}

/** Stable identity for list rendering; EnvRow itself has no id. */
type FormRow = EnvRow & { rowId: number };

/** Add/edit form. Edit mode is entered from a row's edit button
 * (`usePanel.editing`); secrets are typed here but never enter the config
 * payload — they travel through set_server_secret into the OS store. */
export function ServerForm() {
  const add = usePanel((s) => s.add);
  const update = usePanel((s) => s.update);
  const setSecret = usePanel((s) => s.setSecret);
  const editing = usePanel((s) => s.editing);
  const setEditing = usePanel((s) => s.setEditing);

  const [name, setName] = useState("");
  const [command, setCommand] = useState("");
  const [args, setArgs] = useState("");
  const [cwd, setCwd] = useState("");
  const [autoStart, setAutoStart] = useState(false);
  const [timeoutS, setTimeoutS] = useState("");
  const [restartOnCrash, setRestartOnCrash] = useState(false);
  const [rows, setRows] = useState<FormRow[]>([]);
  const [formError, setFormError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const nextRowId = useRef(0);

  useEffect(() => {
    setFormError(null);
    setName(editing?.name ?? "");
    setCommand(editing?.command ?? "");
    setArgs(editing?.args.join(" ") ?? "");
    setCwd(editing?.cwd ?? "");
    setAutoStart(editing?.auto_start ?? false);
    setTimeoutS(editing?.request_timeout_s == null ? "" : String(editing.request_timeout_s));
    setRestartOnCrash(editing?.restart_on_crash ?? false);
    setRows(
      rowsFromEnv(editing?.env ?? {}).map((row) => ({
        ...row,
        rowId: nextRowId.current++,
      })),
    );
  }, [editing]);

  const patchRow = (rowId: number, patch: Partial<EnvRow>) =>
    setRows((rows) => rows.map((row) => (row.rowId === rowId ? { ...row, ...patch } : row)));

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (submitting || !name.trim() || !command.trim()) return;
    const missing = missingSecretValue(rows);
    if (missing) {
      setFormError(`secret "${missing}" needs a value`);
      return;
    }
    const timeout = parseTimeout(timeoutS);
    if (timeout === undefined) {
      setFormError(`timeout must be a whole number of seconds from 1 to ${MAX_SERVER_TIMEOUT_S}`);
      return;
    }
    setFormError(null);
    setSubmitting(true);
    try {
      const base = {
        name: name.trim(),
        command: command.trim(),
        args: parseArgs(args),
        env: envFromRows(rows),
        cwd: cwd.trim() === "" ? null : cwd.trim(),
        auto_start: autoStart,
        request_timeout_s: timeout,
        restart_on_crash: restartOnCrash,
      };
      const id = editing
        ? (await update({ ...base, id: editing.id })) ? editing.id : null
        : ((await add(base))?.id ?? null);
      // On failure the store surfaced the error banner; keep the input.
      if (id == null) return;
      for (const secret of secretsToStore(rows)) {
        if (!(await setSecret(id, secret.key, secret.value))) return;
      }
      if (editing) {
        setEditing(null); // the effect above resets the fields
      } else {
        setName("");
        setCommand("");
        setArgs("");
        setCwd("");
        setAutoStart(false);
        setTimeoutS("");
        setRestartOnCrash(false);
        setRows([]);
      }
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <form className="add-form" onSubmit={(e) => void submit(e)}>
      <h2>{editing ? `Edit "${editing.name}"` : "Add server"}</h2>
      <div className="add-fields">
        <input
          aria-label="server name"
          placeholder="name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          required
        />
        <input
          aria-label="command"
          placeholder="command (e.g. npx)"
          value={command}
          onChange={(e) => setCommand(e.target.value)}
          required
        />
        <input
          aria-label="arguments"
          placeholder="args (e.g. -y some-mcp-server)"
          value={args}
          onChange={(e) => setArgs(e.target.value)}
        />
        <input
          aria-label="working directory"
          placeholder="working dir (optional)"
          value={cwd}
          onChange={(e) => setCwd(e.target.value)}
        />
        <label className="auto-start-field" title="start this server when MCPanel launches">
          <input
            type="checkbox"
            checked={autoStart}
            onChange={(e) => setAutoStart(e.target.checked)}
          />
          auto-start
        </label>
      </div>

      <div className="add-options">
        <label
          className="timeout-field"
          title="bounds the handshake and any request without its own timeout; blank = 30 s"
        >
          timeout
          <input
            type="number"
            min={1}
            max={MAX_SERVER_TIMEOUT_S}
            placeholder="30"
            value={timeoutS}
            onChange={(e) => setTimeoutS(e.target.value)}
          />
          s
        </label>
        <label
          className="auto-start-field"
          title="respawn with backoff (1 s, 2 s, 4 s… up to 5 tries) if the process exits unexpectedly"
        >
          <input
            type="checkbox"
            checked={restartOnCrash}
            onChange={(e) => setRestartOnCrash(e.target.checked)}
          />
          restart on crash
        </label>
      </div>

      <div className="env-editor">
        {rows.map((row) => (
          <div className="env-row" key={row.rowId}>
            <input
              aria-label="environment variable name"
              placeholder="KEY"
              value={row.key}
              onChange={(e) => patchRow(row.rowId, { key: e.target.value })}
            />
            <input
              aria-label="environment variable value"
              type={row.secret ? "password" : "text"}
              placeholder={
                row.secret
                  ? row.hasStored
                    ? "stored — leave blank to keep"
                    : "secret value"
                  : "value"
              }
              value={row.value}
              onChange={(e) => patchRow(row.rowId, { value: e.target.value })}
            />
            <label className="env-secret-toggle" title="store the value in the OS keychain">
              <input
                type="checkbox"
                checked={row.secret}
                onChange={(e) => patchRow(row.rowId, { secret: e.target.checked })}
              />
              secret
            </label>
            <button
              type="button"
              aria-label={`remove ${row.key || "environment variable"}`}
              onClick={() => setRows((rows) => rows.filter((r) => r.rowId !== row.rowId))}
            >
              ×
            </button>
          </div>
        ))}
        <button
          type="button"
          className="env-add"
          onClick={() =>
            setRows((rows) => [
              ...rows,
              { rowId: nextRowId.current++, key: "", secret: false, value: "", hasStored: false },
            ])
          }
        >
          + env var
        </button>
      </div>

      {formError && (
        <p className="form-error" role="alert">
          {formError}
        </p>
      )}

      <div className="form-actions">
        <button type="submit" disabled={submitting}>
          {submitting ? "saving…" : editing ? "Save" : "Add"}
        </button>
        {editing && (
          <button type="button" onClick={() => setEditing(null)}>
            Cancel
          </button>
        )}
      </div>
    </form>
  );
}
