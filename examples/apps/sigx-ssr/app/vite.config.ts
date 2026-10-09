import { defineConfig } from "vite";
import sigx from "@sigx/vite";
import velt from "@sigx/velt";

// sigx() is the browser side (HMR for components); velt() runs and builds the native server.
export default defineConfig({
  plugins: [sigx(), velt()],
  oxc: { jsx: { runtime: "automatic", importSource: "sigx" } },
});
