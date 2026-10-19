import type { Signal } from "sigx";

export interface Router {
  readonly path: Signal<string>;
  navigate(href: string): void;
}

/** A router whose current path starts at `path`; `ctx` is the setup context of the app's root. */
export declare function createRouter(ctx: { signal: <T>(v: T) => Signal<T> }, path: string): Router;

export declare const Link: import("sigx").ComponentFactory<
  { router: Router; href: string; label: string },
  void,
  {}
>;
