import { create } from "zustand";
import * as api from "./api";
import { useLogs } from "./logs";
import type {
  AppEvent,
  DiscoveredConfig,
  ImportOutcome,
  NewServer,
  ServerOverview,
  ServerRecord,
} from "./types";

interface PanelState {
  servers: ServerOverview[];
  loaded: boolean;
  /** The last full-list fetch failed — distinct from "no servers exist". */
  loadFailed: boolean;
  error: string | null;
  /** The server whose config the form is editing; null = add mode. */
  editing: ServerOverview | null;
  /** The import dialog is open. */
  importOpen: boolean;
  load: () => Promise<void>;
  resync: () => void;
  /** Mutations resolve truthy on success so callers can keep user input
   * (form fields) instead of discarding it on a failed backend call. */
  add: (server: NewServer) => Promise<ServerRecord | null>;
  update: (record: ServerRecord) => Promise<boolean>;
  setSecret: (id: number, key: string, value: string) => Promise<boolean>;
  deleteSecret: (id: number, key: string) => Promise<boolean>;
  remove: (id: number) => Promise<boolean>;
  toggle: (id: number, run: boolean) => Promise<boolean>;
  setEditing: (server: ServerOverview | null) => void;
  setImportOpen: (open: boolean) => void;
  /** Import reads return null on failure, having set the error banner —
   * same contract as the mutations above. */
  discoverImports: () => Promise<DiscoveredConfig[] | null>;
  readImportFile: (path: string) => Promise<DiscoveredConfig | null>;
  runImport: (path: string, names: string[]) => Promise<ImportOutcome | null>;
  applyEvent: (event: AppEvent) => void;
  clearError: () => void;
}

// Trailing debounce for resync so a burst of lagged markers becomes one
// list_servers round trip.
let resyncTimer: number | undefined;

export const usePanel = create<PanelState>((set, get) => ({
  servers: [],
  loaded: false,
  loadFailed: false,
  error: null,
  editing: null,
  importOpen: false,

  load: async () => {
    try {
      set({ servers: await api.listServers(), loaded: true, loadFailed: false });
    } catch (error) {
      set({ error: api.describeError(error), loaded: true, loadFailed: true });
    }
  },

  add: async (server) => {
    try {
      const record = await api.addServer(server);
      await get().load();
      return record;
    } catch (error) {
      set({ error: api.describeError(error) });
      return null;
    }
  },

  update: async (record) => {
    try {
      await api.updateServer(record);
      await get().load();
      return true;
    } catch (error) {
      set({ error: api.describeError(error) });
      return false;
    }
  },

  setSecret: async (id, key, value) => {
    try {
      await api.setServerSecret(id, key, value);
      await get().load();
      return true;
    } catch (error) {
      set({ error: api.describeError(error) });
      return false;
    }
  },

  deleteSecret: async (id, key) => {
    try {
      await api.deleteServerSecret(id, key);
      await get().load();
      return true;
    } catch (error) {
      set({ error: api.describeError(error) });
      return false;
    }
  },

  remove: async (id) => {
    try {
      await api.removeServer(id);
      // Removal confirmed — retire the log bucket here so every caller
      // (not just the row button) keeps the two stores consistent, and
      // close the edit form if it was showing the deleted server.
      useLogs.getState().drop(id);
      if (get().editing?.id === id) set({ editing: null });
      await get().load();
      return true;
    } catch (error) {
      set({ error: api.describeError(error) });
      return false;
    }
  },

  // Status updates arrive over SSE; on failure we resync the whole list.
  toggle: async (id, run) => {
    try {
      if (run) {
        await api.startServer(id);
      } else {
        await api.stopServer(id);
      }
      return true;
    } catch (error) {
      set({ error: api.describeError(error) });
      await get().load();
      return false;
    }
  },

  resync: () => {
    window.clearTimeout(resyncTimer);
    resyncTimer = window.setTimeout(() => void get().load(), 250);
  },

  applyEvent: (event) => {
    if (event.type === "lagged") {
      // Dropped broadcast events may have included status changes — the
      // patch-by-event model is stale, so refetch the authoritative list.
      get().resync();
      return;
    }
    if (event.type !== "status_changed") return;
    set({
      servers: get().servers.map((server) =>
        server.id === event.server_id ? { ...server, status: event.status } : server,
      ),
    });
  },

  setEditing: (server) => set({ editing: server }),

  setImportOpen: (open) => set({ importOpen: open }),

  discoverImports: async () => {
    try {
      return await api.discoverImports();
    } catch (error) {
      set({ error: api.describeError(error) });
      return null;
    }
  },

  readImportFile: async (path) => {
    try {
      return await api.readImportConfig(path);
    } catch (error) {
      set({ error: api.describeError(error) });
      return null;
    }
  },

  // The caller reports per-entry failures from the outcome; only a failed
  // call itself reaches the error banner.
  runImport: async (path, names) => {
    try {
      const outcome = await api.importServers(path, names);
      await get().load();
      return outcome;
    } catch (error) {
      set({ error: api.describeError(error) });
      return null;
    }
  },

  clearError: () => set({ error: null }),
}));
