import { create } from "zustand";
import { envelope, rpc } from "./rpc";
import type { AppEvent } from "./types";
import { DEFAULT_TIMEOUT_S, useWorkbench } from "./workbench";

// The prompts browser: `prompts/list` as a list, a prompt's declared
// arguments as a form (all strings, per spec), `prompts/get` rendered as
// the messages it returns.

export interface PromptArgument {
  name: string;
  description?: string;
  required?: boolean;
}

export interface PromptDef {
  name: string;
  title?: string;
  description?: string;
  arguments?: PromptArgument[];
}

export const promptLabel = (prompt: PromptDef): string => prompt.title ?? prompt.name;

/** Argument values → the `arguments` object to send. Empty optionals are
 * omitted; an empty required argument is an error the form shows. */
export type BuildResult =
  | { ok: true; arguments: Record<string, string> }
  | { ok: false; errors: Record<string, string> };

export function buildArguments(prompt: PromptDef, values: Record<string, string>): BuildResult {
  const args: Record<string, string> = {};
  const errors: Record<string, string> = {};
  for (const argument of prompt.arguments ?? []) {
    const value = values[argument.name] ?? "";
    if (value === "") {
      if (argument.required) errors[argument.name] = "required";
      continue;
    }
    args[argument.name] = value;
  }
  return Object.keys(errors).length > 0 ? { ok: false, errors } : { ok: true, arguments: args };
}

/** One message of a `prompts/get` result, rendered for people. */
export interface MessageView {
  id: number;
  role: string;
  kind: "text" | "json";
  /** Non-text content is labelled by type (image, audio, resource…). */
  label?: string;
  body: string;
}

export interface PromptResultView {
  description?: string;
  messages: MessageView[];
}

const pretty = (value: unknown) => JSON.stringify(value, null, 2);

export function describePromptResult(result: unknown): PromptResultView {
  const view: PromptResultView = { messages: [] };
  const record = (result ?? {}) as { description?: unknown; messages?: unknown };
  if (typeof record.description === "string") view.description = record.description;
  if (Array.isArray(record.messages)) {
    for (const message of record.messages as { role?: unknown; content?: unknown }[]) {
      const role = typeof message?.role === "string" ? message.role : "?";
      const content = message?.content as { type?: unknown; text?: unknown } | undefined;
      const id = view.messages.length;
      if (content?.type === "text" && typeof content.text === "string") {
        view.messages.push({ id, role, kind: "text", body: content.text });
      } else {
        const label = typeof content?.type === "string" ? content.type : "content";
        view.messages.push({ id, role, kind: "json", label, body: pretty(content ?? message) });
      }
    }
  }
  if (view.messages.length === 0 && view.description === undefined) {
    view.messages.push({ id: 0, role: "?", kind: "json", body: pretty(result) });
  }
  return view;
}

export function getBody(name: string, args: Record<string, string>): string {
  return pretty(envelope("prompts/get", { name, arguments: args }));
}

const MAX_PAGES = 50;

async function fetchPrompts(
  serverId: number,
): Promise<{ ok: true; prompts: PromptDef[] } | { ok: false; message: string }> {
  const prompts: PromptDef[] = [];
  let cursor: string | undefined;
  for (let page = 0; page < MAX_PAGES; page++) {
    const outcome = await rpc(
      serverId,
      "prompts/list",
      cursor === undefined ? {} : { cursor },
      DEFAULT_TIMEOUT_S,
    );
    if (outcome.kind !== "result") return { ok: false, message: outcome.message };
    const body = outcome.result as { prompts?: unknown; nextCursor?: unknown } | undefined;
    if (Array.isArray(body?.prompts)) {
      for (const prompt of body.prompts as PromptDef[]) {
        if (prompt && typeof prompt.name === "string") prompts.push(prompt);
      }
    }
    if (typeof body?.nextCursor !== "string" || body.nextCursor === "") break;
    cursor = body.nextCursor;
  }
  return { ok: true, prompts };
}

interface PromptsState {
  serverId: number | null;
  prompts: PromptDef[];
  loading: boolean;
  loadError: string | null;
  selected: string | null;
  values: Record<string, string>;
  fieldErrors: Record<string, string>;
  getting: boolean;
  result: PromptResultView | null;
  rawResult: string | null;
  getError: string | null;
  load: (serverId: number | null) => Promise<void>;
  refresh: () => Promise<void>;
  applyEvent: (event: AppEvent) => void;
  select: (name: string | null) => void;
  setValue: (name: string, value: string) => void;
  get: (serverName: string, timeoutS: number) => Promise<void>;
}

const EMPTY_GET = { result: null, rawResult: null, getError: null, fieldErrors: {} } as const;

export const usePrompts = create<PromptsState>((set, get) => ({
  serverId: null,
  prompts: [],
  loading: false,
  loadError: null,
  selected: null,
  values: {},
  getting: false,
  ...EMPTY_GET,

  load: async (serverId) => {
    set({ serverId, prompts: [], selected: null, values: {}, loadError: null, ...EMPTY_GET });
    if (serverId == null) return;
    set({ loading: true });
    const fetched = await fetchPrompts(serverId);
    if (get().serverId !== serverId) return;
    if (!fetched.ok) {
      set({ loading: false, loadError: fetched.message });
      return;
    }
    set({ prompts: fetched.prompts, loading: false });
  },

  refresh: async () => {
    const { serverId, selected } = get();
    if (serverId == null) return;
    const fetched = await fetchPrompts(serverId);
    if (get().serverId !== serverId) return;
    if (!fetched.ok) {
      set({ loadError: fetched.message });
      return;
    }
    const survives = selected != null && fetched.prompts.some((p) => p.name === selected);
    set({ prompts: fetched.prompts, loadError: null, selected: survives ? selected : null });
  },

  applyEvent: (event) => {
    if (event.type !== "notification" || event.server_id !== get().serverId) return;
    const method = (event.payload as { method?: unknown } | null)?.method;
    if (method === "notifications/prompts/list_changed") void get().refresh();
  },

  select: (name) => {
    const prompt = get().prompts.find((p) => p.name === name);
    set({ selected: prompt ? name : null, values: {}, ...EMPTY_GET });
  },

  setValue: (name, value) =>
    set({
      values: { ...get().values, [name]: value },
      fieldErrors: Object.fromEntries(
        Object.entries(get().fieldErrors).filter(([key]) => key !== name),
      ),
    }),

  get: async (serverName, timeoutS) => {
    const { serverId, prompts, selected, values, getting } = get();
    if (serverId == null || getting) return;
    const prompt = prompts.find((p) => p.name === selected);
    if (!prompt) return;
    const built = buildArguments(prompt, values);
    if (!built.ok) {
      set({ fieldErrors: built.errors, getError: null });
      return;
    }
    set({ getting: true, ...EMPTY_GET });
    const outcome = await rpc(
      serverId,
      "prompts/get",
      { name: prompt.name, arguments: built.arguments },
      timeoutS,
    );
    useWorkbench.getState().addHistory(serverId, serverName, getBody(prompt.name, built.arguments));
    switch (outcome.kind) {
      case "result":
        set({
          getting: false,
          result: describePromptResult(outcome.result),
          rawResult: pretty(outcome.result),
        });
        break;
      case "error":
        set({ getting: false, getError: `server error ${outcome.code}: ${outcome.message}` });
        break;
      case "transport":
        set({ getting: false, getError: outcome.message });
        break;
    }
  },
}));
