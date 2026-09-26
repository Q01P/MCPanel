import { create } from "zustand";
import { envelope, rpc } from "./rpc";
import type { AppEvent } from "./types";
import { useWorkbench } from "./workbench";

// The resources browser: `resources/list` and `resources/templates/list`
// as one list, a template's URI variables as a form, `resources/read`
// rendered by content type. Same shape as the tools browser, on purpose.

export interface ResourceDef {
  uri: string;
  name?: string;
  title?: string;
  description?: string;
  mimeType?: string;
  size?: number;
}

export interface ResourceTemplateDef {
  uriTemplate: string;
  name?: string;
  title?: string;
  description?: string;
  mimeType?: string;
}

/** What the list shows: a concrete resource, or a template the user fills
 * in before reading. */
export type ResourceItem =
  | { kind: "resource"; resource: ResourceDef }
  | { kind: "template"; template: ResourceTemplateDef };

export const itemKey = (item: ResourceItem): string =>
  item.kind === "resource" ? `r:${item.resource.uri}` : `t:${item.template.uriTemplate}`;

export const itemLabel = (item: ResourceItem): string =>
  item.kind === "resource"
    ? (item.resource.title ?? item.resource.name ?? item.resource.uri)
    : (item.template.title ?? item.template.name ?? item.template.uriTemplate);

/** RFC 6570 variables in a URI template, in order of first appearance.
 * Operators (`+`, `#`, `?`, `&`, `/`, `.`, `;`) and modifiers (`*`, `:n`)
 * are stripped; a comma-separated group yields each name. */
