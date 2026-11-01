import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: { ignored: ["**/src-tauri/**"] },
    // fixtures/ lives outside the app root and is shared with Flutter/Kotlin.
    fs: { allow: [".."] },
  },
  build: { target: "es2022", sourcemap: false },
  test: { environment: "node", include: ["tests/**/*.test.ts"] },
});
