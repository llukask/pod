import { defineConfig } from "vitest/config";
import solid from "vite-plugin-solid";

// `npm run dev` serves the app on 8001 and forwards the API to a pod-server on
// 3000 (`cargo run -p pod-server`), so the browser sees one origin, as it does
// when pod-server serves the built bundle itself (`web_dir`).
export default defineConfig({
  plugins: [solid()],
  server: {
    host: "127.0.0.1",
    port: 8001,
    strictPort: true,
    proxy: { "/v1": "http://127.0.0.1:3000" },
  },
  build: { target: "es2022" },
  // Date labels are local time; the tests pin it.
  test: { environment: "jsdom", env: { TZ: "UTC" } },
});
