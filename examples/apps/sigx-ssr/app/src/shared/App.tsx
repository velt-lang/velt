import { component } from "sigx";
import { createRouter, Link } from "@sigx/velt/router";
import { Home } from "./Home";
import { About } from "./About";

export const App = component<{ path: string }>((ctx) => {
  const router = createRouter(ctx, ctx.props.path);
  return () => (
    <main>
      <h1>sigx on Velt</h1>
      <nav>
        <Link router={router} href="/" label="Home" />
        <Link router={router} href="/about" label="About" />
      </nav>
      <p class="path">Rendered for {ctx.props.path}</p>
      {router.path.value === "/about" ? <About /> : <Home />}
    </main>
  );
});
