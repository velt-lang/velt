# velt/jsx/jsx-runtime

The JSX provider module of [`velt/jsx`](../jsx.md), as the [provider
contract](../../internals/contracts/jsx.md) names it: a module with JSX imports it implicitly, as
the namespace `JSX` for types (`JSX.Element`, `JSX.IntrinsicElements`), and the compiled code
calls its factories (`jsx`, `Fragment`, `jsxComponent`, `jsxAsyncComponent`). It includes the
SSR precompile exports (`jsxTemplate`, `jsxEscape`, `jsxAttr`, `jsxList`, …), so static markup
compiles to constant strings. Programs import from `velt/jsx`, not from here.
