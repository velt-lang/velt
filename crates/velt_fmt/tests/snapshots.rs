//! Snapshot tests for layouts that are easy to get wrong: long call chains, hugged callbacks and
//! literals, nested arrows, `switch`, union type aliases, class bodies, template literals, comments everywhere.
//! Every case is also checked for idempotency, AST preservation and comment preservation.

mod common;

use velt_fmt::format_source;

#[track_caller]
fn assert_fmt(input: &str, expected: &str) {
    let got = format_source(input).unwrap_or_else(|d| panic!("refused: {d:?}"));
    assert_eq!(
        got, expected,
        "\n--- got ---\n{got}\n--- expected ---\n{expected}"
    );
    if let Err(msg) = common::check(input) {
        panic!("{msg}");
    }
}

#[test]
fn long_member_chain_breaks_one_link_per_line() {
    assert_fmt(
        "const names = users.filter((u) => u.active && u.age > 18).map((u) => u.name.trim()).join(\", \");\n",
        "const names = users
  .filter((u) => u.active && u.age > 18)
  .map((u) => u.name.trim())
  .join(\", \");
",
    );
}

#[test]
fn short_chain_stays_on_one_line() {
    assert_fmt(
        "function f() { return this.items.filter((x) => x.ok).length; }",
        "function f() {\n  return this.items.filter((x) => x.ok).length;\n}\n",
    );
}

/// Formats like [`assert_fmt`] and checks that the output parses without any diagnostic (a
/// line starting with `[` gets "this line continues the expression on the line before").
#[track_caller]
fn assert_fmt_quiet(input: &str, expected: &str) {
    assert_fmt(input, expected);
    let (_, diags) = velt_syntax::parse_file(velt_common::FileId(0), expected);
    assert!(diags.is_empty(), "{diags:?}\n{expected}");
}

#[test]
fn index_stays_on_the_call_it_follows_in_a_broken_chain() {
    // #1875: `[0]` used to start the line after `.split(" ")`.
    assert_fmt_quiet(
        "const counts = stderr.slice(at + 8).split(\" \")[0].split(\"/\").filter((x) => x != \"\").map((x) => x.trim());\n",
        "const counts = stderr
  .slice(at + 8)
  .split(\" \")[0]
  .split(\"/\")
  .filter((x) => x != \"\")
  .map((x) => x.trim());
",
    );
}

#[test]
fn indexes_and_non_null_stay_on_their_link() {
    // Chained, optional and computed indexes and `!` after a call, and an index ending the
    // chain. Prettier moves a computed index to the next line (`[idx].split("/")`); Velt keeps
    // it on the call so that no line starts with `[`.
    assert_fmt_quiet(
        "const a = s.slice(at + 8).split(\" \")[0][1].split(\"/\")[idx].filter((x) => x != \"\").map((x) => x.trim())[2];
const b = s?.slice(at + 8)?.split(\" \")?.[0]?.split(\"/\").filter((x) => x != \"\").map((x) => x.trim());
const c = s.slice(at + 8).splitSomething(\" \")[0]!.splitSomething(\"/\")!.filter((x) => x != \"\").map((x) => x.trim());
",
        "const a = s
  .slice(at + 8)
  .split(\" \")[0][1]
  .split(\"/\")[idx]
  .filter((x) => x != \"\")
  .map((x) => x.trim())[2];
const b = s
  ?.slice(at + 8)
  ?.split(\" \")?.[0]
  ?.split(\"/\")
  .filter((x) => x != \"\")
  .map((x) => x.trim());
const c = s
  .slice(at + 8)
  .splitSomething(\" \")[0]!
  .splitSomething(\"/\")!
  .filter((x) => x != \"\")
  .map((x) => x.trim());
",
    );
}

#[test]
fn chain_starting_with_an_index_keeps_it_on_the_head() {
    assert_fmt_quiet(
        "const a = items[0].filterSomethingLong((x) => x.okay && x.size > 10).mapSomethingLong((x) => x.name).join(\", \");
const b = handlers[key](\"foo\").thenSomethingLong((x) => x.okay && x.size > 10).catchSomething((e) => e.message);
const c = foo()[key].alpha().betaSomethingLong((x) => x + 1).gammaSomethingLong((x) => x + 2222);
",
        "const a = items[0]
  .filterSomethingLong((x) => x.okay && x.size > 10)
  .mapSomethingLong((x) => x.name)
  .join(\", \");
const b = handlers[key](\"foo\")
  .thenSomethingLong((x) => x.okay && x.size > 10)
  .catchSomething((e) => e.message);
const c = foo()[key].alpha()
  .betaSomethingLong((x) => x + 1)
  .gammaSomethingLong((x) => x + 2222);
",
    );
}

#[test]
fn trailing_callback_is_hugged() {
    assert_fmt(
        "function f() { xs.forEach((x) => { total += x; }); }",
        "function f() {
  xs.forEach((x) => {
    total += x;
  });
}
",
    );
}

#[test]
fn trailing_object_is_hugged_when_too_long() {
    assert_fmt(
        "function f() { configure(\"server\", { host: \"localhost\", port: 8080, secure: true, retries: 3, timeout: 5000, verbose: false }); }",
        "function f() {
  configure(\"server\", {
    host: \"localhost\",
    port: 8080,
    secure: true,
    retries: 3,
    timeout: 5000,
    verbose: false,
  });
}
",
    );
}

#[test]
fn long_arguments_one_per_line_with_trailing_comma() {
    assert_fmt(
        "const deep = outer(inner(innermost(argumentNumberOne, argumentNumberTwo), secondArgument), lastArgumentHere);",
        "const deep = outer(
  inner(innermost(argumentNumberOne, argumentNumberTwo), secondArgument),
  lastArgumentHere,
);
",
    );
}

#[test]
fn nested_arrows() {
    assert_fmt(
        "const add = (a: i64) => (b: i64) => (c: i64): i64 => a + b + c;\nconst run = () => { return (x) => { return x; }; };",
        "const add = (a: i64) => (b: i64) => (c: i64): i64 => a + b + c;
const run = () => {
  return (x) => {
    return x;
  };
};
",
    );
}

#[test]
fn long_binary_breaks_after_operators() {
    assert_fmt(
        "function f() { if (someCondition && anotherCondition || yetAnotherVeryLongConditionName && theFinalConditionHereTooNow) { go(); } }",
        "function f() {
  if (
    someCondition && anotherCondition ||
    yetAnotherVeryLongConditionName && theFinalConditionHereTooNow
  ) {
    go();
  }
}
",
    );
}

#[test]
fn switch_cases_and_bodies_indent() {
    assert_fmt(
        "function f(s: Shape) { switch (s.kind) { case \"circle\": case \"dot\": return 3.14 * s.r * s.r; case \"rect\": { log(\"x\"); break; } default: log(s.kind); } }",
        "function f(s: Shape) {
  switch (s.kind) {
    case \"circle\":
    case \"dot\":
      return 3.14 * s.r * s.r;
    case \"rect\": {
      log(\"x\");
      break;
    }
    default:
      log(s.kind);
  }
}
",
    );
}

#[test]
fn long_union_aliases_get_one_member_per_line() {
    assert_fmt(
        "type Shape = { kind: \"circle\"; radius: f64 } | { kind: \"rectangle\"; width: f64; height: f64 } | { kind: \"empty\" };
type Dir = \"up\" | \"down\";",
        "type Shape =
  | { kind: \"circle\"; radius: f64 }
  | { kind: \"rectangle\"; width: f64; height: f64 }
  | { kind: \"empty\" };
type Dir = \"up\" | \"down\";
",
    );
}

#[test]
fn class_body_in_source_order() {
    assert_fmt(
        "class Dog extends Animal implements Named {\n  // the dog\n  tricks: i64 = 2; // count\n\n  constructor(name: string) { super(name); }\n  override speak(): string { return `${this.name} barks`; }\n  static create(): Dog { return new Dog(\"rex\"); }\n}",
        "class Dog extends Animal implements Named {
  // the dog
  tricks: i64 = 2; // count

  constructor(name: string) {
    super(name);
  }
  override speak(): string {
    return `${this.name} barks`;
  }
  static create(): Dog {
    return new Dog(\"rex\");
  }
}
",
    );
}

#[test]
fn template_literals_are_verbatim() {
    assert_fmt(
        "const t = `a ${ x+1 }   b\n   c ${ `inner ${ y } // not a comment` }`;",
        "const t = `a ${ x+1 }   b\n   c ${ `inner ${ y } // not a comment` }`;\n",
    );
}

#[test]
fn comments_everywhere() {
    assert_fmt(
        "// file header\n\nimport {a} from './a'; // why a\n/** doc */\nfunction f(/* none */) { // opens\n  foo(a, // first\n    b);\n\n\n  // before return\n  return; /* done */\n  // dangling end\n}\n// trailer\n",
        "// file header

import { a } from \"./a\"; // why a

/** doc */
function f(/* none */) {
  // opens
  foo(
    a, // first
    b,
  );

  // before return
  return; /* done */
  // dangling end
}
// trailer
",
    );
}

#[test]
fn module_forms() {
    assert_fmt(
        "import * as geo from './geo';\nimport type {Shape,Size as S} from \"./types\";\nimport {type Id, make} from \"./ids\";\nexport {a, b as c} from \"./m\";\nexport * from './all';\nexport type {T} from \"./t\";\nexport {x, y as z};\nconst x = 1;\n",
        "import * as geo from \"./geo\";
import type { Shape, Size as S } from \"./types\";
import { type Id, make } from \"./ids\";
export { a, b as c } from \"./m\";
export * from \"./all\";
export type { T } from \"./t\";
export { x, y as z };

const x = 1;
",
    );
}

#[test]
fn inline_block_comments_stay_with_their_neighbour() {
    assert_fmt(
        "const c = call(a, /* mid */ b, c /* end */);\nconst e = {/* empty */};\nconst l = [ // lonely\n];",
        "const c = call(a, /* mid */ b, c /* end */);
const e = { /* empty */ };
const l = [
  // lonely
];
",
    );
}

#[test]
fn quotes_numbers_and_operators_keep_their_meaning() {
    assert_fmt(
        "const s = 'it\\'s' + 'say \"hi\"';\nconst n = 0xff+1_000*1.5e3;\nconst eq = a===b&&c!=d;\nconst neg = - -x;",
        "const s = \"it's\" + 'say \"hi\"';
const n = 0xff + 1_000 * 1.5e3;
const eq = a === b && c != d;
const neg = - -x;
",
    );
}

#[test]
fn delete_and_negative_literal_types() {
    assert_fmt(
        "function f() {\n  delete (r[\"a\"]);\n  delete(r.b);\n  g<-1>(5);\n  const c = a < -1;\n}\ntype N = -1|1;",
        "function f() {
  delete r[\"a\"];
  delete r.b;
  g<-1>(5);
  const c = a < -1;
}

type N = -1 | 1;
",
    );
}

#[test]
fn types_keep_required_parentheses() {
    assert_fmt(
        "type A = (X|Y)[];\ntype F = ((acc: i64, x: T) => i64)|null;\nfunction g<T extends (A|B)>(x: T) {}",
        "type A = (X | Y)[];
type F = ((acc: i64, x: T) => i64) | null;

function g<T extends (A | B)>(x: T) {}
",
    );
}

#[test]
fn braceless_bodies_get_braces() {
    assert_fmt(
        "function f(n: i64): i64 { if (n < 2) return n; else if (n > 9) return 9; while (n > 0) n--; return n; }",
        "function f(n: i64): i64 {
  if (n < 2) {
    return n;
  } else if (n > 9) {
    return 9;
  }
  while (n > 0) {
    n--;
  }
  return n;
}
",
    );
}

#[test]
fn crlf_input_keeps_crlf() {
    assert_fmt(
        "function f() {\r\n  g( );\r\n}\r\n",
        "function f() {\r\n  g();\r\n}\r\n",
    );
}

#[test]
fn very_long_chains_do_not_overflow() {
    let sum = vec!["x"; 3000].join(" + ");
    let eq = vec!["x"; 3000].join(" == ");
    let casts = format!("x{}", " as i64".repeat(2000));
    let members = format!("a{}", ".b".repeat(3000));
    let calls = format!("a{}", ".b()".repeat(2000));
    let src =
        format!("function f() {{ g({sum}); g({eq}); g({casts}); g({members}); g({calls}); }}");
    let once = format_source(&src).unwrap();
    assert!(common::parses(&once));
    assert_eq!(format_source(&once).unwrap(), once);
}

#[test]
fn throws_clauses_are_kept() {
    assert_fmt(
        "function load(id:string):string throws NotFound|Forbidden{return id;}
function done() throws E {}
async function f(): Promise<void> throws E {}
class C { constructor(x: i64) throws E {} m(): i64 throws E { return 1; } }
const g: (x: i64) => i64 throws E = (x: i64): i64 throws E => x;",
        "function load(id: string): string throws NotFound | Forbidden {
  return id;
}

function done() throws E {}

async function f(): Promise<void> throws E {}

class C {
  constructor(x: i64) throws E {}
  m(): i64 throws E {
    return 1;
  }
}

const g: (x: i64) => i64 throws E = (x: i64): i64 throws E => x;
",
    );
}

#[test]
fn parse_errors_are_refused() {
    assert!(format_source("function f( {").is_err());
}

/// The AST does not keep `this` parameters, so formatting would drop them.
#[test]
fn files_with_this_parameters_are_refused() {
    for src in [
        "function plain(this: void, x: number) { return x; }
",
        "const f = function (this: void, x: number) { return x; };
",
        "type Fn = (this: void, x: number) => number;
",
    ] {
        let diags = format_source(src).expect_err(src);
        assert_eq!(diags[0].message, velt_fmt::NO_THIS_PARAMS, "{src}");
    }
    // `this` elsewhere is formatted.
    assert!(format_source(
        "class C { m(): this { return this; } }
"
    )
    .is_ok());
}

#[test]
fn optional_params_and_object_type_fields_keep_their_spelling() {
    assert_fmt(
        "function f(a: i64, b?: string, c?: A | B | null) {}\ntype O = { x?: i64; y: string | null };\n",
        "function f(a: i64, b?: string, c?: A | B | null) {}\n\ntype O = { x?: i64; y: string | null };\n",
    );
}

#[test]
fn comments_stay_inside_object_types() {
    assert_fmt(
        "type Opts = {\n  // what a does\n  a?: bool;\n};\n",
        "type Opts = {\n  // what a does\n  a?: bool;\n};\n",
    );
    assert_fmt(
        "type Opts = { // the options
  // what a does
  a?: bool; b: i64, // b

  /* c */ c: string
  // last
};
function f(o: { /* x */ x: i64 }, e: { /* none */ }): { y: i64 /* y */ } {}
",
        "type Opts = {
  // the options
  // what a does
  a?: bool;
  b: i64; // b

  /* c */ c: string;
  // last
};

function f(
  o: {
    /* x */ x: i64;
  },
  e: {
    /* none */
  },
): {
  y: i64; /* y */
} {}
",
    );
}

#[test]
fn constructor_visibility_is_kept() {
    assert_fmt(
        "class A { private constructor(private readonly x: i64) {} }\nclass B { protected   constructor() {} }\nclass C { public constructor() {} }",
        "class A {
  private constructor(private readonly x: i64) {}
}

class B {
  protected constructor() {}
}

class C {
  constructor() {}
}
",
    );
}

#[test]
fn generator_methods_in_object_literals_stay_methods() {
    assert_fmt(
        "const bag = { *items(n: i64): Generator<i64> { yield n; }, async  *ticks(): AsyncGenerator<i64> { yield 1; }, f: function* () { yield 2; } };\n",
        "const bag = {
  *items(n: i64): Generator<i64> {
    yield n;
  },
  async *ticks(): AsyncGenerator<i64> {
    yield 1;
  },
  f: function* () {
    yield 2;
  },
};
",
    );
}
