import { type FormEvent, useEffect } from "react";
import { hasCapability } from "../handshake";
import {
  type PromptDef,
  type PromptResultView,
  buildArguments,
  getBody,
  promptLabel,
  usePrompts,
} from "../prompts";
import type { ServerOverview } from "../types";
import { useWorkbench } from "../workbench";

function PromptList({
  prompts,
  selected,
  onSelect,
}: {
  prompts: PromptDef[];
  selected: string | null;
  onSelect: (name: string) => void;
}) {
  return (
    <ul className="tool-list">
      {prompts.map((prompt) => (
        <li key={prompt.name}>
          <button
            type="button"
            className={`tool-item${prompt.name === selected ? " tool-item-active" : ""}`}
            aria-pressed={prompt.name === selected}
            onClick={() => onSelect(prompt.name)}
          >
            <span className="tool-name">{promptLabel(prompt)}</span>
            {prompt.description && (
              <span className="tool-description">{prompt.description}</span>
            )}
          </button>
        </li>
      ))}
    </ul>
  );
}

function Messages({ view, raw }: { view: PromptResultView; raw: string | null }) {
  return (
    <div className="tools-result" role="status">
      {view.description && <p className="tool-blurb">{view.description}</p>}
      {view.messages.map((message) => (
        <div className="result-block" key={message.id}>
          <span className="result-label">
            {message.role}
            {message.label ? ` · ${message.label}` : ""}
          </span>
          <pre className={message.kind === "text" ? "result-text" : "result-json"}>
            {message.body}
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

/** `prompts/list` as a list, declared arguments as a form, `prompts/get`
 * rendered as the messages it returns. */
export function PromptBrowser({ target }: { target: ServerOverview | null }) {
  const targetId = target?.id ?? null;
  const serverId = usePrompts((s) => s.serverId);
  const prompts = usePrompts((s) => s.prompts);
  const loading = usePrompts((s) => s.loading);
  const loadError = usePrompts((s) => s.loadError);
  const selected = usePrompts((s) => s.selected);
  const values = usePrompts((s) => s.values);
  const fieldErrors = usePrompts((s) => s.fieldErrors);
  const getting = usePrompts((s) => s.getting);
  const result = usePrompts((s) => s.result);
  const rawResult = usePrompts((s) => s.rawResult);
  const getError = usePrompts((s) => s.getError);
  const load = usePrompts((s) => s.load);
  const select = usePrompts((s) => s.select);
  const setValue = usePrompts((s) => s.setValue);
  const get = usePrompts((s) => s.get);

  const timeoutS = useWorkbench((s) => s.timeoutS);
  const setBody = useWorkbench((s) => s.setBody);
  const setMode = useWorkbench((s) => s.setMode);

  useEffect(() => {
    if (targetId !== serverId) void load(targetId);
  }, [targetId, serverId, load]);

  if (!target) {
    return <p className="empty">Start a server to browse its prompts.</p>;
  }

  const prompt = prompts.find((p) => p.name === selected) ?? null;
  const advertised = hasCapability(target.handshake, "prompts");

  const submit = (event: FormEvent) => {
    event.preventDefault();
    void get(target.name, timeoutS);
  };

  const openInEditor = () => {
    if (!prompt) return;
    const built = buildArguments(prompt, values);
    setBody(getBody(prompt.name, built.ok ? built.arguments : {}));
    setMode("raw");
  };

  return (
    <div className="tools-panes">
      <aside className="tools-sidebar" aria-label="prompts">
        {loading && <p className="empty">listing prompts…</p>}
        {loadError && (
          <p className="tools-load-error" role="alert">
            {loadError}{" "}
            <button type="button" onClick={() => void load(targetId)}>
              retry
            </button>
          </p>
        )}
        {!loading && !loadError && prompts.length === 0 && (
          <p className="empty">
            {advertised ? "This server lists no prompts." : "This server does not advertise prompts."}
          </p>
        )}
        {prompts.length > 0 && (
          <PromptList prompts={prompts} selected={selected} onSelect={select} />
        )}
      </aside>

      <div className="tools-detail">
        {!prompt ? (
          prompts.length > 0 && <p className="empty">Pick a prompt to see its arguments.</p>
        ) : (
          <>
            <h3 className="tool-title">{promptLabel(prompt)}</h3>
            {prompt.description && <p className="tool-blurb">{prompt.description}</p>}

            <form className="tool-form" onSubmit={submit}>
              {(prompt.arguments ?? []).length === 0 ? (
                <p className="field-help">This prompt takes no arguments.</p>
              ) : (
                (prompt.arguments ?? []).map((argument) => {
                  const id = `prompt-arg-${argument.name}`;
                  const error = fieldErrors[argument.name];
                  return (
                    <div className={`field${error ? " field-invalid" : ""}`} key={argument.name}>
                      <label htmlFor={id} className="field-label">
                        <span className="field-name">{argument.name}</span>
                        {argument.required && (
                          <span className="field-required" title="required">
                            *
                          </span>
                        )}
                        <span className="field-kind">string</span>
                      </label>
                      <input
                        id={id}
                        type="text"
                        value={values[argument.name] ?? ""}
                        onChange={(e) => setValue(argument.name, e.target.value)}
                      />
                      {argument.description && (
                        <p className="field-help">{argument.description}</p>
                      )}
                      {error && (
                        <p className="field-error" role="alert">
                          {error}
                        </p>
                      )}
                    </div>
                  );
                })
              )}
              <div className="tools-actions">
                <button type="submit" className="send-button" disabled={getting}>
                  {getting ? "getting…" : "get"}
                </button>
                <button type="button" className="ghost-button" onClick={openInEditor}>
                  open in editor
                </button>
              </div>
            </form>

            {getError && (
              <p className="tools-call-error" role="alert">
                {getError}
              </p>
            )}
            {result && <Messages view={result} raw={rawResult} />}
          </>
        )}
      </div>
    </div>
  );
}
