import { component } from "sigx";

type Todo = { title: string; done: boolean };

export const Todos = component<{ items: Todo[] }>((ctx) => {
  const remaining = ctx.signal<number>(ctx.props.items.filter((t) => !t.done).length);
  return () => (
    <div class="card">
      <h2>Todos</h2>
      <ul>
        {ctx.props.items.map((t) => (
          <li class={t.done ? "done" : "open"}>{t.title}</li>
        ))}
      </ul>
      <p>{remaining.value} left</p>
    </div>
  );
});
