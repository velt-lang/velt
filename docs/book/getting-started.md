# Getting started

## Install

You need:

- a **system linker**: the *Build Tools for Visual Studio* ("Desktop development with C++") on
  Windows, `build-essential` (or `gcc`) on Linux, the Xcode command line tools on macOS;
- optionally **LLVM/clang 16 or newer**, for optimized `--release` builds.

Then install the latest release. On Linux and macOS:

```sh
curl -fsSL https://github.com/velt-lang/velt/releases/latest/download/get-velt.sh | sh
```

On Windows, in PowerShell:

```powershell
irm https://github.com/velt-lang/velt/releases/latest/download/get-velt.ps1 | iex
```

The installer puts the toolchain in `~/.velt/toolchain` (`%LOCALAPPDATA%\velt` on Windows) and
adds its `bin` directory to your `PATH`. Open a new terminal and check the installation:

```sh
velt doctor
```

`velt doctor` checks the toolchain and builds and runs a hello world. Installer options and
other platforms: [Platforms and installation](../tooling/platforms.md).

### From source

Building Velt yourself also needs **Rust** (stable), from [rustup.rs](https://rustup.rs):

```sh
git clone https://github.com/velt-lang/velt
cd velt
cargo build --release -p veltc -p velt_rt
./target/release/velt doctor
```

Put `target/release` on your `PATH`, or build a self-contained toolchain directory with `scripts/package.sh` (Linux,
macOS) or `scripts/package.ps1` (Windows) and install it with the matching `install` script
([Platforms and installation](../tooling/platforms.md)).

## Your first program

Create `hello.vlt`:

```ts
function main() {
  const name = "world";
  console.log(`Hello, ${name}!`);
}
```

```sh
velt run hello.vlt              # debug build (Cranelift, compiles fast), then run
velt run --release hello.vlt    # optimized build (LLVM -O3 when clang is installed)
velt build hello.vlt            # just build: ./target/velt/hello
```

The result is a native executable with no runtime to install: `target/velt/hello` (`.exe` on
Windows) runs on its own.

## A project

```sh
velt new hello
cd hello
velt run
```

```
Hello, world!
```

`velt new` creates a package:

```
hello/
  package.vlt           the manifest: name, version, dependencies
  src/main.vlt          the entry point: `velt run` builds and runs it
  src/greet.vlt         a module, imported by main and by the tests
  tests/greet.test.vlt  tests: every exported `test_*` function
  README.md
  .gitignore            ignores target/, where builds go
```

```ts ignore
import type { Package } from "velt:package";

export const pkg: Package = { name: "hello", version: "0.1.0" };
```

The manifest is written in Velt but holds only data: `velt` reads it without running it
([`package.vlt`](../tooling/manifest.md)).

`src/main.vlt` imports the greeting from its own module and the program arguments from the
standard library:

```ts ignore
import { args } from "velt:process";
import { greet } from "./greet";

function main() {
  const argv = args();
  console.log(greet(argv.length > 0 ? argv[0] : "world"));
}
```

The everyday commands, inside the package:

| Command | What it does |
|---|---|
| `velt run` | build and run `src/main.vlt` |
| `velt run -- Ada` | pass arguments to the program (after `--`) |
| `velt dev` | run, then hot-swap or restart on every save ([Hot reload](hot-reload.md)) |
| `velt test` | run the tests ([Testing](testing.md)) |
| `velt fmt` | format the sources |
| `velt build --release` | an optimized binary in `target/velt/hello` |

Other templates start you off with a bigger skeleton: `velt new todo --template api` (a JSON
HTTP API), `--template cli` (a command-line tool), `--template websocket` (a chat server and
client), `--template lib` (a library to publish).

## Editor setup

Install the VS Code extension from `editors/vscode` for highlighting, diagnostics as you type,
go-to-definition, completion and quick fixes, or point any LSP client at `velt lsp`
([Editors](../tooling/editors.md)).

## Next steps

- [A tour of Velt](tour.md) covers the language in one page.
- [Velt for TypeScript developers](ts-developers.md) lists every difference from TypeScript.
