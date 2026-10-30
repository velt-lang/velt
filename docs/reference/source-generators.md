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
generator module, relative to the declaring module. Callers are checked against the signature,
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
`Request` and an `Output`:

```ts ignore
// show/gen.vlt
import { Request, Output } from "velt/generate";

export function show(req: Request, out: Output): void {
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
- `out.typeName(id)` writes a type in generated code, also one that is not exported.
- `out.literal(s)` writes a string as a Velt string literal.
- `out.error(message, at)` reports an error at a type's or a field's `at` (`-1`: at the call that
  needed the instance).

An optional field (`legs?: i32`) reads as `T | undefined`: the model gives `T` and
`optional: true`. Compute names from `out` into variables before writing a line that uses them
(`const field = out.instance(…); out.line(…)`): one call cannot both modify `out` and read it in
an argument.

## How it runs

The compiler checks the program, collects the instances that need code, builds the generators
into one program and runs it in a separate process (JIT-compiled, as `velt dev` runs programs),
then checks the program again with the generated code. A generator that throws, or code it writes
that does not compile, is reported at the call that needed it; the second names the line of the
generated code in a note.

Limits for now: the generator module must be a relative path, generated code does not get access
to private fields, and generators are not cached between builds.
