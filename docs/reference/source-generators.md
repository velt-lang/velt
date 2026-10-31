# Source generators

**Experimental.** A library can declare a generic function whose body is written at compile
time, for each type the program uses it with, by a *generator*: a function written in Velt that
reads a description of the types and writes Velt source. It is how a library serializes, compares
or prints values of your types without you writing code per type, and without reflection at run
time. The design, and what is still to come, is in
[the design notes](../internals/design/source-generators.md).

## Declaring a generated function

```ts ignore
// show/index.vlt
/** `value` as text: fields in order, strings quoted. */
export declare generated("./gen") function show<T>(value: T): string;
```

`declare generated("<module>") function` is a declaration without a body. The string names the
generator module as an import of the declaring module would: a relative path (`./gen`), a
module of a package (`some-package/gen`) or of the standard library. Callers are checked against the signature,
like any generic function, and use it as one:

```ts ignore
import { show } from "./show";

class Point {
  x: f64 = 1.5;
  y: f64 = -2;
}

function main() {
  console.log(show(new Point())); // Point { x: 1.5, y: -2 }
}
```

Each type `show` is used with gets its own function, also through generic code (a `show<T>` call
in a function that is called with `Point`). The program never names the generator.

A generated function throws only what its declaration's `throws` clause lists (nothing without
one): code generated for it that throws something else is an error.

## Writing a generator

The generator module exports a function with the same name, which `velt/generate` gives a
`InstanceRequest` and an `Output`:

```ts ignore
// show/gen.vlt
import { InstanceRequest, Output } from "velt/generate";

export function show(req: InstanceRequest, out: Output): void {
  const t = req.model.type(req.args[0]);  // the instance's type argument
  const v = req.params[0];                // the name of the parameter `value`
  if (t.kind == "string") {
    out.line(`return "'" + ${v} + "'";`);
  } else if (t.kind == "class" || t.kind == "object") {
    out.line(`let s = ${out.literal(t.display + " {")};`);
    for (const f of t.fields) {
      const field = out.instance("show", [f.type]); // the function generated for the field
      out.line(`s += ${out.literal(" " + f.name + ": ")} + ${field}(${v}.${f.name});`);
    }
    out.line(`return s + " }";`);
  } else {
    out.error(`show cannot write a ${t.kind}`, t.at);
    out.line(`return "";`);
  }
}
```

- `req.model` describes every type the instance reaches: `Type` (its `kind`, `display`,
  `fields`, union `arms`, enum `members`, a class's `base` and `subclasses`, …) and `Field`
  (`name`, `type`, `optional`, `readonly`, `isPrivate`). Types refer to each other by id, so
  recursive types are fine.
- `out.line(code)` and `out.body(code)` write the body; the compiler writes the function's
  signature. `out.helper(code)` adds a top-level declaration, named with `out.fresh("name")`.
- `out.instance(fn, [typeIds])` is the name of the function generated for another instance of a
  generated function of the same generator module; it is generated in the same run.
- `out.typeName(id)` writes a type in generated code: any type of the model, also one that is
  not exported, is declared inside a function or has no name at all (an intersection, an object
  type with a symbol key). It is a handle the compiler resolves (`__type<…>`), not the type's
  name; `instanceof` takes a class's.
- `out.literal(s)` writes a string as a Velt string literal.
- `out.error(message, at)` reports an error at a type's or a field's `at` (`-1`: at the call that
  needed the instance). The first line of `message` is the error, each further line a note.
  Errors from generators are reported with the program's other errors; the program is then not
  built. `velt/encoding/json` reports what `JSON.stringify` cannot write this way.

An optional field (`legs?: i32`) reads as `T | undefined`: the model gives `T` and
`optional: true`; a field `n?: T | null` reads `undefined` when absent and `null` when it holds
`null`. The prelude's `Map<K, V>`, `Record<K, V>` and `Set<T>` are the kinds `map`, `record` and
`set` (their `args` are the key and value types, or the element type), a `shared<T>` cell is
`shared`, and a function with properties is a `function`. An object literal's getter
(`get b() { … }`) is a field with `isGetter: true` whose `type` is what it returns. When an
interface with optional fields extends an object type and the program converts between the two,
they share objects, and the base's model lists the extension's fields too, each with `via` (the
extension's type id): read one as `(v as ${out.typeName(f.via)}).name`. A class or object type
whose values may be a typed `Proxy` (the program uses `Proxy`) has `mayBeProxy: true`. A
generator class is the kind `generator`; a function with properties has `hasProperties`, a class
used as a value (`typeof C`) `classValue`, and `object` and `Function` (kind `interface`), like
`any` and `unknown`, are `dynamic`. A std type that keeps runtime state in private fields shows
no fields (kind `opaque`, or `set`): its `privateField` names one, and a field such a type
declares (also in a class that extends it) has `stdPrivate`. A class whose constructor is
`private` or `protected` says so in `ctorAccess` (empty when it is public): a generator that
builds values without running the constructor (`__construct`) can refuse such a class. Compute names from `out` into variables before writing a line that uses them
(`const field = out.instance(…); out.line(…)`): one call cannot both modify `out` and read it in
an argument.

## What generated code sees

Generated code is checked in the scope of the module that declares the generated function: its
imports and its top-level declarations, exported or not, are visible as if the code were
appended to it. Generated code cannot import anything else (a generator that needs a function
has the declaring module import it), and it is never standard library code, even when a std
module declares the function: it has the powers any package has.

Code generated for a type has that type's access: it may use the `private` members and the
private constructor of the types its request reaches, and build such a class or struct without
running its constructor:

```ts ignore
// in a generator, for a class `t` of the model:
out.line(`return __construct<${out.typeName(t.id)}>({ name: n, secret: s });`);
```

A union is written as its active member with `__arm(v, i)`, true when the union `v` holds its
member `i` (the index into the model's `arms`). It narrows `v` like `typeof` and `instanceof`
do, also between members no JavaScript test tells apart (`i32 | f64`, `i32[] | f64[]`, two
object types without a discriminant):

```ts ignore
// in a generator, for a union `t` of the model and the parameter `v`:
for (let i: i64 = 0; i < (t.arms.length as i64); i++) {
  const write = out.instance("write", [t.arms[i]]);
  out.line(`if (__arm(${v}, ${i})) { return ${write}(${v}); }`);
}
```

`__construct<T>({ … })` takes the fields as an object literal. A field with an initializer that
is left out gets the initializer's value, an optional field left out is absent, and any other
field must be given. Neither `__construct` nor the private members of other types are available
to generated code for types its request does not reach. ES private fields (`#x`) stay private to
their class, as in JavaScript.

## What a generator may do

A generator is a pure function of its request: it may not read files, the network, the
environment, the clock or random numbers, start processes or call native code. The compiler
checks this when it builds the generators, from everything their code can reach, and reports
what it found (`a source generator cannot reach the clock`). That is what lets it cache their
output, and what makes running a dependency's generator at compile time safe.

## How it runs

While the compiler checks the program, it collects the instances that need code, builds the
generators into a program of their own and runs it in a separate process, and checks the code
they write as part of the same check: a program is checked once, with or without generators
(generated code that uses a closure is checked in a second check). A generator that throws, or
code it writes that does not compile, is reported at the call that needed it; the second names
the line of the generated code in a note.

Generators are built once and cached, and so is what they write: a build that needs the same
code for the same types reuses it without running the generator. Changing a generator, or a
type it was given, runs it again.
