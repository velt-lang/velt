//! A large array literal of number literals (#2421): the parser packs it (`ast::ExprKind::
//! NumArray`), and sema checks it as the array literal of its elements, giving one
//! `hir::ExprKind::ConstArray` against an array of a number type and the ordinary `ArrayLit`
//! anywhere else. Each program here checks to the same diagnostics and the same HIR as when
//! nothing is packed (the `ConstArray`s read as the `ArrayLit`s they stand for).

mod common;

use common::programs::{is_unpacked, load_src, unpacked};

/// `[e0, e1, …]` of `n` elements, `f(i)` the text of element `i`.
fn array(n: usize, f: impl Fn(usize) -> String) -> String {
    let elems: Vec<String> = (0..n).map(f).collect();
    format!("[{}]", elems.join(", "))
}

/// 20 small numbers, element `at` replaced by `odd`.
fn with(at: usize, odd: &str) -> String {
    array(20, |i| if i == at { odd.into() } else { i.to_string() })
}

/// The rendered diagnostics and the HIR (its `Debug`) of `src`.
fn checked(src: &str) -> (String, String) {
    let l = load_src(src);
    let (p, d) = l.check();
    // The definitions (the program's maps print in no fixed order).
    (l.render(&d), format!("{:#?}", p.map(|p| p.defs)))
}

/// `src` checked with packing and without: the same diagnostics, and the same HIR once each
/// `ConstArray` is read as its `ArrayLit`. Returns the diagnostics and how many `ConstArray`s
/// the packed HIR has.
fn same(src: &str) -> (String, usize) {
    let (diags, hir) = checked(src);
    let (plain_diags, plain_hir) = unpacked(|| checked(src));
    assert_eq!(diags, plain_diags, "diagnostics differ for:\n{src}");
    let packed = hir.matches("ConstArray(").count();
    assert!(
        !plain_hir.contains("ConstArray("),
        "unpacked HIR has a ConstArray"
    );
    let expanded = expand(&hir);
    if expanded != plain_hir {
        let at = expanded
            .lines()
            .zip(plain_hir.lines())
            .position(|(a, b)| a != b)
            .unwrap_or(0);
        let show = |s: &str| {
            s.lines()
                .skip(at.saturating_sub(5))
                .take(12)
                .collect::<Vec<_>>()
                .join("\n")
        };
        panic!(
            "HIR differs for:\n{src}\n--- packed (expanded), from line {at}:\n{}\n--- unpacked:\n{}",
            show(&expanded),
            show(&plain_hir)
        );
    }
    (diags, packed)
}

/// The pretty `Debug` of a HIR with each `ConstArray` written as the `ArrayLit` of its elements
/// (`ConstArray::exprs`), as `{:#?}` prints that.
fn expand(hir: &str) -> String {
    let lines: Vec<&str> = hir.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let Some(head) = line.strip_suffix("ConstArray(") else {
            out.push(line.to_string());
            i += 1;
            continue;
        };
        let indent = line.len() - line.trim_start().len();
        // `ConstArray(` / `ConstArray {` / `elem: TyId(` / `n,` / `),` / `values: [` / elements
        // / `],` / `},` / `),`, then the enclosing expression's `ty` and `span`.
        let field = |l: &str, name: &str| -> u64 {
            let t = l.trim().trim_end_matches(',');
            let v = t
                .strip_prefix(name)
                .unwrap_or_else(|| panic!("expected {name} in {l}"));
            v.trim()
                .parse()
                .unwrap_or_else(|_| panic!("a number in {l}"))
        };
        let mut j = i + 1;
        assert!(lines[j].trim() == "ConstArray {", "{}", lines[j]);
        j += 1;
        assert!(lines[j].trim() == "elem: TyId(", "{}", lines[j]);
        let elem: u64 = lines[j + 1].trim().trim_end_matches(',').parse().unwrap();
        j += 3;
        assert!(lines[j].trim() == "values: [", "{}", lines[j]);
        j += 1;
        let mut elems = vec![];
        while lines[j].trim() == "ConstElem {" {
            let bits = field(lines[j + 1], "bits:");
            let float = lines[j + 2].trim() == "float: true,";
            let lo = field(lines[j + 3], "lo:");
            let hi = field(lines[j + 4], "hi:");
            elems.push((bits, float, lo, hi));
            j += 6;
        }
        assert!(lines[j].trim() == "],", "{}", lines[j]);
        assert!(lines[j + 1].trim() == "},", "{}", lines[j + 1]);
        assert!(lines[j + 2].trim() == "),", "{}", lines[j + 2]);
        j += 3;
        // The enclosing `Expr`'s span gives the file.
        let at = (j..lines.len())
            .find(|&k| lines[k].trim() == "file: FileId(")
            .expect("a span after the array");
        let file = lines[at + 1].trim().trim_end_matches(',');
        let pad = " ".repeat(indent);
        out.push(format!("{head}ArrayLit("));
        out.push(format!("{pad}    ["));
        for (bits, float, lo, hi) in elems {
            let lit = match float {
                true => format!(
                    "Float(\n{pad}                    {:?},\n{pad}                ),",
                    f64::from_bits(bits)
                ),
                false => format!("Int(\n{pad}                    {bits},\n{pad}                ),"),
            };
            out.push(format!("{pad}        Expr {{"));
            out.push(format!("{pad}            kind: Lit("));
            out.push(format!("{pad}                {lit}"));
            out.push(format!("{pad}            ),"));
            out.push(format!(
                "{pad}            ty: TyId(\n{pad}                {elem},\n{pad}            ),"
            ));
            out.push(format!("{pad}            span: Span {{\n{pad}                file: FileId(\n{pad}                    {file},\n{pad}                ),\n{pad}                lo: {lo},\n{pad}                hi: {hi},\n{pad}            }},"));
            out.push(format!("{pad}        }},"));
        }
        out.push(format!("{pad}    ],"));
        out.push(format!("{pad}),"));
        i = j;
    }
    // Re-split, as the pushed entries hold line breaks.
    out.join("\n").lines().collect::<Vec<_>>().join("\n")
        + if hir.ends_with('\n') { "\n" } else { "" }
}

