//! A read-only array is a type of its own while sema checks a program and its array for
//! lowering (#3061): a program with `readonly T[]` gives the VIR the same program gives with
//! `readonly` left out. A generic function value with a read-only rest parameter was once boxed
//! through an interface value where the erased program calls a generic function.

mod no_window;
mod test_dir;

fn vir(src: &str) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.velt", "--emit", "vir"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `src` with each `readonly ` blanked out (columns kept, so spans and names stay the same).
fn erased(src: &str) -> String {
    src.replace("readonly ", "         ")
}

const PROGRAMS: [&str; 5] = [
    // A generic function value with a read-only rest parameter.
    "const r: <T>(...a: readonly T[]) => number = (...a) => a.length;\nconsole.log(r(1, 2, 3));\n",
    // A non-generic one, and an annotated arrow.
    "const r = (...a: readonly number[]): number => a.length;\nconsole.log(r(1, 2));\n",
    // A field, a parameter and a result.
    "class H { xs: readonly number[] = [1, 2]; }\nfunction sum(xs: readonly number[]): number { let s = 0; for (const x of xs) s += x; return s; }\nfunction make(): readonly number[] { return [3]; }\nconsole.log(sum(new H().xs), make().length);\n",
    // A mutable array where a read-only one is expected, also inside a promise and an object.
    "async function f(): Promise<number[]> { return [1]; }\nfunction show(p: { xs: readonly number[] }): number { return p.xs.length; }\nasync function main(): Promise<void> {\n  const p: Promise<readonly number[]> = f();\n  const o: { xs: number[] } = { xs: [1, 2] };\n  console.log((await p).length, show(o));\n}\n",
    // Comparison and rest destructuring.
    "const m: number[] = [1, 2];\nconst r: readonly number[] = m;\nconst [a, ...rest] = r;\nrest.push(3);\nconsole.log(m === r, a, rest.length);\n",
];

#[test]
fn readonly_programs_lower_as_their_erased_forms() {
    for src in PROGRAMS {
        let (with, without) = (vir(src), vir(&erased(src)));
        assert!(
            with == without,
            "VIR differs for:\n{src}\n--- readonly ---\n{with}\n--- erased ---\n{without}"
        );
    }
}
