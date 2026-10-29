//! An `any` converted to a function type (sema `exact_or_forwarding`): the function held is
//! used as it is when it is of exactly that type, and only another one gets the closure that
//! calls it through `any` (#379). Building that closure every time cost each call through the
//! conversion a heap environment and a call through `any`, and lost the function's identity.
//!
//! The check: in the VIR of `main`, the call of the converted function is reachable from the
//! entry without passing the block that allocates the closure's environment.

mod no_window;
mod test_dir;

use std::collections::{HashMap, HashSet};

const SRC: &str = "type L = (n: number) => number;
const l1: L = (n) => n + 1;
let sum = 0;
const held: any = l1;
for (let i = 0; i < 10; i++) {
  const f = held as L;
  sum += f(i);
}
console.log(sum);
";

fn vir(src: &str) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.vlt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.vlt", "--emit", "vir"])
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

/// A function's blocks: name → its statement lines (the terminator last).
fn blocks(body: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = vec![];
    for line in body.lines() {
        let t = line.trim();
        if let Some(name) = t.strip_suffix(':').filter(|n| n.starts_with("bb")) {
            out.push((name.to_string(), vec![]));
        } else if let Some((_, lines)) = out.last_mut() {
            lines.push(t.to_string());
        }
    }
    out
}

/// The `bbN` names a terminator line jumps to.
fn successors(term: &str) -> Vec<String> {
    term.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.starts_with("bb") && w[2..].chars().all(|c| c.is_ascii_digit()))
        .map(str::to_string)
        .collect()
}

#[test]
fn a_function_of_exactly_the_type_is_called_without_the_closure() {
    let v = vir(SRC);
    let start = v
        .lines()
        .position(|l| l.starts_with("fn#") && l.contains("_V4main()"))
        .unwrap_or_else(|| panic!("no `main` in\n{v}"));
    let body: String = v
        .lines()
        .skip(start)
        .take_while(|l| *l != "}")
        .collect::<Vec<_>>()
        .join("\n");
    let bs = blocks(&body);
    let has = |pat: &str| -> Vec<String> {
        bs.iter()
            .filter(|(_, ls)| ls.iter().any(|l| l.contains(pat)))
            .map(|(n, _)| n.clone())
            .collect()
    };
    // The closure's environment: header (drop, clone, transfer) and the `any` it calls.
    let envs: HashSet<String> = has("velt_rt_alloc(48_u64").into_iter().collect();
    assert!(
        !envs.is_empty(),
        "no closure environment in `main`:\n{body}"
    );
    // The call of the converted function, `f(i)`, in the loop.
    let calls = has("fn(ptr, f64) -> f64)");
    let call = calls
        .iter()
        .find(|b| !envs.contains(*b))
        .unwrap_or_else(|| panic!("no indirect call in `main`:\n{body}"));
    let succ: HashMap<&str, Vec<String>> = bs
        .iter()
        .map(|(n, ls)| {
            let s = ls.last().map(|t| successors(t)).unwrap_or_default();
            (n.as_str(), s)
        })
        .collect();
    let mut seen = HashSet::new();
    let mut work = vec!["bb0".to_string()];
    let mut reached = false;
    while let Some(b) = work.pop() {
        if &b == call {
            reached = true;
            break;
        }
        if !seen.insert(b.clone()) || envs.contains(&b) {
            continue;
        }
        work.extend(succ.get(b.as_str()).cloned().unwrap_or_default());
    }
    assert!(
        reached,
        "the call in {call} is reached only through the closure's environment:\n{body}"
    );
}
