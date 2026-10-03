// A stand-in JSX provider, so the oracle checks JSX offline: the automatic runtime's factories
// and a `JSX` namespace that accepts any element and attribute. The fixtures name `JSX.Element`
// without importing it, as Velt's do, so the namespace is global too.

type Node = { readonly tag: unknown };

declare global {
  namespace JSX {
    type Element = Node;
    interface IntrinsicElements {
      [tag: string]: Record<string, unknown>;
    }
    interface ElementChildrenAttribute {
      children: {};
    }
  }
}

export namespace JSX {
  type Element = globalThis.JSX.Element;
  type IntrinsicElements = globalThis.JSX.IntrinsicElements;
  type ElementChildrenAttribute = globalThis.JSX.ElementChildrenAttribute;
}

export function jsx(type: unknown, props: unknown, key?: unknown): JSX.Element;
export function jsxs(type: unknown, props: unknown, key?: unknown): JSX.Element;
export const Fragment: unique symbol;
