import { component } from "sigx";

export const About = component<{}>(() => {
  return () => (
    <section class="card">
      <h2>About</h2>
      <p>Server-rendered by a native Velt binary, hydrated by sigx.</p>
    </section>
  );
});