export function templateVariables(uriTemplate: string): string[] {
  const names: string[] = [];
  for (const match of uriTemplate.matchAll(/\{([^}]*)\}/g)) {
    const body = (match[1] ?? "").replace(/^[+#?&/.;]/, "");
    for (const spec of body.split(",")) {
      const name = spec.replace(/\*$/, "").replace(/:\d+$/, "").trim();
      if (name !== "" && !names.includes(name)) names.push(name);
    }
  }
  return names;
}

const encodeReserved = (value: string) => encodeURI(value).replace(/%25/g, "%");

/** Expand the subset of RFC 6570 resource templates use in practice:
 * simple `{var}`, reserved `{+var}`, fragment `{#var}`, path `{/var}`,
 * and query `{?a,b}` / `{&a,b}`. Missing variables expand to nothing. */
export function expandTemplate(uriTemplate: string, values: Record<string, string>): string {
  return uriTemplate.replace(/\{([^}]*)\}/g, (_whole, body: string) => {
    const operator = /^[+#?&/.;]/.test(body) ? body[0] : "";
    const names = body
      .slice(operator === "" ? 0 : 1)
      .split(",")
      .map((spec) => spec.replace(/\*$/, "").replace(/:\d+$/, "").trim())
      .filter((name) => name !== "");
    const present = names.filter((name) => (values[name] ?? "") !== "");
    if (present.length === 0) return "";
    switch (operator) {
      case "+":
        return present.map((name) => encodeReserved(values[name] ?? "")).join(",");
      case "#":
        return `#${present.map((name) => encodeReserved(values[name] ?? "")).join(",")}`;
      case "/":
        return `/${present.map((name) => encodeURIComponent(values[name] ?? "")).join("/")}`;
      case ".":
        return `.${present.map((name) => encodeURIComponent(values[name] ?? "")).join(".")}`;
      case ";":
        return present
          .map((name) => `;${name}=${encodeURIComponent(values[name] ?? "")}`)
          .join("");
      case "?":
      case "&":
        return present
          .map(
            (name, index) =>
              `${index === 0 ? operator : "&"}${name}=${encodeURIComponent(values[name] ?? "")}`,
          )
          .join("");
      default:
        return present.map((name) => encodeURIComponent(values[name] ?? "")).join(",");
    }
  });
}

/** One rendered piece of a `resources/read` result. */
export interface ContentView {
  id: number;
  uri: string;
  mimeType?: string;
  kind: "text" | "json" | "blob";
  /** Text or pretty JSON; for blobs a one-line description (the base64
   * itself is only in the raw result). */
  body: string;
}

const pretty = (value: unknown) => JSON.stringify(value, null, 2);

const isJsonMime = (mime: string | undefined) =>
  mime !== undefined && (mime === "application/json" || mime.endsWith("+json"));

/** Rough decoded size of a base64 payload, for the blob placeholder. */
function base64Bytes(blob: string): number {
  const padding = blob.endsWith("==") ? 2 : blob.endsWith("=") ? 1 : 0;
  return Math.max(0, Math.floor((blob.length * 3) / 4) - padding);
}

export function describeContents(result: unknown): ContentView[] {
  const views: ContentView[] = [];
  const contents = (result as { contents?: unknown } | null)?.contents;
  if (!Array.isArray(contents)) {
    views.push({ id: 0, uri: "", kind: "json", body: pretty(result) });
    return views;
  }
  for (const item of contents as {
    uri?: unknown;
    mimeType?: unknown;
    text?: unknown;
    blob?: unknown;
  }[]) {
    const uri = typeof item?.uri === "string" ? item.uri : "";
    const mimeType = typeof item?.mimeType === "string" ? item.mimeType : undefined;
    const id = views.length;
    if (typeof item?.text === "string") {
      let body = item.text;
      let kind: ContentView["kind"] = "text";
      if (isJsonMime(mimeType)) {
        try {
          body = pretty(JSON.parse(item.text));
          kind = "json";
        } catch {
          // Claimed JSON but isn't — show the text as sent.
        }
      }
      views.push({ id, uri, mimeType, kind, body });
    } else if (typeof item?.blob === "string") {
      views.push({
        id,
        uri,
        mimeType,
        kind: "blob",
        body: `binary content, ${base64Bytes(item.blob)} bytes${mimeType ? ` (${mimeType})` : ""} — see the raw result for the base64`,
      });
    } else {
      views.push({ id, uri, mimeType, kind: "json", body: pretty(item) });
    }
  }
  if (views.length === 0) views.push({ id: 0, uri: "", kind: "text", body: "(no contents)" });
  return views;
}

/** The JSON-RPC a read amounts to, for history and the raw editor. */
export function readBody(uri: string): string {
  return pretty(envelope("resources/read", { uri }));
}

/** Both lists are paginated; a runaway cursor must not hang the browser. */
const MAX_PAGES = 50;

type Fetched<T> = { ok: true; items: T[] } | { ok: false; message: string };

async function fetchPages<T>(
  serverId: number,
  method: string,
  key: string,
  accept: (value: unknown) => value is T,
): Promise<Fetched<T>> {
  const items: T[] = [];
  let cursor: string | undefined;
  for (let page = 0; page < MAX_PAGES; page++) {
    const outcome = await rpc(
      serverId,
      method,
      cursor === undefined ? {} : { cursor },
      null, // the server's own timeout
    );
    if (outcome.kind !== "result") return { ok: false, message: outcome.message };
    const body = outcome.result as Record<string, unknown> | undefined;
    const list = body?.[key];
    if (Array.isArray(list)) {
      for (const entry of list) if (accept(entry)) items.push(entry);
    }
    const next = body?.nextCursor;
    if (typeof next !== "string" || next === "") break;
    cursor = next;
  }
  return { ok: true, items };
}

const isResource = (value: unknown): value is ResourceDef =>
  !!value && typeof (value as ResourceDef).uri === "string";
const isTemplate = (value: unknown): value is ResourceTemplateDef =>
  !!value && typeof (value as ResourceTemplateDef).uriTemplate === "string";

/** Templates are optional per spec, so a server that only lists concrete
 * resources answers `-32601` — that is "none", not a failure. */
async function fetchItems(serverId: number): Promise<Fetched<ResourceItem>> {
  const resources = await fetchPages(serverId, "resources/list", "resources", isResource);
  if (!resources.ok) return resources;
  const templates = await fetchPages(
    serverId,
    "resources/templates/list",
    "resourceTemplates",
    isTemplate,
  );
  const items: ResourceItem[] = resources.items.map((resource) => ({
    kind: "resource",
    resource,
  }));
  if (templates.ok) {
    for (const template of templates.items) items.push({ kind: "template", template });
  } else if (!/-32601|method not found/i.test(templates.message)) {
    return templates;
  }
  return { ok: true, items };
}

interface ResourcesState {
  serverId: number | null;
  items: ResourceItem[];
  loading: boolean;
  loadError: string | null;
  /** `itemKey` of the selection. */
  selected: string | null;
  /** Template variable values, by name. */
  values: Record<string, string>;
  reading: boolean;
  contents: ContentView[] | null;
  rawResult: string | null;
  readError: string | null;
  load: (serverId: number | null) => Promise<void>;
  refresh: () => Promise<void>;
  applyEvent: (event: AppEvent) => void;
  select: (key: string | null) => void;
  setValue: (name: string, value: string) => void;
  /** The URI a read would fetch — the resource's, or the template expanded. */
  targetUri: () => string | null;
  read: (serverName: string, timeoutS: number | null) => Promise<void>;
}

const EMPTY_READ = { contents: null, rawResult: null, readError: null } as const;

export const useResources = create<ResourcesState>((set, get) => ({
  serverId: null,
  items: [],
  loading: false,
  loadError: null,
  selected: null,
  values: {},
  reading: false,
  ...EMPTY_READ,

  load: async (serverId) => {
    set({ serverId, items: [], selected: null, values: {}, loadError: null, ...EMPTY_READ });
    if (serverId == null) return;
    set({ loading: true });
    const fetched = await fetchItems(serverId);
    if (get().serverId !== serverId) return;
    if (!fetched.ok) {
      set({ loading: false, loadError: fetched.message });
      return;
    }
    set({ items: fetched.items, loading: false });
  },

  refresh: async () => {
    const { serverId, selected } = get();
    if (serverId == null) return;
    const fetched = await fetchItems(serverId);
    if (get().serverId !== serverId) return;
    if (!fetched.ok) {
      set({ loadError: fetched.message });
      return;
    }
    const survives = selected != null && fetched.items.some((item) => itemKey(item) === selected);
    set({ items: fetched.items, loadError: null, selected: survives ? selected : null });
  },

  applyEvent: (event) => {
    if (event.type !== "notification" || event.server_id !== get().serverId) return;
    const method = (event.payload as { method?: unknown } | null)?.method;
    if (method === "notifications/resources/list_changed") void get().refresh();
  },

  select: (key) => {
    const item = get().items.find((candidate) => itemKey(candidate) === key);
    set({ selected: item ? key : null, values: {}, ...EMPTY_READ });
  },

  setValue: (name, value) => set({ values: { ...get().values, [name]: value } }),

  targetUri: () => {
    const { items, selected, values } = get();
    const item = items.find((candidate) => itemKey(candidate) === selected);
    if (!item) return null;
    return item.kind === "resource"
      ? item.resource.uri
      : expandTemplate(item.template.uriTemplate, values);
  },

  read: async (serverName, timeoutS) => {
    const { serverId, reading } = get();
    const uri = get().targetUri();
    if (serverId == null || reading || uri == null || uri === "") return;
    set({ reading: true, ...EMPTY_READ });
    const outcome = await rpc(serverId, "resources/read", { uri }, timeoutS);
    useWorkbench.getState().addHistory(serverId, serverName, readBody(uri));
    switch (outcome.kind) {
      case "result":
        set({
          reading: false,
          contents: describeContents(outcome.result),
          rawResult: pretty(outcome.result),
        });
        break;
      case "error":
        set({ reading: false, readError: `server error ${outcome.code}: ${outcome.message}` });
        break;
      case "transport":
        set({ reading: false, readError: outcome.message });
        break;
    }
  },
}));
