import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri conventions: fixed dev port, no screen clearing so cargo/tauri
// output stays visible.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // Never watch the Rust build output: target/ holds hundreds of
      // thousands of files and blows the inotify watcher limit (ENOSPC).
      ignored: ["**/src-tauri/**"],
    },
  },
});
