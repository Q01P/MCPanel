import { create } from "zustand";
import { envelope, rpc } from "./rpc";
import type { AppEvent } from "./types";
import { useWorkbench } from "./workbench";

// The tools browser: `tools/list` rendered as a list, a tool's `inputSchema`
// rendered as a form, `tools/call` fired from it. The subset of JSON Schema
// handled here is deliberately small — scalars, enums of strings — and
// everything else falls back to a JSON textarea rather than a guess.

/** The slice of JSON Schema a tool's `inputSchema` uses in practice. */
export interface JsonSchema {
  type?: string | string[];
  properties?: Record<string, JsonSchema>;
  required?: string[];
  enum?: unknown[];
  description?: string;
  default?: unknown;
  title?: string;
  [extra: string]: unknown;
}

/** MCP tool annotations (hints, not guarantees — the spec is explicit). */
export interface ToolAnnotations {
  title?: string;
  readOnlyHint?: boolean;
  destructiveHint?: boolean;
  idempotentHint?: boolean;
  openWorldHint?: boolean;
}

export interface ToolDef {
  name: string;
  title?: string;
  description?: string;
  inputSchema?: JsonSchema;
  annotations?: ToolAnnotations;
}

export interface Badge {
  label: string;
  tone: "ok" | "warn" | "neutral";
}

/** Badges for the hints a tool actually sets. Absent hints are not
 * defaulted (the spec defaults destructiveHint to true, which would badge
 * every unannotated tool "destructive" — noise, not information). */
export function annotationBadges(tool: ToolDef): Badge[] {
  const a = tool.annotations ?? {};
  const badges: Badge[] = [];
  if (a.readOnlyHint === true) badges.push({ label: "read-only", tone: "ok" });
  if (a.destructiveHint === true) badges.push({ label: "destructive", tone: "warn" });
  if (a.destructiveHint === false && a.readOnlyHint !== true) {
    badges.push({ label: "non-destructive", tone: "ok" });
  }
  if (a.idempotentHint === true) badges.push({ label: "idempotent", tone: "neutral" });
  if (a.openWorldHint === true) badges.push({ label: "open world", tone: "neutral" });
  if (a.openWorldHint === false) badges.push({ label: "closed world", tone: "neutral" });
  return badges;
}

/** Rough context cost: ~4 characters per token, the usual back-of-envelope
 * figure for English and JSON. Good enough to compare tools and to notice a
 * server that costs 10k tokens before the first turn. */
export function estimateTokens(text: string): number {
  return Math.ceil(text.length / 4);
}

/** What a client pays in context to know this tool exists: its listing
 * entry as JSON (name, description, schema, annotations). */
export function toolCost(tool: ToolDef): number {
  return estimateTokens(JSON.stringify(tool));
}

export function listCost(tools: ToolDef[]): number {
  return tools.reduce((total, tool) => total + toolCost(tool), 0);
}

/** Description-length thresholds from the Inspector request the official
 * tool declined: warn at 500 characters, alert at 1000. */
export const DESCRIPTION_WARN_CHARS = 500;
export const DESCRIPTION_ALERT_CHARS = 1000;

export function descriptionWarning(tool: ToolDef): string | null {
  const length = tool.description?.length ?? 0;
  if (length >= DESCRIPTION_ALERT_CHARS) return `very long description (${length} chars)`;
  if (length >= DESCRIPTION_WARN_CHARS) return `long description (${length} chars)`;
  return null;
}

/** Last-used inputs per (server id, tool), kept across restarts and
 * reconnects so the edit → restart → retest loop doesn't start from blank
 * fields every time. */
export const INPUTS_STORAGE_KEY = "mcpanel.toolInputs.v1";

const inputsKey = (serverId: number, tool: string) => `${serverId}\u0000${tool}`;

