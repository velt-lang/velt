import { component } from "sigx";
import { Clicker } from "../islands/Clicker";

// A page that is static HTML except for its islands: the browser loads and hydrates only the
// `Clicker`s, each when its directive says.
export const IslandsPage = component<{}>(() => {
  return () => (
    <main>
      <h1>Islands</h1>
      <p>This text is static: no JavaScript runs for it.</p>
      <Clicker client:load start={1} label="load" />
      <Clicker client:visible start={5} label="visible" />
      <Clicker client:only start={9} label="only" />
      <p>
        <a href="/">Back</a>
      </p>
    </main>
  );
});
