# Security policy

## Supported versions

Velt is pre-1.0. Security fixes go into the `main` branch and the latest release; older
versions are not patched.

## Reporting a vulnerability

Please **do not open a public issue** for a security problem. Report it privately through
GitHub's [private vulnerability reporting](https://github.com/velt-lang/velt/security/advisories/new)
for this repository.

Include what you can: the affected component (compiler, runtime, standard library module,
package manager, language server, playground), the version or commit, a minimal reproduction,
and the impact you expect. We aim to acknowledge reports within a week, and we will keep you
informed while we work on a fix. Once a fix is released we publish an advisory, with credit to
you unless you prefer otherwise.

## Scope

In scope, for example:

- memory unsafety in compiled programs that the compiler should have rejected (a program without
  `declare function` or intrinsics that reads freed memory, races, or corrupts data);
- vulnerabilities in the runtime or standard library (HTTP parsing, TLS configuration,
  WebSockets, database drivers, path handling, the regex engine binding);
- the package manager and registry server (checksum verification, archive extraction, uploads);
- `velt playground` and `velt registry serve` when bound to their default local addresses.

Out of scope: denial of service by compiling deliberately huge programs, and programs that use
`declare function` to call the runtime's C ABI directly.
