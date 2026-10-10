import { defineConfig } from "vite";
import sigx from "@sigx/vite";
import velt from "@sigx/velt";
import { sigxIslands } from "@sigx/vite/islands";

// sigx() is the browser side (HMR for components), sigxIslands() the island modules in
// src/islands/; velt() runs and builds the native server.
export default defineConfig({
  plugins: [sigx(), sigxIslands(), velt()],
  oxc: { jsx: { runtime: "automatic", importSource: "sigx" } },
  // One sigx for the app and @sigx/velt's browser modules (here linked from ../sigx-velt).
  resolve: { dedupe: ["sigx", "@sigx/server-renderer"] },
});
