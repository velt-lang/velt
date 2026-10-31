# velt/jsx/attrs

`import { HtmlAttrs, AnchorAttrs, SvgAttrs, MathAttrs, … } from "velt/jsx/attrs"`. The attribute
types [`velt/jsx`](../jsx.md)'s `IntrinsicElements` is made of, for JSX providers that extend
them ([Extending `IntrinsicElements`](../../internals/contracts/jsx.md)). `HtmlAttrs` holds the
global attributes, and every other HTML type is `HtmlAttrs & { … }` with the element's own
(`AnchorAttrs` for `<a>`, `InputAttrs` for `<input>`, …); the SVG and MathML types are built the
same way on `SvgAttrs` and `MathAttrs`.
