//! JavaScript's order of an object's own string keys (ECMA-262 `OrdinaryOwnPropertyKeys`): the
//! names that are canonical array indices (`"0"` to `"4294967294"`, no leading zeros) first, in
//! ascending numeric order, then the others in the order they were created, then the symbol
//! keys. `console.log`, `JSON.stringify` and `Object.keys` list an object's fields in this order
//! (#756); the last two leave the symbol keys out.

/// The array index that `name` is, if it is one: the canonical decimal form of an integer from 0
/// to 2^32 - 2.
pub fn array_index(name: &str) -> Option<u32> {
    let b = name.as_bytes();
    if b.is_empty() || b.len() > 10 || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if b.len() > 1 && b[0] == b'0' {
        return None;
    }
    let n: u64 = name.parse().ok()?;
    (n < u64::from(u32::MAX)).then_some(n as u32)
}

/// How `console.log` shows the member named `name` when a symbol names it (velt_sema's
/// `symbols`): `[Symbol(d)]` for a symbol constant's key name (`[Symbol(d)]`, or `[Symbol(d) #2]`
/// for a second symbol with that description), `[Symbol(Symbol.iterator)]` for a well-known
/// symbol's (`[Symbol.iterator]`). `None` for a string key.
pub fn symbol_key(name: &str) -> Option<String> {
    if let Some(well_known) = name
        .strip_prefix("[Symbol.")
        .and_then(|n| n.strip_suffix(']'))
    {
        return Some(format!("[Symbol(Symbol.{well_known})]"));
    }
    let inner = name.strip_prefix("[Symbol(")?.strip_suffix(']')?;
    let desc = match inner.rfind(") #") {
        Some(i) if inner[i + 3..].bytes().all(|b| b.is_ascii_digit()) && i + 3 < inner.len() => {
            &inner[..i]
        }
        _ => inner.strip_suffix(')')?,
    };
    Some(format!("[Symbol({desc})]"))
}

/// The positions of `names` in JavaScript's key order: array indices first, ascending, then the
/// other strings in their given order, then the symbol keys ([`symbol_key`]) in theirs.
pub fn js_key_order<S: AsRef<str>>(names: &[S]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..names.len()).collect();
    // A stable sort keeps the names of one group in their order.
    order.sort_by_key(|&i| {
        let name = names[i].as_ref();
        match array_index(name) {
            Some(n) => (0, n),
            None if symbol_key(name).is_some() => (2, 0),
            None => (1, 0),
        }
    });
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_indices_are_canonical_and_below_2_to_the_32_minus_1() {
        assert_eq!(array_index("0"), Some(0));
        assert_eq!(array_index("404"), Some(404));
        assert_eq!(array_index("4294967294"), Some(4294967294));
        for not in [
            "",
            "01",
            "-1",
            "1.5",
            "3d",
            "4294967295",
            "99999999999",
            " 1",
            "+1",
        ] {
            assert_eq!(array_index(not), None, "{not:?}");
        }
    }

    #[test]
    fn symbol_keys_come_last_and_show_as_node_shows_them() {
        let names = [
            "[Symbol(a) #2]",
            "b",
            "[Symbol.iterator]",
            "1",
            "[Symbol()]",
        ];
        let order: Vec<&str> = js_key_order(&names).into_iter().map(|i| names[i]).collect();
        assert_eq!(
            order,
            [
                "1",
                "b",
                "[Symbol(a) #2]",
                "[Symbol.iterator]",
                "[Symbol()]"
            ]
        );
        assert_eq!(symbol_key("[Symbol(a) #2]").as_deref(), Some("[Symbol(a)]"));
        assert_eq!(
            symbol_key("[Symbol(x) #y)]").as_deref(),
            Some("[Symbol(x) #y)]")
        );
        assert_eq!(symbol_key("[Symbol()]").as_deref(), Some("[Symbol()]"));
        assert_eq!(
            symbol_key("[Symbol.iterator]").as_deref(),
            Some("[Symbol(Symbol.iterator)]")
        );
        assert_eq!(symbol_key("b"), None);
    }

    #[test]
    fn indices_come_first_ascending_then_the_rest_in_order() {
        let names = ["b", "3d", "1", "01", "a", "0"];
        let order: Vec<&str> = js_key_order(&names).into_iter().map(|i| names[i]).collect();
        assert_eq!(order, ["0", "1", "b", "3d", "01", "a"]);
    }
}
