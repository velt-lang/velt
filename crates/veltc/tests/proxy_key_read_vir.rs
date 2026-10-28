//! `s[k]` in a generic function, with `k` a union of keys and `s` a proxy typed as its target
//! (velt_vir `key_read`): each key's arm reads its field through the `get` trap into a slot
//! that owns the answer. The slot must be dropped in its own arm: dropped after the arms join,
//! it was garbage on the arms that never set it (a string key's slot dropped when the number
//! key was read), which crashed on WebAssembly, where that stack memory is not zero.
//!
//! The check: in the VIR of `get`, every string the function drops (`velt_rt_str_drop(&x)`) is
//! assigned on every path from the entry to the drop.

mod no_window;
mod test_dir;

use std::collections::{HashMap, HashSet};

const SRC: &str = "function get<T extends object, K extends keyof T>(s: T, k: K): T[K] {
  return s[k];
}
type State = { count: number; name: string; on: boolean };
const s: State = new Proxy({ count: 1, name: \"a\", on: false }, {
  get(t, k, r) {
    return Reflect.get(t, k, r);
  },
});
const keys: (\"count\" | \"name\")[] = [\"count\", \"name\"];
for (const k of keys) {
  console.log(k, get(s, k));
}
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

/// The local a line assigns as a whole (`_19 = ...`).
fn assigned(line: &str) -> Option<&str> {
    let (lhs, _) = line.split_once(" = ")?;
    (lhs.starts_with('_') && lhs[1..].chars().all(|c| c.is_ascii_digit())).then_some(lhs)
}

/// Whether `local` is assigned on every path from `bb0` to the start of `target`.
fn assigned_on_every_path(bs: &[(String, Vec<String>)], local: &str, target: &str) -> bool {
    let assigns: HashSet<&str> = bs
        .iter()
        .filter(|(_, ls)| ls.iter().any(|l| assigned(l) == Some(local)))
        .map(|(n, _)| n.as_str())
        .collect();
    let succ: HashMap<&str, Vec<String>> = bs
        .iter()
        .map(|(n, ls)| {
            (
                n.as_str(),
                ls.last().map(|t| successors(t)).unwrap_or_default(),
            )
        })
        .collect();
    // Reach `target` from the entry without passing a block that assigns the local.
    let mut seen = HashSet::new();
    let mut work = vec!["bb0".to_string()];
    while let Some(b) = work.pop() {
        if b == target {
            return false;
        }
        if !seen.insert(b.clone()) || assigns.contains(b.as_str()) {
            continue;
        }
        work.extend(succ.get(b.as_str()).cloned().unwrap_or_default());
    }
    true
}

#[test]
fn a_proxy_answer_for_one_key_is_dropped_in_that_keys_arm() {
    let v = vir(SRC);
    let start = v
        .lines()
        .position(|l| l.starts_with("fn#") && l.contains("_V3get_"))
        .unwrap_or_else(|| panic!("no `get` in\n{v}"));
    let body: String = v
        .lines()
        .skip(start)
        .take_while(|l| *l != "}")
        .collect::<Vec<_>>()
        .join("\n");
    let bs = blocks(&body);
    let mut drops = 0;
    for (name, lines) in &bs {
        for (i, line) in lines.iter().enumerate() {
            let Some(arg) = line
                .split_once("velt_rt_str_drop(")
                .and_then(|(_, r)| r.split_once(')'))
                .map(|(a, _)| a)
            else {
                continue;
            };
            // `_37 = &_19` before the drop, in its block or the one that branched to it.
            let addr = format!("{arg} = &");
            let local = bs
                .iter()
                .flat_map(|(_, ls)| ls.iter())
                .find_map(|l| l.strip_prefix(&addr))
                .unwrap_or_else(|| panic!("no address for {arg} in\n{body}"));
            drops += 1;
            let here = lines[..i].iter().any(|l| assigned(l) == Some(local));
            assert!(
                here || assigned_on_every_path(&bs, local, name),
                "{local} is dropped in {name} but not set on every path there:\n{body}"
            );
        }
    }
    assert!(drops > 0, "no string drop in `get`:\n{body}");
}
