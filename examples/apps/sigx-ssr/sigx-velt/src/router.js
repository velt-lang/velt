// @sigx/velt/router in the browser: the same API as velt/src/router.vlt, so shared components
// route with either. Plain JavaScript (`jsx` calls) so the package needs no build step.
import { component } from "sigx";
import { jsx } from "sigx/jsx-runtime";

/** The router of an app: `path` is a signal, `navigate` changes it and the URL. */
export function createRouter(ctx, path) {
  const current = ctx.signal(path);
  const router = {
    path: current,
    navigate(href) {
      if (href === current.value) return;
      window.history.pushState({}, "", href);
      current.value = href;
    },
  };
  // Also runs under Node (JavaScript sigx rendering the same components on the server).
  if (typeof window !== "undefined") {
    window.addEventListener("popstate", () => {
      current.value = window.location.pathname;
    });
  }
  return router;
}

export const Link = component((ctx) => {
  return () =>
    jsx("a", {
      href: ctx.props.href,
      class: ctx.props.router.path.value === ctx.props.href ? "active" : "",
      onClick: (e) => {
        // New tab, new window, download: the browser's.
        if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
        e.preventDefault();
        ctx.props.router.navigate(ctx.props.href);
      },
      children: ctx.props.label,
    });
});
