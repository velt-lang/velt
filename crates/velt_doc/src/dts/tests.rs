//! The type package of a small std built by hand.

use std::collections::BTreeMap;

use super::type_package;
use crate::Input;

fn input(file: &str, source: &str) -> Input {
    let module = file.trim_end_matches(".velt").trim_end_matches("/index");
    Input {
        module: format!("std/{module}"),
        source: source.to_string(),
        file: file.to_string(),
    }
}

/// A small std: a public module `box` declared in internal files, `io`, the prelude.
fn package() -> BTreeMap<String, String> {
    let inputs = [
        input(
            "prelude/sync.velt",
            "/** A lock. */\nexport class Mutex<T> {\n  lock(): T { panic(\"\"); }\n}\n\n\
             export extend<T> Mutex<T> {\n  /** The count. */\n  count(): i64 { return 0; }\n}\n\n\
             export extend Mutex<string> {\n  joined(): string { return \"\"; }\n}\n\n\
             export extend<T> Array<T> {\n  total(): i64 { return 0; }\n}\n",
        ),
        input("io/index.velt", "export class IoError {\n  code: string;\n}\n"),
        // The intrinsics are `lang/`, but no globals: only std imports them.
        input("lang/intrinsics/modes.velt", "export type Mut<T> = T;\n"),
        input(
            "lang/intrinsics/array.velt",
            "import type { Mut } from \"./modes\";\n\
             @intrinsic\nexport declare function arraySwap<T>(xs: Mut<T[]>, i: usize, j: usize): void;\n",
        ),
        input(
            "box/index.velt",
            "export { Box, make } from \"./box\";\n\
             export { Opts as BoxOptions } from \"velt/box/opts\";\n",
        ),
        input("box/opts.velt", "export struct Opts {\n  flag?: bool;\n}\n"),
        input("box/hidden.velt", "export class Hidden {\n  n: u8;\n}\n"),
        input(
            "box/box.velt",
            "import { IoError } from \"velt/io\";\nimport { Hidden } from \"./hidden\";\n\
             import { Opts } from \"./opts\";\n\n\
             /** Holds a value. */\nexport class Box<T> {\n  value: T;\n  private secret: i64 = 0;\n\
             \x20 constructor(h: Hidden, value: T) { this.value = value; }\n\
             \x20 static of<U>(v: U): Box<U> { panic(\"\"); }\n\
             \x20 static empty(): Box<T> { panic(\"\"); }\n\
             \x20 get size(): i64 { return 1; }\n\
             \x20 lock(): Mutex<T> { panic(\"\"); }\n\
             \x20 failure(): IoError | null { return null; }\n}\n\n\
             export async function make(o: Opts = Opts {}, n?: i32): Promise<Box<f64>, IoError> {\n\
             \x20 panic(\"\");\n}\n",
        ),
    ];
    type_package(&inputs, "1.2.3").into_iter().collect()
}

#[test]
fn a_module_declared_in_internal_files() {
    let files = package();
    let names: Vec<&str> = files.keys().map(|k| k.as_str()).collect();
    assert_eq!(
        names,
        [
            "README.md",
            "box/index.d.ts",
            "globals.d.ts",
            "io/index.d.ts",
            "package.json"
        ]
    );
    let b = &files["box/index.d.ts"];
    let want = [
        // A type of the prelude is imported from globals.d.ts, one of another public module
        // from that module.
        "import type { Mutex } from \"../globals.js\";",
        "import type { IoError } from \"velt/io\";",
        "/**\n * Holds a value.\n */\nexport declare class Box<T> {",
        "  value: T;",
        // Velt's types in TypeScript's, a static method with its class's type parameter.
        "  constructor(h: Hidden, value: T);",
        "  static of<U>(v: U): Box<U>;",
        "  static empty<T>(): Box<T>;",
        "  get size(): number;",
        "  lock(): Mutex<T>;",
        "  failure(): IoError | null;",
        // No error type, a parameter with a default is optional.
        "export declare function make(o?: Opts, n?: number): Promise<Box<number>>;",
        // A type no public module exports, under the name the declaration uses.
        "declare class Hidden {\n  n: number;\n}",
        // Exported under another name than it is declared with.
        "export declare class BoxOptions {\n  flag?: boolean;\n}",
        "declare type Opts = BoxOptions;",
    ];
    for w in want {
        assert!(b.contains(w), "missing {w:?} in:\n{b}");
    }
    assert!(!b.contains("secret") && !b.contains("IoError>"), "{b}");
}

#[test]
fn globals_and_extensions() {
    let files = package();
    let g = &files["globals.d.ts"];
    let want = [
        "/**\n * A lock.\n */\nexport declare class Mutex<T> {",
        // The extension's parameters are the target's: the methods as written.
        "export interface Mutex<T> {\n  /**\n   * The count.\n   */\n  count(): number;\n}",
        // A specialized target: a `this` parameter.
        "export interface Mutex<T> {\n  joined(this: Mutex<string>): string;\n}",
        "export {};",
    ];
    for w in want {
        assert!(g.contains(w), "missing {w:?} in:\n{g}");
    }
    // Nothing global: a type-only import must not change the client's globals, so neither
    // Velt's globals nor its methods on TypeScript's own types (`Array`) are declared there.
    assert!(!g.contains("declare global") && !g.contains("total"), "{g}");
    assert!(!g.contains("type Mut") && !g.contains("arraySwap"), "{g}");
    let json = &files["package.json"];
    assert!(json.contains("\"version\": \"1.2.3\""), "{json}");
    assert!(
        json.contains("\"./box\": { \"types\": \"./box/index.d.ts\" }"),
        "{json}"
    );
}