#[test]
fn tables_of_every_number_type_are_one_const_array() {
    for t in [
        "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "f32", "f64", "number",
    ] {
        let src = format!(
            "const T: {t}[] = {};\nfunction main() {{ console.log(T[3], T.length); }}",
            array(24, |i| match i % 3 {
                0 => format!("{}", i * 5),
                1 => format!("0x{:x}", i),
                _ => format!("0b{:b}", i),
            })
        );
        let (diags, packed) = same(&src);
        assert!(diags.is_empty(), "{t}: {diags}");
        assert!(packed >= 1, "{t}: not packed");
    }
}

#[test]
fn float_tables_and_local_arrays_are_one_const_array() {
    let src = format!(
        "function main() {{\n  const a: f64[] = {};\n  let b: u8[] = {};\n  b.push(1);\n  console.log(a[1], b.length);\n}}",
        array(20, |i| format!("{i}.25")),
        array(16, |i| (i * 15).to_string()),
    );
    let (diags, packed) = same(&src);
    assert!(diags.is_empty(), "{diags}");
    assert!(packed >= 2, "{packed}");
}

#[test]
fn an_element_out_of_range_is_reported_where_it_is() {
    // The messages and positions of the checker before #2421 (`velt check` of the same files).
    let src = format!(
        "const T: u8[] = {};\nconsole.log(T.length);\n",
        with(7, "256")
    );
    let (diags, _) = same(&src);
    assert!(
        diags.contains("main.velt:1:39: error: literal out of range for `u8`"),
        "{diags}"
    );
    assert!(diags
        .contains("the literal `256` does not fit into the type `u8` whose range is `0..=255`"));
    let src = format!(
        "function main() {{\n  const t: i8[] = {};\n  console.log(t.length);\n}}\n",
        with(3, "-129")
    );
    let (diags, _) = same(&src);
    assert!(
        diags.contains("main.velt:2:30: error: literal out of range for `i8`"),
        "{diags}"
    );
    let src = format!(
        "const T: u32[] = {};\nconsole.log(T.length);\n",
        with(5, "2.5")
    );
    let (diags, _) = same(&src);
    assert!(
        diags.contains("main.velt:1:34: error: mismatched types"),
        "{diags}"
    );
    let src = format!(
        "const T: u16[] = {};\nconsole.log(T.length);\n",
        with(9, "-1")
    );
    let (diags, _) = same(&src);
    assert!(
        diags.contains("main.velt:1:46: error: cannot apply unary operator `-` to type `u16`"),
        "{diags}"
    );
    // The last element, and one past `u64`'s range in an `i64` table.
    let src = format!(
        "const T: u8[] = {};\nconsole.log(T.length);\n",
        with(19, "300")
    );
    assert!(same(&src).0.contains("literal out of range for `u8`"));
    let src = format!(
        "const T: i64[] = {};\nconsole.log(T.length);\n",
        with(0, "9223372036854775808")
    );
    assert!(same(&src).0.contains("literal out of range for `i64`"));
}

#[test]
fn negated_elements_check_as_before() {
    let src = format!(
        "const T: i32[] = {};\nconst F: f64[] = {};\nfunction main() {{ console.log(T[0], F[1]); }}",
        array(20, |i| format!("-{i}")),
        array(20, |i| if i % 2 == 0 { format!("-{i}.5") } else { format!("{i}") }),
    );
    let (diags, _) = same(&src);
    assert!(diags.is_empty(), "{diags}");
}

