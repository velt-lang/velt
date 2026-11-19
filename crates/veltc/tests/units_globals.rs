//! The globals std loads on demand (`std/prelude/global/*.velt`, velt_loader `globals`) in a
//! build from std's unit (#2943): std's unit has every module, lazy std only what the program
//! asked for, and sema asks "did std load this global's class?" in places (#3227). The loader
//! tells a check by units which globals the program left out (`Loaded::unloaded_globals`,
//! velt_sema `CheckOptions::unloaded_globals`), so both modes answer alike.
//!
//! One program per global file, run with lazy std and from std's unit on disk: the two runs
//! give the same exit status and output. A global file added to std without a program here
//! is run by the name it re-exports; one that re-exports nothing fails the test until it gets
//! a program of its own, so the next conditional global cannot drift.

mod no_window;
mod test_dir;

use std::path::{Path, PathBuf};

/// The programs of the globals that re-export no name (loaded for a construct, or when a std
/// module is loaded and the program says a word), and one that asks for no global at all.
const PROGRAMS: &[(&str, &str)] = &[
    (
        "<none>",
        "function add(a: number, b: number) { return a + b; }\nfunction main() { console.log(add.call(null, 1, 2)); }\n",
    ),
    (
        "this_fn",
        "function main() {\n  const f: any = function (this: any, k: number) { return this.n + k; };\n  console.log(f.call({ n: 1 }, 2));\n}\n",
    ),
    (
        "host_async",
        "declare host(\"env\") async function later(): Promise<number>;\nasync function main() { console.log(await later()); }\n",
    ),
    (
        "blob_typed",
        "async function main() { const b = new Blob([\"ab\"]); console.log((await b.arrayBuffer()).byteLength); }\n",
    ),
    (
        "bytes_view",
        "function main() { const b: u8[] = [1, 2, 3]; console.log(b.subarray(1).length); }\n",
    ),
    (
        "fetch_bytes",
        "async function main() { const r = new Response(\"ab\"); console.log((await r.arrayBuffer()).byteLength); }\n",
    ),
    (
        "fetch_forms",
        "async function main() { const r = new Response(\"ab\"); console.log((await r.blob()).size); }\n",
    ),
    (
        "fetch_body",
        "function main() { const r = new Response(\"ab\"); console.log(r.body === null); }\n",
    ),
    (
        "locale_compare",
        "function main() { console.log(\"a\".localeCompare(\"b\")); }\n",
    ),
    (
        "math_functions",
        "function main() { console.log(Math.exp(0)); }\n",
    ),
    (
        "record_reflect",
        "function main() { const r: Record<string, number> = { a: 1 }; console.log(Reflect.ownKeys(r).length); }\n",
    ),
    (
        "record_symbols",
        "function main() {\n  const s = Symbol(\"k\");\n  const r: Record<string | symbol, number> = {};\n  r[s] = 1;\n  console.log(r[s]);\n}\n",
    ),
    // Names `cert` and loads the TCP functions, without listening.
    (
        "tls_reactor",
        "import { connect } from \"velt/net\";\nfunction main() { const cert = 1; console.log(typeof connect, cert); }\n",
    ),
    (
        "to_fixed",
        "function main() { console.log((1.25).toFixed(1)); }\n",
    ),
    (
        "tuple_elements",
        "function main() { const t: [number, number] = [1, 2]; console.log(t.every((x) => x > 0)); }\n",
    ),
];

/// std's global files, by file stem.
fn globals() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std/prelude/global");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "velt"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
            (stem, std::fs::read_to_string(&p).unwrap())
        })
        .collect()
}

/// The first name a global file re-exports (`export { A, B as C } from "…"`: `A`).
fn first_reexport(src: &str) -> Option<String> {
    let (_, rest) = src.split_once("export {")?;
    let (names, _) = rest.split_once('}')?;
    let first = names.split(',').map(str::trim).find(|n| !n.is_empty())?;
    let name = first.rsplit(" as ").next()?.trim();
    Some(name.to_string())
}

/// The program that asks for global `stem`: its own of [`PROGRAMS`], or one that names what
/// the file re-exports.
fn program(stem: &str, src: &str) -> String {
    if let Some((_, p)) = PROGRAMS.iter().find(|(s, _)| *s == stem) {
        return p.to_string();
    }
    let name = first_reexport(src).unwrap_or_else(|| {
        panic!("std/prelude/global/{stem}.velt re-exports no name: give it a program in PROGRAMS")
    });
    format!("function main() {{\n  console.log(typeof {name});\n}}\n")
}

/// `velt run` of `main.velt` in `dir`, with lazy std or from std's unit in `unit`: exit code,
/// stdout, and stderr without the unit's own report lines.
fn run(dir: &Path, unit: Option<&Path>) -> (Option<i32>, String, String) {
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["run", "main.velt"]).current_dir(dir);
    for v in [
        "VELT_UNITS",
        "VELT_UNITS_SPLIT",
        "VELT_UNITS_BUILD",
        "VELT_UNITS_DISK",
        "VELT_UNITS_FREEZE",
    ] {
        cmd.env_remove(v);
    }
    if let Some(unit) = unit {
        cmd.env("VELT_UNITS", "2")
            .env("VELT_UNITS_SPLIT", "1")
            .env("VELT_UNITS_BUILD", "1")
            .env("VELT_UNITS_DISK", unit);
    }
    let out = cmd.output().unwrap();
    let stderr: Vec<&str> = std::str::from_utf8(&out.stderr)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with("velt-units:"))
        .collect();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr.join("\n"),
    )
}

#[test]
fn a_program_asking_for_a_global_runs_the_same_from_std_s_unit() {
    let dir = test_dir::TestDir::new();
    let unit = dir.path().join("unit");
    let mut programs: Vec<(String, String)> = vec![("<none>".into(), program("<none>", ""))];
    for (stem, src) in globals() {
        let p = program(&stem, &src);
        programs.push((stem, p));
    }
    let mut differ = vec![];
    for (i, (stem, src)) in programs.iter().enumerate() {
        let at = dir.path().join(format!("p{i}"));
        std::fs::create_dir_all(&at).unwrap();
        std::fs::write(at.join("main.velt"), src).unwrap();
        let lazy = run(&at, None);
        let units = run(&at, Some(&unit));
        if lazy != units {
            differ.push(format!(
                "{stem}:\n--- lazy std: {lazy:?}\n--- std's unit: {units:?}"
            ));
        }
    }
    assert!(differ.is_empty(), "{}", differ.join("\n\n"));
}

#[test]
fn the_first_name_a_global_re_exports_is_read_from_its_export_list() {
    let src = "// c\nexport { Blob as B, BlobPart } from \"velt/net/http/fetch/blob\";\n";
    assert_eq!(first_reexport(src).as_deref(), Some("B"));
    assert_eq!(first_reexport("import \"velt/lang/this_fn\";\n"), None);
}
