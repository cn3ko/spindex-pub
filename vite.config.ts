import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    watch: {
      // native/build dirs; .vcpkg holds locked files that crash the watcher
      ignored: ["**/analyzer/**", "**/src-tauri/**"],
    },
  },
});
