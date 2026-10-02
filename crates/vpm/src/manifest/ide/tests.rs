use super::*;

const HEAD: &str = "import type { Package } from \"velt:package\";\n\nexport const pkg: Package = ";

/// `src` with the cursor at `|` (removed): the text and the offset.
fn at(src: &str) -> (String, u32) {
    let offset = src.find('|').expect("a `|` cursor");
    (src.replacen('|', "", 1), offset as u32)
}

/// The labels completed at `|` in `HEAD` + `body`.
fn labels(body: &str) -> Vec<String> {
    let (text, offset) = at(&format!("{HEAD}{body}"));
    completions(&text, offset)
        .into_iter()
        .map(|c| c.label)
        .collect()
}

#[test]
fn top_level_keys_not_yet_written() {
    assert_eq!(
        labels("{ name: \"a\", | }"),
        [
            "version",
            "entry",
            "registry",
            "dependencies",
            "paths",
            "jsx",
            "native"
        ]
    );
    // While typing a key, including when the file does not parse.
    assert!(labels("{ name: \"a\", dep| }").contains(&"dependencies".to_string()));
    assert!(labels("{ name: \"a\", dep|").contains(&"dependencies".to_string()));
    assert!(labels("{\n  name: \"a\",\n  |").contains(&"version".to_string()));
    // Keys after the cursor count as written, also right after it.
    assert!(!labels("{ | , version: \"1.0.0\" }").contains(&"version".to_string()));
    assert!(!labels("{ name: \"a\", |version: \"1.0.0\" }").contains(&"version".to_string()));
}

#[test]
fn a_key_completion_inserts_a_value_placeholder() {
    let (text, offset) = at(&format!("{HEAD}{{ name: \"a\", ver| }}"));
    let c = completions(&text, offset)
        .into_iter()
        .find(|c| c.label == "version")
        .unwrap();
    assert_eq!((c.text.as_str(), c.snippet), ("version: \"$1\"", true));
    assert_eq!(
        &text[c.replace.start as usize..c.replace.end as usize],
        "ver"
    );
    assert_eq!(c.detail, "string");
    assert!(c.doc.contains("semantic version"));
}

#[test]
fn nested_objects_complete_their_own_fields() {
    assert_eq!(labels("{ jsx: { | } }"), ["importSource"]);
    assert_eq!(labels("{ native: { targets: [], | } }"), ["path", "wasm"]);
    assert_eq!(
        labels("{ dependencies: { util: { path: \"../u\", | } } }"),
        ["version"]
    );
    // The keys of `dependencies` and `paths` are the user's.
    assert!(labels("{ dependencies: { | } }").is_empty());
    assert!(labels("{ paths: { | } }").is_empty());
}

#[test]
fn values_from_fixed_sets() {
    assert_eq!(labels("{ native: { wasm: | } }"), ["false", "true"]);
    assert_eq!(labels("{ native: { wasm: f| } }"), ["false", "true"]);
    let targets = labels("{ native: { targets: [\"x86_64-apple-darwin\", |] } }");
    assert!(targets.contains(&"aarch64-apple-darwin".to_string()));
    assert!(
        !targets.contains(&"x86_64-apple-darwin".to_string()),
        "already written"
    );
    // Inside a string, only the contents are replaced.
    let (text, offset) = at(&format!("{HEAD}{{ native: {{ targets: [\"aarch|\"] }} }}"));
    let c = &completions(&text, offset)[0];
    assert_eq!(
        &text[c.replace.start as usize..c.replace.end as usize],
        "aarch"
    );
    assert!(!c.text.starts_with('"'));
    // Free-form values get nothing.
    assert!(labels("{ name: | }").is_empty());
    assert!(labels("{ name: \"a|\" }").is_empty());
}

#[test]
fn nothing_outside_the_manifest_object() {
    let (text, offset) =
        at("import type { | } from \"velt:package\";\nexport const pkg: Package = {};");
    assert!(completions(&text, offset).is_empty());
    let (text, offset) = at(&format!("{HEAD}{{ name: \"a\" }};\n|"));
    assert!(completions(&text, offset).is_empty());
    // Comments and strings with braces do not confuse the scan.
    assert_eq!(
        labels("{ /* { */ name: \"{\", // }\n version: \"1\", jsx: { | } }"),
        ["importSource"]
    );
}

#[test]
fn escapes_before_non_ascii_characters_do_not_break_the_scan() {
    // `\é` once left the tokenizer inside a UTF-8 character (a panic).
    assert_eq!(
        labels("{ name: \"caf\\é\\\u{1F600}\", jsx: { | } }"),
        ["importSource"]
    );
    assert_eq!(labels("{ name: \"\\é|"), Vec::<String>::new());
}

#[test]
fn hover_explains_known_keys() {
    let (text, offset) = at(&format!(
        "{HEAD}{{ name: \"a\", nat|ive: {{ targets: [] }} }}"
    ));
    let h = hover(&text, offset).unwrap();
    assert!(
        h.markdown.starts_with("```velt\nnative?: Native\n```\n"),
        "{}",
        h.markdown
    );
    assert_eq!(
        &text[h.range.start as usize..h.range.end as usize],
        "native"
    );
    let (text, offset) = at(&format!("{HEAD}{{ na|me: \"a\" }}"));
    assert!(hover(&text, offset)
        .unwrap()
        .markdown
        .contains("name: string"));
    let (text, offset) = at(&format!("{HEAD}{{ native: {{ ta|rgets: [] }} }}"));
    assert!(hover(&text, offset)
        .unwrap()
        .markdown
        .contains("targets?: string[]"));
    // A dependency name, a value, an unknown key: nothing (yet).
    for body in [
        "{ dependencies: { ut|il: \"1\" } }",
        "{ name: \"a|b\" }",
        "{ nmae|: \"a\" }",
    ] {
        let (text, offset) = at(&format!("{HEAD}{body}"));
        assert_eq!(hover(&text, offset), None, "{body}");
    }
}
