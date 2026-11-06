# registry-conformance

What any Velt package registry must answer over `/api/v1`
([#1740](https://github.com/velt-lang/velt/issues/1740), step 2). The cases send requests and check
the answers, and know nothing about how a registry is built, so every host is held to the same
behaviour.

```sh
velt run src/main.velt <url> --alice <token> --bob <token>   # a registry with users alice and bob
velt run src/main.velt <url> --open                          # a registry without users
```

A run publishes packages named `conf-<id>-…` and leaves them there, so it needs no empty registry.
It exits 1 and lists every failed check when a case fails.

`crates/veltc/tests/registry_conformance.rs` runs the suite over HTTP against `velt registry serve`,
and `tests/memory.test.velt` runs it against registry-core's `Registry.handle` over in-memory stores.

**Not covered:**

- Native libraries, until registry-core has them (#1740 step 1d).
- Administrator actions (`velt registry user`, `velt registry owner`), which aren't HTTP.
- registry-core's stricter publish policy (#2225), which `velt registry serve` doesn't have.
- Exact refusal wording where the servers differ on purpose (#2222); cases check the status and
  the words clients rely on.
- Paths with `..` or `\`, which a URL-standard client resolves before sending. The servers' own
  tests cover them.
