import { type FormEvent, useEffect } from "react";
import { buildArguments, callBody, formFromSchema, useTools } from "../tools";
import type { Field, ResultView, ToolDef } from "../tools";
import type { ServerOverview } from "../types";
import { useWorkbench } from "../workbench";

function FieldInput({
  field,
  value,
  error,
  onChange,
}: {
  field: Field;
  value: string | boolean;
  error?: string;
  onChange: (value: string | boolean) => void;
}) {
  const id = `tool-field-${field.name}`;
  const label = (
    <label htmlFor={id} className="field-label">
      <span className="field-name">{field.name}</span>
      {field.required && (
        <span className="field-required" title="required">
          *
        </span>
      )}
      <span className="field-kind">{field.kind}</span>
    </label>
  );

  let control: React.ReactNode;
  switch (field.kind) {
    case "boolean":
      control = (
        <input
          id={id}
          type="checkbox"
          checked={value === true}
          onChange={(e) => onChange(e.target.checked)}
        />
      );
      break;
    case "enum":
      control = (
        <select id={id} value={String(value)} onChange={(e) => onChange(e.target.value)}>
          {!field.required && <option value="">(unset)</option>}
          {field.options?.map((option) => (
            <option key={option} value={option}>
              {option}
            </option>
          ))}
        </select>
      );
      break;
    case "number":
    case "integer":
      control = (
        <input
          id={id}
          type="text"
          inputMode="decimal"
          value={String(value)}
          onChange={(e) => onChange(e.target.value)}
        />
      );
      break;
    case "json":
      control = (
        <textarea
          id={id}
          rows={3}
          spellCheck={false}
          value={String(value)}
          placeholder="JSON"
          onChange={(e) => onChange(e.target.value)}
        />
      );
      break;
    default:
      control = (
        <input
          id={id}
          type="text"
          value={String(value)}
          onChange={(e) => onChange(e.target.value)}
        />
      );
  }

  return (
    <div className={`field${error ? " field-invalid" : ""}`}>
      {label}
      {control}
      {field.description && <p className="field-help">{field.description}</p>}
      {error && (
        <p className="field-error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

function Result({ view, raw }: { view: ResultView; raw: string | null }) {
  return (
    <div className={`tools-result${view.isError ? " tools-result-error" : ""}`} role="status">
      {view.isError && <p className="tools-result-flag">the tool reported an error</p>}
      {view.blocks.map((block) => (
        <div className="result-block" key={block.id}>
          {block.label && <span className="result-label">{block.label}</span>}
          <pre className={block.kind === "text" ? "result-text" : "result-json"}>
            {block.body}
          </pre>
        </div>
      ))}
      {raw && (
        <details className="result-raw">
          <summary>raw result</summary>
          <pre className="result-json">{raw}</pre>
        </details>
      )}
    </div>
  );
}

function ToolList({
  tools,
  selected,
  onSelect,
}: {
  tools: ToolDef[];
  selected: string | null;
  onSelect: (name: string) => void;
}) {
  return (
    <ul className="tool-list">
      {tools.map((tool) => (
        <li key={tool.name}>
          <button
            type="button"
            className={`tool-item${tool.name === selected ? " tool-item-active" : ""}`}
            aria-pressed={tool.name === selected}
            onClick={() => onSelect(tool.name)}
          >
            <span className="tool-name">{tool.name}</span>
            {tool.description && <span className="tool-description">{tool.description}</span>}
          </button>
        </li>
      ))}
    </ul>
  );
}

/**
 * `tools/list` as a list, a tool's `inputSchema` as a form, `tools/call`
 * from a button. The raw JSON workbench stays one click away: every call
 * lands in its history, and "open in editor" hands over the exact request.
 */
export function ToolBrowser({ target }: { target: ServerOverview | null }) {
  const targetId = target?.id ?? null;
  const serverId = useTools((s) => s.serverId);
  const tools = useTools((s) => s.tools);
  const loading = useTools((s) => s.loading);
  const loadError = useTools((s) => s.loadError);
  const selected = useTools((s) => s.selected);
  const values = useTools((s) => s.values);
  const calling = useTools((s) => s.calling);
  const result = useTools((s) => s.result);
  const rawResult = useTools((s) => s.rawResult);
  const callError = useTools((s) => s.callError);
  const fieldErrors = useTools((s) => s.fieldErrors);
  const load = useTools((s) => s.load);
  const select = useTools((s) => s.select);
  const setValue = useTools((s) => s.setValue);
  const call = useTools((s) => s.call);

  const timeoutS = useWorkbench((s) => s.timeoutS);
  const setBody = useWorkbench((s) => s.setBody);
  const setMode = useWorkbench((s) => s.setMode);

  // The list follows the target: a new server (or none) means a new list.
  useEffect(() => {
    if (targetId !== serverId) void load(targetId);
  }, [targetId, serverId, load]);

  const tool = tools.find((t) => t.name === selected) ?? null;
  const form = tool ? formFromSchema(tool.inputSchema) : null;

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (target) void call(target.name, timeoutS);
  };

  const openInEditor = () => {
    if (!tool || !form) return;
    const built = buildArguments(form, values);
    setBody(callBody(tool.name, built.ok ? built.arguments : {}));
    setMode("raw");
  };

  if (!target) {
    return <p className="empty">Start a server to browse its tools.</p>;
  }

  return (
    <div className="tools-panes">
      <aside className="tools-sidebar" aria-label="tools">
        {loading && <p className="empty">listing tools…</p>}
        {loadError && (
          <p className="tools-load-error" role="alert">
            {loadError}{" "}
            <button type="button" onClick={() => void load(targetId)}>
              retry
            </button>
          </p>
        )}
        {!loading && !loadError && tools.length === 0 && (
          <p className="empty">This server advertises no tools.</p>
        )}
        {tools.length > 0 && <ToolList tools={tools} selected={selected} onSelect={select} />}
      </aside>

      <div className="tools-detail">
        {!tool || !form ? (
          tools.length > 0 && <p className="empty">Pick a tool to see its inputs.</p>
        ) : (
          <>
            <h3 className="tool-title">{tool.name}</h3>
            {tool.description && <p className="tool-blurb">{tool.description}</p>}

            <form className="tool-form" onSubmit={submit}>
              {form.freeform ? (
                <FieldInput
                  field={{ name: "*", kind: "json", required: false }}
                  value={values["*"] ?? ""}
                  error={fieldErrors["*"]}
                  onChange={(v) => setValue("*", v)}
                />
              ) : form.fields.length === 0 ? (
                <p className="field-help">This tool takes no arguments.</p>
              ) : (
                form.fields.map((field) => (
                  <FieldInput
                    key={field.name}
                    field={field}
                    value={values[field.name] ?? (field.kind === "boolean" ? false : "")}
                    error={fieldErrors[field.name]}
                    onChange={(v) => setValue(field.name, v)}
                  />
                ))
              )}

              <div className="tools-actions">
                <button type="submit" className="send-button" disabled={calling}>
                  {calling ? "calling…" : "call"}
                </button>
                <button type="button" className="ghost-button" onClick={openInEditor}>
                  open in editor
                </button>
              </div>
            </form>

            {callError && (
              <p className="tools-call-error" role="alert">
                {callError}
              </p>
            )}
            {result && <Result view={result} raw={rawResult} />}
          </>
        )}
      </div>
    </div>
  );
}