function loadInputs(): Record<string, Values> {
  try {
    const raw = window.localStorage.getItem(INPUTS_STORAGE_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : null;
    return parsed && typeof parsed === "object" && !Array.isArray(parsed)
      ? (parsed as Record<string, Values>)
      : {};
  } catch {
    return {};
  }
}

/** Bounded so a long-lived install can't grow the entry without limit. */
const INPUTS_CAP = 200;

export function rememberInputs(serverId: number, tool: string, values: Values): void {
  try {
    const all = loadInputs();
    delete all[inputsKey(serverId, tool)];
    const entries = Object.entries(all).slice(-(INPUTS_CAP - 1));
    entries.push([inputsKey(serverId, tool), values]);
    window.localStorage.setItem(INPUTS_STORAGE_KEY, JSON.stringify(Object.fromEntries(entries)));
  } catch {
    // Not remembered this time.
  }
}

/** Schema defaults, overlaid with whatever was last typed for fields that
 * still exist. A field whose kind changed keeps the default — a remembered
 * string in a now-boolean field would be nonsense. */
export function recallInputs(serverId: number, tool: string, form: Form): Values {
  const values = initialValues(form);
  const remembered = loadInputs()[inputsKey(serverId, tool)];
  if (!remembered) return values;
  const kinds = new Map(form.fields.map((field) => [field.name, field.kind]));
  for (const [name, value] of Object.entries(remembered)) {
    if (form.freeform && name === "*" && typeof value === "string") {
      values[name] = value;
      continue;
    }
    const kind = kinds.get(name);
    if (kind === undefined) continue;
    if (kind === "boolean" ? typeof value === "boolean" : typeof value === "string") {
      values[name] = value;
    }
  }
  return values;
}

export type FieldKind = "string" | "number" | "integer" | "boolean" | "enum" | "json";

export interface Field {
  name: string;
  kind: FieldKind;
  required: boolean;
  description?: string;
  /** For `enum`: the allowed values, in schema order. */
  options?: string[];
  default?: unknown;
}

/** What the form should show for a tool. `freeform` means the schema gave
 * us nothing to build fields from, so the whole arguments object is one
 * JSON textarea. */
export interface Form {
  fields: Field[];
  freeform: boolean;
}

/** A schema may say `["string", "null"]`; the non-null type decides. */
function primaryType(schema: JsonSchema): string | undefined {
  const type = schema.type;
  if (Array.isArray(type)) return type.find((t) => t !== "null");
  return type;
}

function kindOf(schema: JsonSchema): FieldKind {
  const values = schema.enum;
  if (Array.isArray(values) && values.length > 0 && values.every((v) => typeof v === "string")) {
    return "enum";
  }
  switch (primaryType(schema)) {
    case "string":
      return "string";
    case "number":
      return "number";
    case "integer":
      return "integer";
    case "boolean":
      return "boolean";
    default:
      return "json";
  }
}

export function formFromSchema(schema: JsonSchema | undefined): Form {
  const properties = schema?.properties;
  if (!schema || primaryType(schema) !== "object" || !properties) {
    return { fields: [], freeform: true };
  }
  const required = new Set(schema.required ?? []);
  const fields = Object.entries(properties).map(([name, property]) => {
    const kind = kindOf(property);
    const field: Field = { name, kind, required: required.has(name) };
    if (property.description) field.description = property.description;
    if (kind === "enum") field.options = property.enum as string[];
    if (property.default !== undefined) field.default = property.default;
    return field;
  });
  return { fields, freeform: false };
}

/** What the form holds per field: text for everything but checkboxes. */
export type Values = Record<string, string | boolean>;

/** Initial values from schema defaults; a boolean without one starts off. */
export function initialValues(form: Form): Values {
  const values: Values = {};
  for (const field of form.fields) {
    if (field.kind === "boolean") {
      values[field.name] = field.default === true;
    } else if (field.default !== undefined) {
      values[field.name] =
        field.kind === "json" ? JSON.stringify(field.default) : String(field.default);
    } else {
      values[field.name] = "";
    }
  }
  return values;
}

export type BuildResult =
  | { ok: true; arguments: Record<string, unknown> }
  | { ok: false; errors: Record<string, string> };

/** Text field → typed argument. Coercion is strict: `"12abc"` is not a
 * number, `1.5` is not an integer. An empty optional field is omitted, not
 * sent as `""` — a tool can tell absent from blank; we can't tell for it. */
export function buildArguments(form: Form, values: Values): BuildResult {
  const errors: Record<string, string> = {};
  const args: Record<string, unknown> = {};

  if (form.freeform) {
    const raw = String(values["*"] ?? "").trim();
    if (raw === "") return { ok: true, arguments: {} };
    try {
      const parsed: unknown = JSON.parse(raw);
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
        return { ok: false, errors: { "*": "arguments must be a JSON object" } };
      }
      return { ok: true, arguments: parsed as Record<string, unknown> };
    } catch {
      return { ok: false, errors: { "*": "not valid JSON" } };
    }
  }

  for (const field of form.fields) {
    const value = values[field.name];
    if (field.kind === "boolean") {
      // A checkbox is binary; there is no "unset" to omit.
      args[field.name] = value === true;
      continue;
    }
    const text = String(value ?? "").trim();
    if (text === "") {
      if (field.required) errors[field.name] = "required";
      continue;
    }
    switch (field.kind) {
      case "string":
      case "enum":
        args[field.name] = text;
        break;
      case "number":
      case "integer": {
        const n = Number(text);
        if (!Number.isFinite(n)) {
          errors[field.name] = "must be a number";
        } else if (field.kind === "integer" && !Number.isInteger(n)) {
          errors[field.name] = "must be an integer";
        } else {
          args[field.name] = n;
        }
        break;
      }
      case "json":
        try {
          args[field.name] = JSON.parse(text);
        } catch {
          errors[field.name] = "not valid JSON";
        }
        break;
    }
  }

  return Object.keys(errors).length > 0 ? { ok: false, errors } : { ok: true, arguments: args };
}

