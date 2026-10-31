//! The type package of a small std built by hand.

use std::collections::BTreeMap;

use super::type_package;
use crate::Input;

fn input(file: &str, source: &str) -> Input {
    let module = file.trim_end_matches(".vlt").trim_end_matches("/index");
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
            "prelude/sync.vlt",
            "/** A lock. */\nexport class Mutex<T> {\n  lock(): T { panic(\"\"); }\n}\n\n\
             export extend<T> Mutex<T> {\n  /** The count. */\n  count(): i64 { return 0; }\n}\n\n\
             export extend Mutex<string> {\n  joined(): string { return \"\"; }\n}\n\n\
             export extend<T> Array<T> {\n  total(): i64 { return 0; }\n}\n",
        ),
        input("io/index.vlt", "export class IoError {\n  code: string;\n}\n"),
        // The intrinsics are `lang/`, but no globals: only std imports them.
        input("lang/intrinsics/modes.vlt", "export type Mut<T> = T;\n"),
        input(
            "lang/intrinsics/array.vlt",
            "import type { Mut } from \"./modes\";\n\
             export declare intrinsic function arraySwap<T>(xs: Mut<T[]>, i: usize, j: usize): void;\n",
        ),
        input(
            "box/index.vlt",
            "export { Box, make } from \"./box\";\n\
             export { Opts as BoxOptions } from \"velt/box/opts\";\n",
        ),
        input("box/opts.vlt", "export struct Opts {\n  flag?: bool;\n}\n"),
        input("box/hidden.vlt", "export class Hidden {\n  n: u8;\n}\n"),
        input(
            "box/box.vlt",
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
