# velt/jsx/generic/jsx-runtime

[`velt/jsx`](../../jsx.md)'s provider without the SSR precompile exports, so the compiler lowers
every element to `jsx` / `Fragment` calls. It is there to check that both lowerings render
byte-identical HTML: `/** @jsxImportSource velt/jsx/generic */` at the top of a file selects it.
