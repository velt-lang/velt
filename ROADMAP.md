# Roadmap

Velt is pre-1.0. These are the milestones ahead, roughly in order; each is tracked in GitHub
issues. Breaking changes are expected until 1.0: old syntax is removed, not deprecated, and the
compiler explains how to update code.

| Milestone | What it brings |
|---|---|
| **Packages** | A public package registry; module-scoped `extend` blocks, visible only to the modules that import them. |
| **Platform** | Windows on arm64: a native toolchain, unwind information for JIT code, hot swap and CI. |
| **Performance** | Separate compilation, so std and packages are checked and compiled once and a build compiles only the app; smaller closure environments. |
| **Tooling** | A warm front end in `velt dev`, which keeps the checked program between edits. |
| **Semantics stage 3** | Reference cycles freed as in Node: a collector confined to the types that can form cycles, `weak` references, and a lint. ([design](docs/internals/design/cycles.md)) |

Proposals for language changes start as issues; see
[CONTRIBUTING.md](CONTRIBUTING.md#proposing-a-language-change).
