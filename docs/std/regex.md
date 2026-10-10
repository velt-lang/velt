# velt:regex

`RegExp` is a global, as in Node, and so are regex literals (`/ab+c/gi`); `import { RegExp } from
"velt:regex"` also works. JavaScript-flavoured regular expressions on Rust's `regex`
engine. Matching is linear-time. Offsets (`index`, `end`, `from`) are UTF-16 code units, like
every string position, so `s.slice(m.index, m.end)` is the match. `exec(s)` and `test(s)` of a
regex with the `g` or `y` flag start at `lastIndex` and update it, as in JS; `exec(s, from)`
takes the start offset explicitly and leaves `lastIndex` alone. A lone surrogate is not matched
by `.` or a negated class yet (#377 phase 5), and an empty match steps over a whole surrogate
pair (JavaScript without the `u` flag stops between its halves; #401).

- `new RegExp(pattern, flags = "")`: flags `g i m s y` (`d u v` are accepted and change
  nothing). Throws `RegExpError` for an invalid pattern, or an unknown or repeated flag.
  Fields: `source`, `flags`, `global`, `sticky`, `groupCount`, `lastIndex` (a `number`, as in
  JS).
- `test(s, from?)`, `exec(s, from?): RegExpMatch | null`.
- `matchAll(s): RegExpMatch[]`, `matches(s): string[]`: every match from 0, whatever the flags.
- `replace(s, repl)`: every match with `g`, otherwise the first; with `y` only at `lastIndex`
  (JS's `s.replace(re, repl)`). `replaceAll(s, repl)`: every match, whatever the flags.
  `repl` expands `` $& $1 $<name> $` $' $$ ``.
- `replaceWith(s, f: (m) => string)`: the matches `replace` replaces.
- `split(s, limit = 0)`: captured groups are included in the result (`""` for a group that did
  not take part, where JS gives `undefined`); `limit` 0 means no limit.
- `RegExp.escape(s)`, `clone()` (compiles the pattern again; it never throws, so a value
  holding a `RegExp` can be copied for another task, `spawn` or a channel).
- `RegExpMatch { index; end; value; captures: (string | null)[]; names }`, with `group(n)`,
  `named(name)` and `substitute(s, repl)` (`repl` expanded for the match). `m[n]` is group `n`
  (`m[0]` the match), `""` for a group that did not take part.

## String methods with a regex

A string's `replace`, `replaceAll`, `match`, `matchAll`, `search` and `split` take a `RegExp`
(a regex literal or a variable) as in JS, `lastIndex` included:

| Call | Result |
|---|---|
| `s.replace(re, repl)` | as `re.replace(s, repl)` |
| `s.replace(re, (match, p1, …, offset, s) => …)` | each replaced match is the function's result |
| `s.replaceAll(re, …)` | the same; `re` must have the `g` flag |
| `s.match(re)` | `string[] \| null`: with `g`, the text of every match (`null` if none); otherwise the match and its groups |
| `s.matchAll(re)` | `RegExpMatch[]`, every match from `lastIndex` on; `re` must have the `g` flag |
| `s.search(re)` | where the first match starts, or -1 |
| `s.split(re, limit?)` | the pieces between matches, with the groups between them |

A replacer function gets the match, then each group, then the match's offset (a `number`) and
the string, as in JS, and may take fewer parameters. A group that did not take part is `""`
(JS: `undefined`), `null` for a parameter declared optional (`(m, p1?: string) => …`), or the
parameter's default (`(m, p1 = "none") => …`); the parameters are variables the function may
assign. The offset and the string follow the groups, so the number of groups must be known when
compiling: the regex is a literal, or a `const` or `readonly` field initialized with one (or with
`new RegExp` of string literals). For another regex (a parameter, a `let`), a replacer taking
more than the match is a compile error; take the match only, or use the regex's `exec`. For
`matchAll` and `replaceAll`, such a regex without `g` is a compile error, and another regex
without it panics like JS's `TypeError`.

```ts
function main() {
  console.log("banana".replace(/a/g, "o"), "a-b_c".split(/[-_]/)); // bonono [ 'a', 'b', 'c' ]
  const title = "hello world".replace(/\b(\w)(\w*)/g, (_m, first: string, rest: string) =>
    first.toUpperCase() + rest,
  );
  console.log(title, "x1y22".match(/\d+/g)); // Hello World [ '1', '22' ]
  for (const m of "a1b2".matchAll(/([a-z])(\d)/g)) {
    console.log(m[1], m[2], m.index); // a 1 0, then b 2 2
  }
  const date = "2024-05-06".match(/(\d+)-(\d+)/);
  console.log(date?.[1], "hello".search(/l+/)); // 2024 2
}
```

Unlike JS, the array of a match without `g` (`s.match(/(a)/)`) has no `index`, `input` or
`groups` properties (it prints as a plain array; use the regex's `exec` for the position), and
the replacer function doesn't get the named-groups object.

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
throws `RegExpError`. `\d \w \b` match ASCII only, as in JS. Without `s`, `.` matches no line
terminator (`\n`, `\r`, U+2028, U+2029), as in JS. With `m`, `^` and `$` match at `\n` and `\r`
but, unlike JS, not at U+2028 or U+2029: `new RegExp("^", "gm").replace("a\u2028b", ">")` gives
`">a\u2028b"` where JS gives `">a\u2028>b"`.
