import { type FormEvent, useEffect } from "react";
import { hasCapability } from "../handshake";
import {
  type ContentView,
  type ResourceItem,
  itemKey,
  itemLabel,
  readBody,
  templateVariables,
  useResources,
} from "../resources";
import type { ServerOverview } from "../types";
import { useWorkbench } from "../workbench";

function ItemList({
  items,
  selected,
  onSelect,
}: {
  items: ResourceItem[];
  selected: string | null;
  onSelect: (key: string) => void;
}) {
  return (
    <ul className="tool-list">
      {items.map((item) => {
        const key = itemKey(item);
        const def = item.kind === "resource" ? item.resource : item.template;
        return (
          <li key={key}>
            <button
              type="button"
              className={`tool-item${key === selected ? " tool-item-active" : ""}`}
              aria-pressed={key === selected}
              onClick={() => onSelect(key)}
            >
              <span className="tool-name">
                {itemLabel(item)}
                {item.kind === "template" && (
                  <span className="item-tag" title="URI template — fill in its variables">
                    template
                  </span>
                )}
              </span>
              <span className="item-uri">
                {item.kind === "resource" ? item.resource.uri : item.template.uriTemplate}
              </span>
              {def.description && <span className="tool-description">{def.description}</span>}
            </button>
          </li>
        );
      })}
    </ul>
  );
}

function Contents({ views, raw }: { views: ContentView[]; raw: string | null }) {
  return (
    <div className="tools-result" role="status">
      {views.map((view) => (
        <div className="result-block" key={view.id}>
          <span className="result-label">
            {view.uri}
            {view.mimeType ? ` (${view.mimeType})` : ""}
          </span>
          <pre className={view.kind === "text" ? "result-text" : "result-json"}>{view.body}</pre>
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

/**
 * `resources/list` + `resources/templates/list` as one list; a template's
 * URI variables as a form; `resources/read` rendered by content type.
 */
export function ResourceBrowser({ target }: { target: ServerOverview | null }) {
  const targetId = target?.id ?? null;
  const serverId = useResources((s) => s.serverId);
  const items = useResources((s) => s.items);
  const loading = useResources((s) => s.loading);
  const loadError = useResources((s) => s.loadError);
  const selected = useResources((s) => s.selected);
  const values = useResources((s) => s.values);
  const reading = useResources((s) => s.reading);
  const contents = useResources((s) => s.contents);
  const rawResult = useResources((s) => s.rawResult);
  const readError = useResources((s) => s.readError);
  const load = useResources((s) => s.load);
  const select = useResources((s) => s.select);
  const setValue = useResources((s) => s.setValue);
  const read = useResources((s) => s.read);
  const targetUri = useResources((s) => s.targetUri);

  const timeoutS = useWorkbench((s) => s.timeoutS);
  const setBody = useWorkbench((s) => s.setBody);
  const setMode = useWorkbench((s) => s.setMode);

  useEffect(() => {
    if (targetId !== serverId) void load(targetId);
  }, [targetId, serverId, load]);

  if (!target) {
    return <p className="empty">Start a server to browse its resources.</p>;
  }

  const item = items.find((candidate) => itemKey(candidate) === selected) ?? null;
  const variables = item?.kind === "template" ? templateVariables(item.template.uriTemplate) : [];
  const uri = targetUri();
  const advertised = hasCapability(target.handshake, "resources");

  const submit = (event: FormEvent) => {
    event.preventDefault();
    void read(target.name, timeoutS);
  };

  const openInEditor = () => {
    if (uri == null) return;
    setBody(readBody(uri));
    setMode("raw");
  };

  return (
    <div className="tools-panes">
      <aside className="tools-sidebar" aria-label="resources">
        {loading && <p className="empty">listing resources…</p>}
        {loadError && (
          <p className="tools-load-error" role="alert">
            {loadError}{" "}
            <button type="button" onClick={() => void load(targetId)}>
              retry
            </button>
          </p>
        )}
        {!loading && !loadError && items.length === 0 && (
          <p className="empty">
            {advertised
              ? "This server lists no resources."
              : "This server does not advertise resources."}
          </p>
        )}
        {items.length > 0 && <ItemList items={items} selected={selected} onSelect={select} />}
      </aside>

      <div className="tools-detail">
        {!item ? (
          items.length > 0 && <p className="empty">Pick a resource to read it.</p>
        ) : (
          <>
            <h3 className="tool-title">{itemLabel(item)}</h3>
            {(item.kind === "resource" ? item.resource : item.template).description && (
              <p className="tool-blurb">
                {(item.kind === "resource" ? item.resource : item.template).description}
              </p>
            )}

            <form className="tool-form" onSubmit={submit}>
              {variables.map((name) => {
                const id = `resource-var-${name}`;
                return (
                  <div className="field" key={name}>
                    <label htmlFor={id} className="field-label">
                      <span className="field-name">{name}</span>
                      <span className="field-kind">template variable</span>
                    </label>
                    <input
                      id={id}
                      type="text"
                      value={values[name] ?? ""}
                      onChange={(e) => setValue(name, e.target.value)}
                    />
                  </div>
                );
              })}
              <p className="field-help resource-uri">
                <span className="field-name">uri</span> {uri === "" ? "(fill in the template)" : uri}
              </p>
              <div className="tools-actions">
                <button
                  type="submit"
                  className="send-button"
                  disabled={reading || uri == null || uri === ""}
                >
                  {reading ? "reading…" : "read"}
                </button>
                <button type="button" className="ghost-button" onClick={openInEditor}>
                  open in editor
                </button>
              </div>
            </form>

            {readError && (
              <p className="tools-call-error" role="alert">
                {readError}
              </p>
            )}
            {contents && <Contents views={contents} raw={rawResult} />}
          </>
        )}
      </div>
    </div>
  );
}
