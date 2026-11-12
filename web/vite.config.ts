import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// In dev, the API and git endpoints are served by `forgust serve`.
const backend = process.env.FORGUST_URL ?? "http://localhost:8080";

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/api": backend,
      "/health": backend,
    },
  },
  build: {
    target: "es2022",
    sourcemap: false,
    chunkSizeWarningLimit: 200,
  },
});
