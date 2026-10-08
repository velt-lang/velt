// The blog's JSX provider for TypeScript (`tsc`, Node, a browser bundle): the automatic runtime
// (`jsx`, `jsxs`, `Fragment`) rendering HTML strings with std/jsx's rules, so the shared
// components (src/shared) produce the same markup in both. tsconfig.json maps the provider to
// this file; the server uses jsx/jsx-runtime.vlt.

export class Element {
  constructor(readonly html: string) {}
}

type Child = Element | Child[] | string | number | boolean | null | undefined;
type Props = { [name: string]: unknown; children?: Child };
type Component = (props: Props) => Element;

declare global {
  namespace JSX {
    type Element = import("./jsx-runtime").Element;
    interface IntrinsicElements {
      [tag: string]: Record<string, unknown>;
    }
    interface ElementChildrenAttribute {
      children: {};
    }
  }
}

const VOID = new Set([
  "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr",
]);

// `& < > " '` as entities, in text and attribute values, like std/jsx and react-dom (`'`:
// `&#x27;`).
function escape(s: string): string {
  return s
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#x27;");
}

function render(c: Child): string {
  if (c === null || c === undefined || typeof c === "boolean") {
    return "";
  }
  if (c instanceof Element) {
    return c.html;
  }
  if (Array.isArray(c)) {
    return c.map(render).join("");
  }
  return escape(String(c));
}

export function jsx(type: string | Component, props: Props, _key?: string): Element {
  if (typeof type !== "string") {
    return type(props);
  }
  let open = `<${type}`;
  for (const [name, value] of Object.entries(props)) {
    if (name === "children" || value === null || value === undefined || value === false) {
      continue;
    }
    open += value === true ? ` ${name}` : ` ${name}="${escape(String(value))}"`;
  }
  open += ">";
  if (VOID.has(type)) {
    return new Element(open);
  }
  return new Element(`${open}${render(props.children)}</${type}>`);
}

export const jsxs = jsx;

export function Fragment(props: Props): Element {
  return new Element(render(props.children));
}
