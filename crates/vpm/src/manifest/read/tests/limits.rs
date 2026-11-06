//! Hostile input: manifests also come from uploaded archives (the registry server).

use super::*;

#[test]
fn oversized_manifest_is_refused_before_parsing() {
    let src = with(&format!("/*{}*/", "x".repeat(MAX_BYTES)));
    let (message, _) = error(&src);
    assert_eq!(message, "the manifest is larger than 64 KiB");
}

#[test]
fn deep_nesting_is_an_error_not_a_crash() {
    for depth in [300, 30_000] {
        let src = with(&format!(
            "paths: {}{}",
            "[".repeat(depth),
            "]".repeat(depth)
        ));
        assert!(src.len() <= MAX_BYTES);
        assert!(Manifest::read(FILE, &src).is_err());
    }
}

#[test]
fn too_many_values_are_refused() {
    let array = with(&format!("paths: [{}]", "0,".repeat(MAX_VALUES)));
    let many_keys: String = (0..MAX_VALUES / 2).map(|i| format!("k{i}:[0],")).collect();
    let keys = with(&format!("paths: {{ {many_keys} }}"));
    for src in [array, keys] {
        assert!(src.len() <= MAX_BYTES, "{}", src.len());
        let (message, _) = error(&src);
        assert_eq!(message, "the manifest has more than 10000 values");
    }
}

#[test]
fn a_large_array_of_numbers_is_data() {
    // From 16 number literals on, the parser packs an array literal into one node (#2421): it
    // is still the data of its elements, each one a value.
    let elems: Vec<String> = (0..20).map(|i| i.to_string()).collect();
    let src = with(&format!("paths: [{}]", elems.join(", ")));
    let (values, value) = read_data(FILE, &src, &PACKAGE_SHAPE, |r, _| r.values)
        .unwrap_or_else(|d| panic!("refused: {:?}", d[0].message));
    // The object, `name`, `version`, `paths` and its 20 elements.
    assert_eq!(values, 24);
    let ValueKind::Object(fields) = &value.kind else {
        panic!("not an object");
    };
    let ValueKind::Array(paths) = &fields[2].1.kind else {
        panic!("`paths` is not an array");
    };
    assert_eq!(paths.len(), 20);
    assert!(paths.iter().all(|v| matches!(v.kind, ValueKind::Num)));
    assert_eq!(
        &src[paths[19].span.lo as usize..paths[19].span.hi as usize],
        "19"
    );
    // A negated element is not data, packed or not.
    for n in [2, 20] {
        let src = with(&format!("paths: [{}-1]", "0, ".repeat(n - 1)));
        let (message, covered) = error(&src);
        assert!(
            message.starts_with("the manifest is data only"),
            "{message}"
        );
        assert_eq!(covered, "-1");
    }
}
