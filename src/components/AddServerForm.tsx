import { type FormEvent, useState } from "react";
import { usePanel } from "../store";

/** Naive whitespace split — quoting support can come with the workbench. */
function parseArgs(raw: string): string[] {
  return raw.trim().length === 0 ? [] : raw.trim().split(/\s+/);
}

export function AddServerForm() {
  const add = usePanel((s) => s.add);
  const [name, setName] = useState("");
  const [command, setCommand] = useState("");
  const [args, setArgs] = useState("");
  const [autoStart, setAutoStart] = useState(false);
  const [submitting, setSubmitting] = useState(false);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (submitting || !name.trim() || !command.trim()) return;
    setSubmitting(true);
    const added = await add({
      name: name.trim(),
      command: command.trim(),
      args: parseArgs(args),
      env: {},
      cwd: null,
      auto_start: autoStart,
    });
    setSubmitting(false);
    // On failure the fields keep the user's input for correction; the
    // store already surfaced the error banner.
    if (!added) return;
    setName("");
    setCommand("");
    setArgs("");
    setAutoStart(false);
  };

  return (
    <form className="add-form" onSubmit={(e) => void submit(e)}>
      <h2>Add server</h2>
      <div className="add-fields">
        <input
          placeholder="name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          required
        />
        <input
          placeholder="command (e.g. npx)"
          value={command}
          onChange={(e) => setCommand(e.target.value)}
          required
        />
        <input
          placeholder="args (e.g. -y some-mcp-server)"
          value={args}
          onChange={(e) => setArgs(e.target.value)}
        />
        <label className="auto-start-field" title="start this server when MCPanel launches">
          <input
            type="checkbox"
            checked={autoStart}
            onChange={(e) => setAutoStart(e.target.checked)}
          />
          auto-start
        </label>
        <button type="submit" disabled={submitting}>
          {submitting ? "adding…" : "Add"}
        </button>
      </div>
    </form>
  );
}
