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
      "/metrics": backend,
      // the files the server answers itself under /-/ (crates/forgust-server/src/web.rs):
      // raw files, uploads, release downloads, images from other sites. The app's own
      // pages under /-/ (issues, merge requests…) stay with Vite.
      "^/-/(project|image)/": backend,
      "^/[^?]+/-/(raw|uploads)/": backend,
      "^/[^?]+/-/releases/[^/?]+/downloads/": backend,
    },
  },
  build: {
    target: "es2022",
    sourcemap: false,
    chunkSizeWarningLimit: 200,
  },
});
