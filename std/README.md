# The Velt standard library

Written in Velt. `import { x } from "velt/<module>"` names the module whose folder is
`std/<module>/`, and its `index.vlt` is the module's API; `prelude/` is in scope everywhere
and `lang/` implements the language, so neither is imported.

- What each module does, for users: [veltlang.com/docs/std](https://veltlang.com/docs/std/), one
  page per module at the same path, and `velt doc --std` for the API reference.
- How std is laid out and why: [the std layout design](../docs/internals/design/std-layout.md).
- Working on it (the layout rules `scripts/std-lint` checks, imports, the prelude's order, new
  modules, runtime bindings, performance): CONTRIBUTING.md, [The standard
  library](../CONTRIBUTING.md#the-standard-library). `velt check --std` checks every file, and
  `velt test std/` runs the white-box tests next to the code.
