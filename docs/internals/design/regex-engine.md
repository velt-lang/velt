# The regular expression engine

Status: implemented. Issue: [#3317](https://github.com/velt-lang/velt/issues/3317).

## Summary

`RegExp`, regex literals and the string methods that take a regex (`match`, `matchAll`,
`replace`, `replaceAll`, `search`, `split`) are compiled and matched by Velt code in
`std/text/regex/`. A pattern is parsed with JavaScript's grammar into a tree, compiled into a
small program of instructions, and matched against the subject's own WTF-8 bytes. Matching takes
time linear in the subject whatever the pattern; in exchange lookaround and back references are
refused when the pattern is compiled.

What a program sees is JavaScript's: the match, the captures, named groups, `lastIndex`, the
`i m s u y g` flags, UTF-16 offsets, and V8's `SyntaxError` texts for a pattern that does not
parse.

## Motivation

A regex engine behind runtime calls is opaque to the compiler: a pattern is compiled by code
LLVM cannot see, every `test` crosses a call, and the subject's positions are translated at the
boundary. An engine in std is compiled with the program, so a search is inlined into its caller,
and everything the compiler learns to do well speeds up matching too.

An engine written for another language's regex flavour also matches another flavour. JavaScript
matches UTF-16 code units without the `u` flag, clears a group's captures on every iteration of
the quantifier around it, and ends an iteration that matched nothing. A translation of the
pattern into another flavour approximates these; an engine built for JavaScript has them.

## Design

### Rule: the engine is written in Velt

The parser, the compiler, the matchers, the Unicode tables and `replace` and `split` are Velt
code in `std/text/regex/`, on what every program has (strings, arrays, `velt/simd`). The runtime
contributes what every string operation uses: a string of a byte range and the string builder.

### The text model

A string is WTF-8 (see [JavaScript string semantics](strings.md)), and the matchers read its
bytes where they are: no copy of the subject is made. A position is a byte offset.

- With the `u` (or `v`) flag a character is a code point: one sequence of 1 to 4 bytes. A lone
  surrogate is its 3-byte sequence, so `/\uD83D/u` matches a lone high surrogate and never half
  of a pair.
- Without it a character is a UTF-16 code unit. A pair, which a string stores as one 4-byte
  sequence, is two characters: the high surrogate at the sequence's first byte, one byte long,
  and the low surrogate at its second byte, three bytes long. So the position between the
  halves of a pair is the offset of the sequence's second byte. `/./`, `/[^a]/` and
  `/[\uDC00-\uDFFF]/` match half a pair there, and an empty match can fall between the halves,
  as in JavaScript.

The parser reads the pattern the same way: a supplementary character of a pattern without the
flag is its two surrogates, so `/😀+/` repeats the low one.

With the `u` flag the position between the halves exists for the search only: no character is
read there, but an assertion or an empty pattern can match (`/\B/u` matches inside `"😀"`),
because V8's search tries every code unit.

Offsets a program gives (`exec(s, from)`, `lastIndex`) and gets (`index`, `end`) are code
units. They are translated with the position cache every string method uses
(`std/lang/string/positions.velt`), so a loop of `exec` calls over one string stays linear. An
ASCII subject needs no translation.

### Parsing

`parse_pattern.velt` and `parse_escape.velt` are a recursive-descent parser of ECMAScript's
pattern grammar, with Annex B's leniency without the `u` flag (`\q` is `q`, `x{` is text, `\101`
is octal, `[\d-z]` holds a `-`) and its errors with it. The error texts are V8's (`Unterminated
group`, `Nothing to repeat`, `Invalid capture group name`, …), in a `RegExpError`, which is a
`SyntaxError`.

Three things JavaScript has are refused with an error that names them: lookaround (`(?=…)`,
`(?<!…)`) and back references (`\1`, `\k<name>`), which a matcher whose time is linear cannot
run, and the set syntax of classes under the `v` flag (a nested class, `--`, `&&`, `\q{…}`, a
property of strings such as `\p{RGI_Emoji}`). A pattern that JavaScript does not accept either
gets JavaScript's error, not the refusal: the pattern is read to its end first, so `(?=a` is
`Unterminated group` and `(?=a)\k<n>` is `Invalid named reference`. The `v` flag is otherwise
the `u` flag with its own rules for a class's characters: `( ) { } / | -` are escaped there, the
doubled punctuators (`!!`, `##`, `&&`, …) are errors, and `\&`, `\-`, `\!` and the like stand
for themselves. A `v` class is read by `parse_class_set.velt`, which checks its set syntax as V8
does before refusing it: one kind of operator per class, no range that ends in a class escape
or a nested class, no negated class that may match a string. A `\q{…}` of single characters is
matched (its characters are members); V8 leaves those characters as they are under `i`, where
the engine closes them over case like any other member. A group's name is a JavaScript identifier (`ID_Start`, `ID_Continue`). Groups
nest at most 250 deep.

A class is a `CharSet`: sorted ranges of characters. `\d \w \s` are JavaScript's. `\p{…}` reads
the generated tables of `unicode_tables.velt` (General_Category, Script, Script_Extensions and
the binary properties ECMAScript lists). With the `i` flag a set is closed over case when it is
parsed: every character that canonicalizes like one of its members is added (simple case folding
with `u`, `toUpperCase` of one code unit without). Matching then never folds case: a literal
`k` under `/iu` is the set of `k`, `K` and the Kelvin sign. `\P{…}` under `i` is the complement first and the
closing second with `u`, the other way round with `v`, as ECMAScript says.

The tables are generated by `scripts/gen-regex-tables.js` from Node's own regular expressions,
asked one code point at a time, so they are Node's by construction: the Node that generated
them (22.23, Unicode 17). An older Node knows fewer characters and scripts.

### The program

`compiler.velt` turns the tree into instructions (`program.velt`), Thompson's construction: an
alternation is a split, a quantifier a loop of splits, a counted quantifier copies of its body.
Every instruction that reads text reads one character.

| Instruction | Meaning |
|---|---|
| `CHAR c`, `CLASS set` | one character |
| `SPAN set` | as many characters of `set` as there are: a greedy `[…]*` |
| `SPLIT a b` | go on at `a`; if that leads to no match, at `b` |
| `JUMP a` | go on at `a` |
| `SAVE n` | capture slot `n` is here |
| `CLEAR a b` | slots `a..b` are unset |
| `ASSERT kind` | `^ $ \b \B` hold here |
| `ENTER mark`, `NONEMPTY mark` | an optional iteration starts; it did not end where one could start |
| `MATCH` | the pattern matched |

Three of JavaScript's rules shape the program:

- **Captures do not outlive an iteration.** Each iteration of a quantifier past the first
  starts with a `CLEAR` of the groups in its body, so `/(?:(a)|b)*/` on `"ab"` leaves group 1
  unset.
- **An iteration past the least number matches something.** Where an iteration's body can
  match nothing (`(a*)*`, `(?:a?b??)*`, `(a?){1,2}`), `ENTER` marks where the iteration starts
  and `NONEMPTY` fails at its end when it is where it started. Every such quantifier has a mark
  of its own, so one inside another keeps its own start. A loop whose body always reads a
  character has neither instruction.
- **A choice is made again in every iteration.** A lazy body may match nothing now and
  something in the next iteration (`/(a*?)*/` on `"aa"` matches all of it), so a matcher may
  not conclude from having been at an instruction and a position that being there again leads
  nowhere while it is still inside that first visit. In a program with marks a state counts as
  tried only when everything after it has been (see the matchers).
- **`\b` under `/iu`** takes `ſ` and the Kelvin sign for word characters, which fold to `s` and
  `k`.

A character or a capture also says where the program goes on, so a matcher need not stop at
the jump that closes a loop.

A program has at most 400,000 instructions (`a{50000}` compiles); a pattern that needs more is
`Regular expression too large`.

A program that has no assertion gets two more things when it is compiled (`assemble.velt`):
the classes of characters it cannot tell apart, for the automaton, and the same pattern
compiled back to front, which reads a match from its end to its start. Every split whose two
ways can be told apart by the next ASCII character gets a hint (`hints.velt`): a table of which
way each character can start.

### Matching

Four matchers run the program, and a search gives the match JavaScript's backtracking would:
the leftmost one, and of those that start there the one the pattern prefers.

**The backtracker** (`backtrack.velt`) tries alternatives in order, depth first, with a stack of
pending alternatives and of capture slots to restore. At a split with a hint it looks at the
next character first: a way that cannot start with it is not remembered, so `(a|b)*c` is
matched without going back.

- *With a memory*, for a short subject: a bit per state (join instruction, position). It tries
  no state twice, so its time is bounded by the program's size times the subject's length. Only
  instructions a jump or a split leads to are states; the instructions between two are a
  straight line. In a program without marks a state is remembered when it is entered. In one
  with marks it is remembered when everything after it has been tried, by a frame on the stack
  that says so: a loop whose body matched nothing comes back to a state it is still in, and
  what that state can still do is JavaScript's next choice.
- *Without one*, for a program without marks: the same search, with nothing remembered. It is
  the fast path of every search. It counts its steps against a fuel of 32 per byte of the
  subject left; a search that needs more is one the pattern makes costly, and starts again with
  a matcher whose time is bounded.

**The Pike machine** (`pike.velt`): it reads the subject once, keeping every way the program
could be matching so far as a thread, in the pattern's order of preference. A list holds at
most one thread per instruction, so a step costs at most the program's size. Its closure
follows the same rule for marks as the backtracker's memory.

**The automaton** (`dfa.velt`), for a program without assertions other than `$` (without the
`m` flag): a deterministic automaton built while it runs. A state is the Pike machine's list of threads; the
state a class of characters leads to is worked out the first time it is needed, and after that
a character costs one table lookup, whatever the pattern. It says where the match ends, or that
there is none. The automaton of the reversed program, run backwards from there for the longest
match, says where it starts. An automaton holds at most 512 states; a search that needs more
gives up and goes to the matchers above, and is not kept. The automaton knows nothing of
groups. `$` is a thread that waits: a character ends it, and a state where one leads to a match
holds a match where the subject ends. The reversed program starts past its `$` only when the
match ends with the subject, and the groups of a program with `$` are read with the whole
subject after the match, so that `$` does not hold where the match ends before the subject
does.

**The search memory** (`search.velt`) holds what the matchers work in for one call: the
backtracker's arrays, the Pike machine's (`lists.velt`, made when it first runs) and the
automatons. A `RegExp` keeps what its automatons worked out from call to call, so they are
worked out once, not once per `test` or `exec`: as `DfaTables` (`dfa_tables.velt`), a value that
never changes, in an `Atomic` of the regex's cursor. A call's automaton reads those tables in
place and copies them only when it works out a new state; a call that worked out more puts its
tables there. No lock is held (a `RegExp` holds none, so it converts to `any`, prints and
writes as JSON): calls on several threads read what is there, and the last to finish keeps its
tables. The cursor also counts the calls.

**The reader of groups** (`straight.velt`): when the automaton has found a match and the search
wants its groups, the program is run over the match with the hints alone, no stack and no
fuel. Where a split has no hint, or both ways can start with the next character, it stops, and
the backtracker reads the groups between the two ends the automaton found. It clears every slot
first: an attempt that stopped leaves none behind.

`find.velt` chooses:

1. A pattern that is a few literal strings is found by the scan (below); no matcher runs.
2. A subject without the byte every match has (below) has no match.
3. The automaton first, when the program has one and the search memory has served a call before
   or holds an automaton already, from a subject of 32 bytes on; on a regex's first call, from
   256 bytes on.
4. Otherwise the backtracker without a memory, when the program has no marks. Over a subject of
   256 bytes or more that has an automaton it gets 256 steps only, enough for a match that is
   near and short; then the automaton.
5. The automaton: one pass for `test`, a pass each way for a match's ends, then the reader of
   groups or the backtracker between the ends.
6. Otherwise, or when the automaton gave up: the backtracker with a memory when its table is at
   most 4 KiB, else the Pike machine.

So every search is linear in the subject, and no pattern makes it exponential.

### Where a match may start

`literals.velt` reads from the tree what every match starts with, and `prefilter.velt` searches
for that before any matcher runs.

- **Literals.** When the pattern matches only a few strings and captures nothing (`cat|dog`,
  `gr[ae]y`, at most eight strings), no matcher runs at all: the search finds the strings.
  Otherwise the strings every match starts with are where the matchers are started.
- **Grams.** The strings are found by the bytes they have at up to four fixed offsets: sixteen
  positions a step, compared with SIMD against at most four byte tuples. The offsets are the
  ones whose tuples are fewest and rarest. A hit of a literal pattern is checked by comparing
  eight bytes of the subject, as one word, with each string's.
- **First byte.** Without such strings, the set of bytes a match can start with.
- A pattern that starts with `^` (without `m`) is tried at the start only.
- **A byte every match has** (`required.velt`): of the ASCII bytes every match contains, the one
  least likely in text (`;` in `(\w+)=(\d+);`). A subject without it from the search's start
  on has no match; one byte scan per subject says so, and where it was found is kept for the
  next search of the same call.
- The automaton's scan from its start state is given up, for that automaton, when it skips
  fewer than 8 bytes a scan: reading those bytes costs less.

`matchAll`, `match` with `g` and `replace` with `g` find every match in one call (`findAll`), so
a literal pattern scans the subject once, and `replace` appends the text between two matches to
one buffer from the subject's own bytes.

### `lastIndex`

`lastIndex` is an `Atomic<f64>` in an object of the regex's own, so `exec` and `test` take a
receiver they may not change, like every other method. A regex copied to another task has its
own.

## Runtime cost

Instructions retired (cachegrind, x86-64) of the programs that guard the engine, against the
same programs matched by Rust's `regex` crate, which the engine replaced:

| Program | `regex` crate | std engine |
|---|---|---|
| `bench/benchmarks-game/regex-redux` on `fasta 100000` | 194.0 M | 189.2 M |
| `bench/regex/literal.velt` (compiles three patterns per iteration) | 88.6 G | 17.9 G |
| `bench/regex/units.velt` (code-unit patterns over pairs) | 13.7 G | 0.65 G |
| `long.velt` 0: `/(a+)+b/` fails | 3.67 M | 3.48 M |
| `long.velt` 1: `/(a+)+b/` matches | 18.12 M | 6.40 M |
| `long.velt` 2: `/.*.*.*=.*/` fails | 4.03 M | 2.85 M |
| `long.velt` 3: `/.*.*.*=.*/` matches | 13.92 M | 7.92 M |
| `long.velt` 4: `/([^,]*,)*x/` fails | 3.88 M | 3.54 M |
| `long.velt` 5: `/([^,]*,)*x/` matches | 26.29 M | 20.89 M |
| `long.velt` 6: `/(a\|b)*c/` fails | 3.67 M | 3.48 M |
| `long.velt` 7: `/(a\|b)*c/` matches | 12.79 M | 11.89 M |
| `lines.velt` 0: `/(\w+)=(\d+);/.test`, 320 B | 27.69 M | 29.56 M |
| `lines.velt` 1: `/(\w+), $/.exec`, 320 B | 33.39 M | 42.66 M |
| `lines.velt` 2: `/([^,]*,)*#/.test`, 320 B | 32.88 M | 29.52 M |
| `lines.velt` 3: `/value\d+;/i.test`, 320 B | 55.38 M | 29.49 M |
| `lines.velt` 4: as 0, 250 B | 22.51 M | 24.11 M |
| `lines.velt` 5: as 1, 250 B | 28.12 M | 36.26 M |
| `lines.velt` 6: as 2, 250 B | 26.69 M | 24.06 M |
| `lines.velt` 7: as 3, 250 B | 44.01 M | 24.04 M |

Compiling a pattern is the larger difference in the first three: it allocates a few small
arrays, where an automaton library builds tables. A pattern that is no literal is also compiled
back to front and gets its hints and classes then, about half of its compile time.
`bench/regex/long.velt` has patterns that are no literal over a subject of 100,000 characters,
each failing (`test`) and succeeding (`exec`); `bench/regex/lines.velt` the same kind of
patterns searched line by line, 2,000 lines of 320 or 250 bytes. Each row's number includes
starting the program and building its subject: about 2.7 M for `long`, 26.6 M and 21.4 M for
`lines`, so a `lines` row's matching is its number less that.

Four `lines` rows are above the old engine, which kept no state between calls either but whose
calls cost little. Rows 0 and 4 (+7%: one byte scan finds no `;`) spend about 900 instructions
a call more than it, making the call's search memory and reading the regex's tables. Rows 1 and 5 (+28%,
`exec` of a pattern that ends in `$`, every line a match) spend about 4,600 instructions a line
more than its 3,400: the automaton reads every byte of the line to find where the match ends
(about 3,200, 10 a byte), then the pass back and the groups take about 1,300, and making two
automatons over the kept tables and the match the rest.

The Pike machine is the slow matcher, tens of instructions per byte and thread. It runs when
the subject is too long for the backtracker's memory and the program has no automaton (it has
an assertion) or the automaton gave up, after the backtracker without a memory spent its fuel
or for a program it cannot run.

## Alternatives considered

**Backtracking alone, as V8 does.** It supports lookaround and back references, and it is the
simplest engine that is JavaScript's to the letter. Its time is exponential in the subject for
patterns such as `(a*)*b`, which a server that matches text it was sent cannot afford.

**No automaton.** The first version had the backtrackers and the Pike machine only. Patterns
with nested or adjacent loops over a long subject (`(a+)+b`, `.*.*=.*`) spent their fuel and
ran in the Pike machine, several times slower than the engine this one replaced. The automaton
costs a second compilation of such a pattern and two more files, and is why those rows are
below the old engine's.

**Translating the pattern for another engine.** The translation is where the differences from
JavaScript are: code units, captures in loops, empty iterations, the error texts.
