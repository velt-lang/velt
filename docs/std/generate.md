# velt/generate

`import { InstanceRequest, Output, Type, Field, quote } from "velt/generate"`. What a [source
generator](../reference/source-generators.md) is written with: a library declares
`declare generated("./gen") function f<T>(…): R;`, and the module `./gen` exports a function
`f(req: InstanceRequest, out: Output): void` that writes the body of `f` for each concrete
instance the program uses. The compiler runs it at compile time, in a separate process, and
checks the code it writes with the program. **Experimental**; the design and what is still to
come: [the design notes](../internals/design/source-generators.md).

- `InstanceRequest`: the instance asked for, and `req.model`.
- `Model`: every type the instance reaches, as `Type`s (a `kind`, `display`, `fields`, union
  `arms`, enum `members` as `EnumMember`s, a class's `base` and `subclasses`, and the `Method`s
  std's operations call by name, such as `toJSON` and `toString`; `mayBeProxy` when a typed
  `Proxy` may stand for a value of it; `hasProperties`, `classValue`, `dynamic`,
  `privateField`, and `ctorAccess` when a class's constructor is `private` or `protected`) and `Field`s (`name`, `type`, `optional`, `readonly`, `isPrivate`,
  `stdPrivate`, and `via` for a field an extension of the type adds to its objects). Types refer
  to each other by id, so recursive types are fine. Code generated for a type has that type's access: its `private` members and private
  constructor ([What generated code sees](../reference/source-generators.md#what-generated-code-sees)).
- `Output`: where the body goes (`line`, `body`), with top-level helpers (`helper`, named with
  `fresh`), the name of another instance of the same generator (`instance`), type names and
  string literals in generated code (`typeName`, `literal`), and errors (`error(message, at)`,
  at a type or field, or at the call that needed the instance; lines after the first are
  notes).
- `quote(s)`: `s` as a double-quoted Velt (and JavaScript) string literal.
- `serve`, `Session` and `GeneratorEntry` are the host's loop, which the compiler builds into
  the generator program; a generator does not call them.
