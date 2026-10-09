import { component } from "sigx";
import { Counter } from "./Counter";
import { Todos } from "./Todos";

export const App = component<{ path: string }>((ctx) => {
  return () => (
    <main>
      <h1>sigx on Velt</h1>
      <p class="path">Rendered for {ctx.props.path}</p>
      <Counter start={1} label="Counter" />
      <Todos
        items={[
          { title: "Render on Velt", done: true },
          { title: "Hydrate with sigx", done: true },
          { title: "Ship one package", done: false },
        ]}
      />
    </main>
  );
});
