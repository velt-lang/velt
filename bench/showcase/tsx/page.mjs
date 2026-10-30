// The component tree of _page.vlt for React or Preact, written as the calls their automatic
// JSX runtimes take (`jsx(type, props, key)`: what a JSX compiler emits for this tree), so Node
// runs it without a build step. Used by render.mjs and server.mjs.

export async function load(lib) {
  const { jsx, jsxs } = await import(lib === "react" ? "react/jsx-runtime" : "preact/jsx-runtime");
  const renderToString =
    lib === "react"
      ? (await import("react-dom/server")).renderToString
      : (await import("preact-render-to-string")).renderToString;

  function Row(props) {
    const it = props.item;
    return jsxs("tr", {
      className: it.done ? "row done" : "row",
      children: [
        jsx("td", { children: it.id }),
        jsx("td", { children: it.name }),
        jsx("td", { children: it.note }),
        jsx("td", { children: it.done ? "yes" : "no" }),
      ],
    });
  }

  function Page(props) {
    return jsxs("main", {
      className: "page",
      children: [
        jsx("h1", { children: props.title }),
        jsxs("table", {
          children: [
            jsx("thead", {
              children: jsxs("tr", {
                children: [
                  jsx("th", { children: "id" }),
                  jsx("th", { children: "name" }),
                  jsx("th", { children: "note" }),
                  jsx("th", { children: "done" }),
                ],
              }),
            }),
            jsx("tbody", { children: props.items.map((it) => jsx(Row, { item: it }, it.id)) }),
          ],
        }),
        jsx("footer", { children: props.items.length }),
      ],
    });
  }

  return { render: (title, data) => renderToString(jsx(Page, { title, items: data })) };
}

export class Item {
  constructor(id, name, note, done) {
    this.id = id;
    this.name = name;
    this.note = note;
    this.done = done;
  }
}

export function items(n) {
  const out = [];
  for (let i = 0; i < n; i++) {
    const note = i % 7 === 0 ? `a < b && c ${i}` : `plain note ${i}`;
    out.push(new Item(i, `item ${i}`, note, i % 3 === 0));
  }
  return out;
}
