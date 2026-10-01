# velt:regex

`import { RegExp } from "velt:regex"`. JavaScript-flavoured regular expressions on Rust's `regex`
engine. Matching is linear-time. Offsets are byte offsets. There is no hidden `lastIndex`:
`exec(s, from)` takes the start offset explicitly.

- `new RegExp(pattern, flags = "")`: flags `g i m s y`. Throws `RegExpError`.
  Fields: `source`, `flags`, `global`, `sticky`, `groupCount`.
- `test(s, from = 0)`, `exec(s, from = 0): RegExpMatch | null`.
- `matchAll(s): RegExpMatch[]`, `matches(s): string[]`.
- `replace(s, repl)`: every match with `g`, otherwise the first. `replaceAll(s, repl)`.
  `repl` expands `` $& $1 $<name> $` $' $$ ``.
- `replaceWith(s, f: (m) => string)`.
- `split(s, limit = 0)`: captured groups are included in the result.
- `RegExp.escape(s)`, `clone()`.
- `RegExpMatch { index; end; value; captures: (string | null)[]; names }`, with `group(n)` and
  `named(name)`.

```ts
import { RegExp } from "velt:regex";

function main() {
  const date = new RegExp("(?<y>\\d{4})-(?<m>\\d{2})-(?<d>\\d{2})", "g");
  const text = "from 2024-02-29 to 2024-03-01";
  const m = date.exec(text);
  if (m != null) {
    console.log(m.index, m.value, m.named("y"), m.group(2)); // 5 2024-02-29 2024 02
  }
  console.log(date.matches(text), date.replace(text, "$<d>/$<m>/$<y>"));
  console.log(new RegExp("\\s*,\\s*").split("a , b,c"), new RegExp("^hi", "i").test("Hi there"));
}
```

Notes: lookaround and backreferences are not supported; compiling a pattern that uses them
throws `RegExpError`. `\d \w \b` match ASCII only, as in JS.
