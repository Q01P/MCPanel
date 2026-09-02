import { create } from "zustand";
import { envelope, rpc } from "./rpc";
import { DEFAULT_TIMEOUT_S, useWorkbench } from "./workbench";

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

export interface ToolDef {
  name: string;
  description?: string;
  inputSchema?: JsonSchema;
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
  select: (name: string | null) => void;
  setValue: (name: string, value: string | boolean) => void;
  call: (serverName: string, timeoutS: number) => Promise<void>;
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
    const tools: ToolDef[] = [];
    let cursor: string | undefined;
    for (let page = 0; page < MAX_PAGES; page++) {
      const outcome = await rpc(
        serverId,
        "tools/list",
        cursor === undefined ? {} : { cursor },
        DEFAULT_TIMEOUT_S,
      );
      // A stale reply from a server we've since left must not land.
      if (get().serverId !== serverId) return;
      if (outcome.kind !== "result") {
        set({ loading: false, loadError: outcome.message });
        return;
      }
      const body = outcome.result as { tools?: unknown; nextCursor?: unknown } | undefined;
      if (Array.isArray(body?.tools)) {
        for (const tool of body.tools as ToolDef[]) {
          if (tool && typeof tool.name === "string") tools.push(tool);
        }
      }
      if (typeof body?.nextCursor !== "string" || body.nextCursor === "") break;
      cursor = body.nextCursor;
    }
    set({ tools, loading: false });
  },

  select: (name) => {
    const tool = get().tools.find((t) => t.name === name);
    set({
      selected: tool ? name : null,
      values: tool ? initialValues(formFromSchema(tool.inputSchema)) : {},
      ...EMPTY_CALL,
    });
  },

  setValue: (name, value) =>
    set({
      values: { ...get().values, [name]: value },
      // Editing a field retires its error; the rest stand until re-checked.
      fieldErrors: Object.fromEntries(
        Object.entries(get().fieldErrors).filter(([key]) => key !== name),
      ),
    }),

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
