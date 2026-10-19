import { component } from "sigx";

// An island: hydrated on its own (sigx islands). Island modules live in src/islands/ and are
// known by their export name; their props are sent to the browser, so they must be data.
export const Clicker = component<{ start: number; label: string }>((ctx) => {
  const count = ctx.signal(ctx.props.start);
  return () => (
    <p class="clicker">
      <button onClick={() => count.value++}>{ctx.props.label}</button> count:{" "}
      <strong>{count.value}</strong>
    </p>
  );
});