/** A `tools/call` result rendered for people: text content as text,
 * anything else as labelled JSON. */
export interface ResultBlock {
  /** Position in the result — a stable React key; content can repeat. */
  id: number;
  kind: "text" | "json";
  label?: string;
  body: string;
}

export interface ResultView {
  isError: boolean;
  blocks: ResultBlock[];
}

const pretty = (value: unknown) => JSON.stringify(value, null, 2);

export function describeResult(result: unknown): ResultView {
  const view: ResultView = { isError: false, blocks: [] };
  const push = (block: Omit<ResultBlock, "id">) =>
    view.blocks.push({ id: view.blocks.length, ...block });

  if (!result || typeof result !== "object") {
    push({ kind: "json", body: pretty(result) });
    return view;
  }
  const record = result as { content?: unknown; isError?: unknown; structuredContent?: unknown };
  view.isError = record.isError === true;

  if (Array.isArray(record.content)) {
    for (const item of record.content as { type?: unknown; text?: unknown; mimeType?: unknown }[]) {
      if (item?.type === "text" && typeof item.text === "string") {
        push({ kind: "text", body: item.text });
      } else {
        const type = typeof item?.type === "string" ? item.type : "content";
        const mime = typeof item?.mimeType === "string" ? ` (${item.mimeType})` : "";
        push({ kind: "json", label: `${type}${mime}`, body: pretty(item) });
      }
    }
  }
  if (record.structuredContent !== undefined) {
    push({ kind: "json", label: "structuredContent", body: pretty(record.structuredContent) });
  }
  if (view.blocks.length === 0) {
    // Not the MCP result shape at all — show what came back.
    push({ kind: "json", body: pretty(result) });
  }
  return view;
}

/** The JSON-RPC a call amounts to — what goes into history, and what the
 * "open in editor" button hands the raw workbench. */
export function callBody(name: string, args: Record<string, unknown>): string {
  return pretty(envelope("tools/call", { name, arguments: args }));
}

/** `tools/list` is paginated; a server with a runaway cursor must not hang
 * the browser forever. */
const MAX_PAGES = 50;

/** Every page of `tools/list`, or the message of the first failure. */
async function fetchTools(
  serverId: number,
): Promise<{ ok: true; tools: ToolDef[] } | { ok: false; message: string }> {
  const tools: ToolDef[] = [];
  let cursor: string | undefined;
  for (let page = 0; page < MAX_PAGES; page++) {
    const outcome = await rpc(
      serverId,
      "tools/list",
      cursor === undefined ? {} : { cursor },
      null, // the server's own timeout
    );
    if (outcome.kind !== "result") return { ok: false, message: outcome.message };
    const body = outcome.result as { tools?: unknown; nextCursor?: unknown } | undefined;
    if (Array.isArray(body?.tools)) {
      for (const tool of body.tools as ToolDef[]) {
        if (tool && typeof tool.name === "string") tools.push(tool);
      }
    }
    if (typeof body?.nextCursor !== "string" || body.nextCursor === "") break;
    cursor = body.nextCursor;
  }
  return { ok: true, tools };
}

