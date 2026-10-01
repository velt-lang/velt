# {{name}}

A Velt library: text helpers (`slugify`, `truncate`, `WordCounter`).

```sh
velt test                # run tests/*.test.vlt
velt doc                 # HTML API docs from the /// comments -> target/doc/index.html
velt fmt
velt publish             # to the registry ($VELT_REGISTRY, default ~/.vlt/registry)
```

Using it from another package:

```sh
velt add {{name}}                        # from the registry
velt add {{name}} --path ../{{name}}     # or straight from a directory
```

```ts
import { slugify } from "{{name}}";
```

| File | What |
|---|---|
| `src/lib.vlt` | the public API: every `export` |
| `tests/lib.test.vlt` | `velt test` |
