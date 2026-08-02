import { create } from "zustand";
import * as api from "./api";
import type { AppEvent, NewServer, ServerOverview } from "./types";

interface PanelState {
  servers: ServerOverview[];
  loaded: boolean;
  error: string | null;
  load: () => Promise<void>;
  add: (server: NewServer) => Promise<void>;
  remove: (id: number) => Promise<void>;
  toggle: (id: number, run: boolean) => Promise<void>;
  applyEvent: (event: AppEvent) => void;
  clearError: () => void;
}

export const usePanel = create<PanelState>((set, get) => ({
  servers: [],
  loaded: false,
  error: null,

  load: async () => {
    try {
      set({ servers: await api.listServers(), loaded: true });
    } catch (error) {
      set({ error: api.describeError(error), loaded: true });
    }
  },

  add: async (server) => {
    try {
      await api.addServer(server);
      await get().load();
    } catch (error) {
      set({ error: api.describeError(error) });
    }
  },

  remove: async (id) => {
    try {
      await api.removeServer(id);
      await get().load();
    } catch (error) {
      set({ error: api.describeError(error) });
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
    } catch (error) {
      set({ error: api.describeError(error) });
      await get().load();
    }
  },

  applyEvent: (event) => {
    if (event.type !== "status_changed") return;
    set({
      servers: get().servers.map((server) =>
        server.id === event.server_id ? { ...server, status: event.status } : server,
      ),
    });
  },

  clearError: () => set({ error: null }),
}));
