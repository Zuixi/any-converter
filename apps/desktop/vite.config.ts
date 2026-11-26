import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const gatewayTarget = process.env.ANY_CONVERTER_API_TARGET || "http://127.0.0.1:3028";
const devPort = Number(process.env.ANY_CONVERTER_UI_PORT || 8420);

export default defineConfig({
  plugins: [react()],
  server: {
    host: "127.0.0.1",
    port: devPort,
    strictPort: true,
    proxy: {
      // Preserve the browser's Host header (changeOrigin: false) so gateway
      // loopback Origin/Host checks and port-scoped session cookies match.
      "/v1": {
        target: gatewayTarget,
        changeOrigin: false,
      },
      "/health": {
        target: gatewayTarget,
        changeOrigin: false,
      },
    },
  },
  clearScreen: false,
});
