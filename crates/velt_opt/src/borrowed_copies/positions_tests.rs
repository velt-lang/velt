//! The pass's view of std/lang/string/positions.velt (`translates`, `slices`) checked against the
//! module's source: every function the pass takes for the read-only translation returns a number,
//! a `bool` or nothing, and every name in `SLICE_ITEMS` is a function there that returns a string.
//! A function renamed or added there fails this test instead of silently joining the translation.

use super::{translates, SLICE_ITEMS};

/// positions.velt's functions as `(name, result type)`, from their one-line signatures.
fn functions() -> Vec<(String, String)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../std/lang/string/positions.velt");
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut out = vec![];
    for line in src.lines() {
        let Some(rest) =
            (line.strip_prefix("export function ")).or_else(|| line.strip_prefix("function "))
        else {
            continue;
        };
        let name = rest
            .split('(')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        let ret = (rest.rsplit_once("):"))
            .map(|(_, r)| r.trim().trim_end_matches('{').trim().to_string())
            .unwrap_or_else(|| panic!("positions.velt: `{name}` has no one-line signature"));
        out.push((name, ret));
    }
    out
}

/// The symbol of positions.velt's function `name`, as the pass sees it.
fn symbol(name: &str) -> String {
    format!("_V3stdP4langP6stringP9positionsN{}{name}", name.len())
}

#[test]
fn the_pass_classifies_every_function_of_positions_velt() {
    let fns = functions();
    assert!(
        fns.len() > 10,
        "positions.velt: only {} functions read",
        fns.len()
    );
    for item in SLICE_ITEMS {
        let ret = (fns.iter().find(|(n, _)| n == item))
            .map(|(_, r)| r.as_str())
            .unwrap_or_else(|| panic!("SLICE_ITEMS names `{item}`, which positions.velt lacks"));
        assert_eq!(ret, "string", "`{item}`");
    }
    let scalar = ["u64", "usize", "i64", "u32", "bool", "void"];
    for (name, ret) in &fns {
        let translation = translates(&symbol(name));
        match ret.as_str() {
            "string" => assert!(
                SLICE_ITEMS.contains(&name.as_str()) && !translation,
                "positions.velt's `{name}` returns a string, but SLICE_ITEMS lacks it"
            ),
            r if scalar.contains(&r) => assert!(
                translation || name == "key1",
                "positions.velt's `{name}` is not taken for the translation"
            ),
            r => panic!("positions.velt's `{name}` returns `{r}`: classify it in borrowed_copies"),
        }
    }
}
