import { component } from "sigx";
import { Counter } from "./Counter";
import { Todos } from "./Todos";
import { Stats } from "./Stats";

export const Home = component<{}>(() => {
  return () => (
    <section>
      <Counter start={1} label="Counter" />
      <Stats />
      <Todos
        items={[
          { title: "Render on Velt", done: true },
          { title: "Hydrate with sigx", done: true },
          { title: "Ship one package", done: false },
        ]}
      />
    </section>
  );
});
