//! Runtime JSON (`JSON.parseValue` / `JSON.stringify` of `json.Value`).

use velt_rt::json::value::{parse, stringify_into};

/// No panics; a document that parses stringifies to a canonical form that parses back to itself.
pub fn check(data: &[u8]) {
    let Ok(value) = parse(data) else { return };
    let mut first = Vec::new();
    stringify_into(&mut first, &value);
    let reparsed = parse(&first).unwrap_or_else(|e| {
        panic!(
            "stringify output does not parse ({e:?}): {}",
            String::from_utf8_lossy(&first)
        )
    });
    let mut second = Vec::new();
    stringify_into(&mut second, &reparsed);
    assert_eq!(
        String::from_utf8_lossy(&first),
        String::from_utf8_lossy(&second),
        "stringify is not a fixpoint for {}",
        String::from_utf8_lossy(data)
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn round_trips() {
        super::check(br#"{"a":[1,2.5e3,-0,true,null,"x\u00e9\n"],"b":{}}"#);
        super::check(b"[1e400]");
        super::check(b"{");
    }
}
