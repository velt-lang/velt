import { component } from "sigx";

export const Counter = component<{ start: number; label: string }>((ctx) => {
  const count = ctx.signal(ctx.props.start);
  return () => (
    <div class="card">
      <h2>{ctx.props.label}</h2>
      <p>
        Count: {count.value} (doubled {count.value * 2})
      </p>
      <button id="inc" onClick={() => count.value++}>
        +1
      </button>
    </div>
  );
});
