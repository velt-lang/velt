import { component } from "sigx";
import { useData } from "@sigx/velt/data";
import { getStats } from "../api.server";
import type { Stats as StatsData } from "../api.server";

// Data from a server function: on the server it loads while the page streams (the shell shows
// "Loading…" first); in the browser it is restored from the page, not fetched again.
export const Stats = component<{}>((ctx) => {
  const stats = useData(ctx, "stats", () => getStats());
  return () => (
    <div class="card">
      <h2>Server data</h2>
      {stats.match({
        pending: () => <p class="stats">Loading…</p>,
        ready: (s: StatsData) => (
          <p class="stats">
            Rendered by {s.renderer}, {s.components} components
          </p>
        ),
      })}
    </div>
  );
});
