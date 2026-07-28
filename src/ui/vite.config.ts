import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath, URL } from "node:url";

// During `vite dev`, proxy the API to a locally running smalog
// (`service.listen`). In production the built assets and the API are
// expected to be served from the same origin, so requests are relative.
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  server: {
    proxy: {
      "/api": "http://localhost:8080",
    },
  },
});
