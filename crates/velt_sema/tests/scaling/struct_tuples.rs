//! Programs for the struct-tuple check's rows of `instantiation_work_grows_linearly`: its
//! fixed point over generic functions, their dispatched methods and their conversions to `any`.

/// `n` classes implementing a generic interface method, each entering one generic chain at its
/// own link, whose end stores its argument in a struct, and one call through the interface,
/// which runs all of them. The chain's needs reach one more class per round of the fixed point:
/// a round must visit again only the functions whose sources grew, not every function (what
/// each visit reads of them, [`growing_source`] holds).
pub fn implementations(n: usize) -> String {
    let mut out = String::from(
        "struct Held<T> {\n  v: T;\n  n: number;\n}\ninterface M {\n  m<T>(x: T): number;\n}\n",
    );
    let mut news = vec![];
    for i in 0..n {
        let next = match i + 1 < n {
            true => format!("return h{}<T>(x);", i + 1),
            false => "const h: Held<T> = { v: x, n: 1 };\n  return h.n;".to_string(),
        };
        out += &format!(
            "class C{i} implements M {{\n  m<T>(x: T): number {{\n    return h{i}<T>(x);\n  }}\n}}\n\
             function h{i}<T>(x: T): number {{\n  {next}\n}}\n"
        );
        news.push(format!("new C{i}()"));
    }
    out + &format!(
        "function main() {{\n  const ms: M[] = [{}];\n  console.log(ms[0].m<i64>(1));\n}}\n",
        news.join(", ")
    )
}

/// Generic functions that convert what they're given to `any`, each calling the next with two
/// object types around its parameter, the last storing it in a struct's field: the types
/// reaching `f0` double per level (`{ p: { q: T } }`, …), while nothing calls `f0`, so only the
/// check sees them. Only the type parameters a conversion to `any` holds are carried to callers,
/// not the types, so the work is linear.
pub fn any_wrappers(n: usize) -> String {
    let mut out = String::from(
        "struct Held<T> {\n  v: T;\n  n: number;\n}\n\
         function keep<T>(x: T): number {\n  const h: Held<T> = { v: x, n: 1 };\n  return h.n;\n}\n",
    );
    for k in 0..n {
        let body = match k + 1 < n {
            true => format!(
                "return f{j}<{{ p: T }}>({{ p: x }}) + f{j}<{{ q: T }}>({{ q: x }});",
                j = k + 1
            ),
            false => "return keep<T>(x) + (a.v === undefined ? 0 : 1);".to_string(),
        };
        out += &format!(
            "function f{k}<T>(x: T): number {{\n  const a: any = {{ v: x }};\n  {body}\n}}\n"
        );
    }
    out + "function main() {\n  console.log(keep<i64>(1));\n}\n"
}

/// A source whose needs grow one per round, re-read by its caller each round: `s<T1..Tn>` calls
/// `d{k}<Tk>` for each `k`, and `d{k}` calls `d{k+1}`, the last storing its `T` in a struct's
/// field. Each `d{k}` learns its need a round after `d{k+1}` (a reader before its source waits
/// for the next round), so `s` gains one need per round, and `r` reads `s` again each round:
/// only what `s` added since, or all of it each time (quadratic).
pub fn growing_source(n: usize) -> String {
    let mut out = String::from("struct Held<T> {\n  v: T;\n  n: number;\n}\n");
    let us = vec!["U"; n].join(", ");
    let xs = vec!["x"; n].join(", ");
    out += &format!("function r<U>(x: U): number {{\n  return s<{us}>({xs});\n}}\n");
    let ts: Vec<String> = (0..n).map(|k| format!("T{k}")).collect();
    let ps: Vec<String> = (0..n).map(|k| format!("x{k}: T{k}")).collect();
    let calls: String = (0..n).map(|k| format!("  d{k}<T{k}>(x{k});\n")).collect();
    out += &format!(
        "function s<{}>({}): number {{\n{calls}  return 0;\n}}\n",
        ts.join(", "),
        ps.join(", ")
    );
    for k in 0..n {
        let body = match k + 1 < n {
            true => format!("return d{}<T>(x);", k + 1),
            false => "const h: Held<T> = { v: x, n: 1 };\n  return h.n;".to_string(),
        };
        out += &format!("function d{k}<T>(x: T): number {{\n  {body}\n}}\n");
    }
    out + "function main() {\n  console.log(r<i64>(1));\n}\n"
}