/// Members keyed by symbols (`[KEY]`): each file that declares one has its key, as TypeScript
/// requires (#2055).
#[test]
fn symbol_keyed_members_have_their_keys() {
    let inputs = [
        input(
            "lang/keys.velt",
            "export const INSPECT = Symbol.for(\"nodejs.util.inspect.custom\");\n",
        ),
        // The compiler's hooks: no program names them, so they are no globals.
        input(
            "lang/hooks/keys.velt",
            "export const INDEX = Symbol(\"index\");\n\
             export function recordOf<K, V>(m: Map<K, V>): Record<K, V> { panic(\"\"); }\n",
        ),
        input("io/index.velt", "export { Source, CHUNKS } from \"./source\";\n"),
        input(
            "io/source.velt",
            "export const CHUNKS = Symbol(\"chunks\");\n\
             export const LIMIT = \"limit\";\n\
             export class Source {\n  [CHUNKS](): i64 { return 0; }\n  [LIMIT]: i64 = 0;\n\
             \x20 [Symbol.iterator](): Iterator<i64> { panic(\"\"); }\n}\n",
        ),
        input("net/index.velt", "export { Body } from \"./body\";\n"),
        input(
            "net/body.velt",
            "import { Source, CHUNKS } from \"velt/io\";\n\
             import { INDEX } from \"velt/lang/hooks/keys\";\n\
             const SETTLE = Symbol(\"settle\");\n\
             export class Body extends Source {\n  [CHUNKS](): i64 { return 1; }\n\
             \x20 [SETTLE](): void {}\n  [INDEX](i: i64): i64 { return i; }\n  [INSPECT](): string { return \"\"; }\n\
             \x20 get [KIND](): string { return \"\"; }\n}\n",
        ),
    ];
    let files: BTreeMap<String, String> = type_package(&inputs, "1.2.3").into_iter().collect();
    let g = &files["globals.d.ts"];
    assert!(
        g.contains("export declare const INSPECT: unique symbol;"),
        "{g}"
    );
    assert!(!g.contains("INDEX") && !g.contains("recordOf"), "{g}");
    let io = &files["io/index.d.ts"];
    for w in [
        "export declare const CHUNKS: unique symbol;",
        // A key not exported publicly is declared where it is used.
        "declare const LIMIT = \"limit\";",
        "  [CHUNKS](): number;",
        "  [LIMIT]: number;",
        "  [Symbol.iterator](): Iterator<number>;",
    ] {
        assert!(io.contains(w), "missing {w:?} in:\n{io}");
    }
    assert!(!io.contains("declare const Symbol"), "{io}");
    let net = &files["net/index.d.ts"];
    for w in [
        "import type { INSPECT } from \"../globals.js\";",
        "import type { CHUNKS, Source } from \"velt/io\";",
        // A file's own key, and one the file does not declare (a symbol).
        "declare const SETTLE: unique symbol;",
        "declare const KIND: unique symbol;",
        // A hook's key, which no public module exports.
        "declare const INDEX: unique symbol;",
        "  [INDEX](i: number): number;",
        "  [SETTLE](): void;",
        "  [INSPECT](): string;",
        "  get [KIND](): string;",
    ] {
        assert!(net.contains(w), "missing {w:?} in:\n{net}");
    }
}

/// Operator methods (`[Symbol.add]`): the files that declare them import a `Symbol` that has
/// those symbols, the one `globals.d.ts` exports, so a client's `Symbol` stays TypeScript's.
#[test]
fn operator_keys_come_from_the_globals_file() {
    let inputs = [
        input("ops/index.velt", "export { Add } from \"./add\";\n"),
        input(
            "ops/add.velt",
            "export interface Add<R, O = R> {\n  [Symbol.add](other: R): O;\n}\n",
        ),
        input("math/index.velt", "export { Big } from \"./big\";\n"),
        input(
            "math/big.velt",
            "export class Big {\n  [Symbol.neg](): Big { panic(\"\"); }\n\
             \x20 static [Symbol.mul](l: f64, r: Big): Big { panic(\"\"); }\n\
             \x20 [Symbol.iterator](): Iterator<i64> { panic(\"\"); }\n}\n",
        ),
        input("io/index.velt", "export { Source } from \"./source\";\n"),
        input(
            "io/source.velt",
            "export class Source {\n  [Symbol.iterator](): Iterator<i64> { panic(\"\"); }\n}\n",
        ),
    ];
    let files: BTreeMap<String, String> = type_package(&inputs, "1.2.3").into_iter().collect();
    let import = "import type { Symbol } from \"../globals.js\";";
    let ops = &files["ops/index.d.ts"];
    assert!(ops.contains(import), "{ops}");
    assert!(ops.contains("  [Symbol.add](other: R): O;"), "{ops}");
    let math = &files["math/index.d.ts"];
    assert!(math.contains(import), "{math}");
    assert!(
        math.contains("  static [Symbol.mul](l: number, r: Big): Big;"),
        "{math}"
    );
    // TypeScript's own symbols need nothing.
    let io = &files["io/index.d.ts"];
    assert!(!io.contains("Symbol }"), "{io}");
    let g = &files["globals.d.ts"];
    assert!(
        g.contains("export declare const Symbol: SymbolConstructor & {"),
        "{g}"
    );
    for name in velt_syntax::OPERATOR_SYMBOLS {
        assert!(
            g.contains(&format!("  readonly {name}: unique symbol;\n")),
            "{g}"
        );
    }
    assert!(!g.contains("declare global"), "{g}");
}