#[test]
fn every_other_context_checks_the_array_literal_as_before() {
    let a = array(20, |i| i.to_string());
    let neg = array(20, |i| if i == 4 { "-4".into() } else { i.to_string() });
    let f = array(17, |i| format!("{i}.5"));
    let contexts = [
        // Inferred, and widened later.
        format!("function main() {{ const t = {a}; const u: number[] = t; console.log(u.length); }}"),
        format!("function main() {{ let t = {f}; t.push(1); console.log(t); }}"),
        format!("function main() {{ const t: any[] = {a}; console.log(t.length); }}"),
        format!("function main() {{ const t: (number | string)[] = {a}; console.log(t.length); }}"),
        format!("function main() {{ const t: (u8 | null)[] = {a}; console.log(t.length); }}"),
        format!("function main() {{ const t: u8[] | string = {a}; console.log(t); }}"),
        format!("function main() {{ const t = {a} as const; console.log(t.length); }}"),
        format!("function main() {{ const t = {neg} as u8[]; console.log(t.length); }}"),
        format!("function main() {{ const t: number[] = [...{a}, 1]; console.log(t.length); }}"),
        format!("function main() {{ console.log(Math.max(...{a})); }}"),
        format!("function main() {{ const [x, y, ...r] = {neg}; console.log(x, y, r.length); }}"),
        format!("function main() {{ const [x, y] = {a}; console.log(x, y); }}"),
        format!("function f(xs: Iterable<u16>): void {{ for (const x of xs) console.log(x); }}\nfunction main() {{ f({a}); }}"),
        format!("function f(xs: readonly i32[]): i32 {{ return xs[0]; }}\nfunction main() {{ console.log(f({neg})); }}"),
        format!("function main() {{ const s = new Set<number>({a}); console.log(s.size); }}"),
        format!("function main() {{ const b = Uint8Array.from({a}); console.log(b.length); }}"),
        format!("function main() {{ for (const x of {f}) console.log(x); }}"),
        format!("function main() {{ const m = [{a}, {a}]; console.log(m.length); }}"),
        format!("function main() {{ const m: u8[][] = [{a}, {a}]; console.log(m.length); }}"),
        format!("function main() {{ const o = {{ xs: {a} }}; console.log(o.xs.length); }}"),
        format!("function main() {{ const x: f16[] = {f}; console.log(x[0]); }}"),
        format!("function main() {{ const x: string[] = {a}; console.log(x); }}"),
        format!("function main() {{ console.log(JSON.stringify({f})); }}"),
        format!("function main() {{ console.log({a}.map((x) => x * 2).length); }}"),
        format!("class C {{ static readonly T: u8[] = {a}; }}\nfunction main() {{ console.log(C.T[3]); }}"),
        format!("const T: number[] = {neg};\nfunction main() {{ console.log(T.indexOf(4)); }}"),
        // An argument of a tuple's method, which runs nothing (`tuple_text`).
        format!("function main() {{ const t: [number, number] = [1, 2]; const c = t.concat({a}); console.log(c.length); }}"),
        // A literal passed for a type parameter an annotated arrow parameter names (`arrow_defaults`).
        format!("function g<T>(x: T, f: (v: T) => T): T {{ return f(x); }}\nfunction main() {{ const r = g({a}, (v: u8[]) => v); console.log(r.length); }}"),
        format!("function main() {{ const t = {a} as number[]; console.log(t.length); }}"),
    ];
    for src in &contexts {
        same(src);
    }
}

#[test]
fn the_editor_sees_every_element_of_a_packed_array() {
    // The editor's analysis takes the ordinary path: each element is an expression it records.
    let src = format!(
        "const T: u8[] = {};\nfunction main() {{ console.log(T[0]); }}\n",
        array(20, |i| (i + 100).to_string())
    );
    let l = load_src(&src);
    let file = l.modules[l.root].file;
    let a = velt_sema::ide::check_for_ide(&l.modules, l.root);
    let at = src.find("113").expect("element") as u32;
    assert_eq!(a.type_at(file, at).as_deref(), Some("u8"));
}

/// `unpacked` puts the flag back when `f` returns, inside a nested call, and when `f` panics, so a
/// failed test doesn't leave the next test on the thread unpacked.
#[test]
fn unpacked_restores_the_flag() {
    assert!(!is_unpacked());
    unpacked(|| {
        unpacked(|| assert!(is_unpacked()));
        assert!(is_unpacked(), "a nested call cleared the outer one's flag");
    });
    assert!(!is_unpacked());
    let r = std::panic::catch_unwind(|| unpacked(|| panic!("a failing check")));
    assert!(r.is_err());
    assert!(!is_unpacked(), "a panic left the flag set");
}
