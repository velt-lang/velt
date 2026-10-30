# velt/text

`import { RegExp, StringBuilder, escapeHtml } from "velt/text"`. Text processing:

- [Regular expressions](text/regex.md): `RegExp` (also a global, as are regex literals), its
  matches (`RegExpMatch`), `RegExpError`, and the `Pattern` interface `RegExp` implements.
- [`StringBuilder`](text/string_builder.md) (also a global): a string built by appending,
  returned without a copy.
- [HTML escaping](text/html.md): `escapeHtml(s)`, for element content and quoted attribute
  values.