interface ToolsState {
  serverId: number | null;
  tools: ToolDef[];
  loading: boolean;
  loadError: string | null;
  selected: string | null;
  values: Values;
  calling: boolean;
  result: ResultView | null;
  rawResult: string | null;
  callError: string | null;
  fieldErrors: Record<string, string>;
  load: (serverId: number | null) => Promise<void>;
  /** Re-list the current server's tools in place (no loading flash). */
  refresh: () => Promise<void>;
  /** `notifications/tools/list_changed` from the shown server re-lists;
   * the selection is kept when the tool survives. */
  applyEvent: (event: AppEvent) => void;
  select: (name: string | null) => void;
  setValue: (name: string, value: string | boolean) => void;
  call: (serverName: string, timeoutS: number | null) => Promise<void>;
}

const EMPTY_CALL = {
  result: null,
  rawResult: null,
  callError: null,
  fieldErrors: {},
} as const;

export const useTools = create<ToolsState>((set, get) => ({
  serverId: null,
  tools: [],
  loading: false,
  loadError: null,
  selected: null,
  values: {},
  calling: false,
  ...EMPTY_CALL,

  load: async (serverId) => {
    // Switching servers invalidates everything shown for the old one.
    set({ serverId, tools: [], selected: null, values: {}, loadError: null, ...EMPTY_CALL });
    if (serverId == null) return;
    set({ loading: true });
    const fetched = await fetchTools(serverId);
    // A stale reply from a server we've since left must not land.
    if (get().serverId !== serverId) return;
    if (!fetched.ok) {
      set({ loading: false, loadError: fetched.message });
      return;
    }
    set({ tools: fetched.tools, loading: false });
  },

  refresh: async () => {
    const { serverId, selected } = get();
    if (serverId == null) return;
    const fetched = await fetchTools(serverId);
    if (get().serverId !== serverId) return;
    if (!fetched.ok) {
      set({ loadError: fetched.message });
      return;
    }
    const survives = selected != null && fetched.tools.some((t) => t.name === selected);
    set({ tools: fetched.tools, loadError: null });
    // The schema may have changed under the form; a fresh selection
    // rebuilds it from the new definition rather than trusting old values.
    if (selected != null && get().selected === selected) get().select(survives ? selected : null);
  },

  applyEvent: (event) => {
    if (event.type !== "notification" || event.server_id !== get().serverId) return;
    const method = (event.payload as { method?: unknown } | null)?.method;
    if (method !== "notifications/tools/list_changed") return;
    void get().refresh();
  },

  select: (name) => {
    const { tools, serverId } = get();
    const tool = tools.find((t) => t.name === name);
    set({
      selected: tool ? name : null,
      values:
        tool && serverId != null
          ? recallInputs(serverId, tool.name, formFromSchema(tool.inputSchema))
          : {},
      ...EMPTY_CALL,
    });
  },

  setValue: (name, value) => {
    const values = { ...get().values, [name]: value };
    set({
      values,
      // Editing a field retires its error; the rest stand until re-checked.
      fieldErrors: Object.fromEntries(
        Object.entries(get().fieldErrors).filter(([key]) => key !== name),
      ),
    });
    const { serverId, selected } = get();
    if (serverId != null && selected != null) rememberInputs(serverId, selected, values);
  },

  call: async (serverName, timeoutS) => {
    const { serverId, tools, selected, values, calling } = get();
    if (serverId == null || calling) return;
    const tool = tools.find((t) => t.name === selected);
    if (!tool) return;

    const built = buildArguments(formFromSchema(tool.inputSchema), values);
    if (!built.ok) {
      set({ fieldErrors: built.errors, callError: null });
      return;
    }

    set({ calling: true, ...EMPTY_CALL });
    const outcome = await rpc(
      serverId,
      "tools/call",
      { name: tool.name, arguments: built.arguments },
      timeoutS,
    );
    useWorkbench.getState().addHistory(serverId, serverName, callBody(tool.name, built.arguments));
    switch (outcome.kind) {
      case "result":
        set({
          calling: false,
          result: describeResult(outcome.result),
          rawResult: pretty(outcome.result),
        });
        break;
      case "error":
        set({ calling: false, callError: `server error ${outcome.code}: ${outcome.message}` });
        break;
      case "transport":
        set({ calling: false, callError: outcome.message });
        break;
    }
  },
}));
