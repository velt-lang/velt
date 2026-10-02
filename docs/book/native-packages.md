# Packages with native code

Most packages are pure Velt. When a package needs code that only exists natively (a database
driver, an image codec, a C library), it can include a **Rust crate**. Users of the package never
need Rust: you publish a prebuilt library for each platform, and `velt` downloads the right one.
This chapter writes one; `packages/sqlite` in the Velt repository is a complete example.

## Layout

```text
greet/
  package.vlt
  src/lib.vlt          # the package's Velt API
  native/Cargo.toml    # the Rust crate
  native/src/lib.rs
```

```ts ignore
// package.vlt
import type { Package } from "velt:package";

export const pkg: Package = {
  name: "greet",
  version: "0.1.0",
  native: { targets: ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-pc-windows-msvc"] },
};
```

```toml
# native/Cargo.toml
[package]
name = "greet_native"
version = "0.1.0"
edition = "2021"

[lib]
name = "velt_native_greet"           # always velt_native_<package>
crate-type = ["cdylib", "staticlib"]

[dependencies]
velt_native = "0.1"
```

`velt_native` is not on crates.io yet: until it is, depend on it by path or git
(`velt_native = { git = "https://github.com/velt-lang/velt" }`).

## The Rust side

```rust
use velt_native::{export, Error};

velt_native::package!(greet); // once per crate: the start-up function

#[export]
fn greet_hello(name: &str, excited: bool) -> String {
    format!("hello, {name}{}", if excited { "!" } else { "" })
}

#[export]
fn greet_parse(text: &str) -> Result<i64, Error> {
    text.trim().parse().map_err(|e| Error::other(format!("{e}")))
}

#[export(blocking)] // an async function: runs on the runtime's blocking pool
fn greet_slow(n: u64) -> Result<u64, Error> {
    std::thread::sleep(std::time::Duration::from_millis(10));
    Ok(n * 2)
}
```

- Every exported function's name starts with the package name and `_` (`greet_…`; a `-` in the
  package name becomes `_`).
- Parameters are lent for the call only, so references take no lifetime (`&str`, not
  `&'static str`).
- Parameters: `bool`, integers, `f32`/`f64`, `&str`/`String` (a Velt `string`), `&[u8]`/
  `Vec<u8>` (a `u8[]`). `blocking` functions take owned values (`String`, `Vec<u8>`).
- Results: the same scalars, `()`, `String`, `Vec<u8>`, or `Result<T, velt_native::Error>`
  (an `IoResult<T>` in Velt; `Result<(), Error>` is an `IoStatus`).
- A panic becomes an error result (`native panic: …`) where the result can carry one.
- Objects (a connection, a decoder) stay on the Rust side behind a `u64` handle.

## The Velt side

The package declares each function with exactly the signature the Rust side exports, and wraps
them in a Velt API:

```ts ignore
import { IoResult } from "velt:io";

declare function greet_hello(name: string, excited: bool): string;
declare function greet_parse(text: string): IoResult<i64>;
declare async function greet_slow(n: u64): Promise<IoResult<u64>>;

export function hello(name: string): string {
  return greet_hello(name, true);
}
```

`velt` checks every `declare` against the library: a name it does not export, or a type that
differs, is a compile error at the `declare`, not a crash at run time:

```text
src/lib.vlt:5:1: error: `declare` of `greet_parse` does not match the native library of `greet 0.1.0`
  = note: the library exports `(string)->IoResult<i64>`
  = note: this declares     `(string)->IoResult<i32>`
```

Wrap handles in a class that releases them in `[Symbol.dispose]()`, so `using` and the end of
the object's life close them (see `packages/sqlite/src/lib.vlt`).

## Building and publishing

While you work on the package, any program that depends on it by path builds the crate with
cargo when needed. To publish:

```sh
velt native build                                  # this machine's target → target/velt-native/<triple>/
velt native build --target aarch64-apple-darwin    # another target your toolchain can build
velt publish                                       # the package plus one library per target
```

`velt publish` needs the crate's `Cargo.lock` (commit it) and a library for every target in
`native.targets`. Build the others on machines for those platforms (a CI matrix), collect their `target/velt-native/<triple>/`
directories into one directory, and pass it with `velt publish --native-artifacts <dir>`. A
target can be added to a published version later (`velt publish --native-only`); a published
library is never replaced.

## Using the package

```sh
velt add greet
```

`velt add` downloads the library for your machine, checks it against the checksum recorded in
`velt.lock` (which pins the library of every published target), and lists the packages that run
native code. Then `velt run`, `velt build --release` (a self-contained executable) and `velt dev`
(the library is loaded into the running program; Velt edits still hot-swap) work as usual.
