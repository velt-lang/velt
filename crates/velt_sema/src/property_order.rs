//! JavaScript's order of an object's own string keys (ECMA-262 `OrdinaryOwnPropertyKeys`): the
//! names that are canonical array indices (`"0"` to `"4294967294"`, no leading zeros) first, in
//! ascending numeric order, then the others in the order they were created. `console.log`,
//! `JSON.stringify` and `Object.keys` list an object's fields in this order (#756).

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
    (n <= u64::from(u32::MAX) - 1).then_some(n as u32)
}

/// The positions of `names` in JavaScript's key order: array indices first, ascending, then the
/// others in their given order.
pub fn js_key_order<S: AsRef<str>>(names: &[S]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..names.len()).collect();
    // A stable sort keeps the non-index names (key `None`, after every index) in place.
    order.sort_by_key(|&i| match array_index(names[i].as_ref()) {
        Some(n) => (0, n),
        None => (1, 0),
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
        for not in ["", "01", "-1", "1.5", "3d", "4294967295", "99999999999", " 1", "+1"] {
            assert_eq!(array_index(not), None, "{not:?}");
        }
    }

    #[test]
    fn indices_come_first_ascending_then_the_rest_in_order() {
        let names = ["b", "3d", "1", "01", "a", "0"];
        let order: Vec<&str> = js_key_order(&names).into_iter().map(|i| names[i]).collect();
        assert_eq!(order, ["0", "1", "b", "3d", "01", "a"]);
    }
}
