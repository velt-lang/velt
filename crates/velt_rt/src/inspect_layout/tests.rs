//! Expected texts are node 22's `util.inspect(v)`; the inputs are the same values printed on one
//! line (`util.inspect(v, { compact: true, breakLength: Infinity })`), as the print glue writes
//! them.

use super::{layout, velt_rt_strbuf_inspect_layout};
use crate::str::VeltStr;

/// (what it shows, one-line text, node's text)
const CASES: &[(&str, &str, &str)] = &[
    (
        "short object stays on one line",
        r#"{ a: 1, b: 'two', c: [ 3, 4 ] }"#,
        r#"{ a: 1, b: 'two', c: [ 3, 4 ] }"#,
    ),
    (
        "nested object over 72 columns",
        r#"{ name: 'velt', version: '0.1.0', tags: [ 'compiler', 'typescript' ], nested: { deep: true } }"#,
        r#"{
  name: 'velt',
  version: '0.1.0',
  tags: [ 'compiler', 'typescript' ],
  nested: { deep: true }
}"#,
    ),
    (
        "seven numbers group into columns",
        r#"[ 1, 2, 3, 4, 5, 6, 7 ]"#,
        r#"[
  1, 2, 3, 4,
  5, 6, 7
]"#,
    ),
    (
        "numbers right-aligned",
        r#"[ 0, 919, 838, 757, 676, 595, 514, 433, 352, 271, 190, 109, 28, 947, 866, 785, 704, 623, 542, 461, 380, 299, 218, 137, 56, 975 ]"#,
        r#"[
    0, 919, 838, 757, 676, 595,
  514, 433, 352, 271, 190, 109,
   28, 947, 866, 785, 704, 623,
  542, 461, 380, 299, 218, 137,
   56, 975
]"#,
    ),
    (
        "strings left-aligned",
        r#"[ 'apple', 'banana', 'cherry', 'date', 'elderberry', 'fig', 'grape', 'honeydew' ]"#,
        r#"[
  'apple',
  'banana',
  'cherry',
  'date',
  'elderberry',
  'fig',
  'grape',
  'honeydew'
]"#,
    ),
    (
        "entries of very different length do not group",
        r#"[ 'a', 'b', 'c', 'd', 'e', 'f', 'a much longer string than all the others put together' ]"#,
        r#"[
  'a',
  'b',
  'c',
  'd',
  'e',
  'f',
  'a much longer string than all the others put together'
]"#,
    ),
    (
        "map and class instances",
        r#"Map(3) { 'origin' => Point { x: 0, y: 0 }, 'far away' => Point { x: 123456.789, y: -98765.4321 }, 'third' => Point { x: 1, y: 2 } }"#,
        r#"Map(3) {
  'origin' => Point { x: 0, y: 0 },
  'far away' => Point { x: 123456.789, y: -98765.4321 },
  'third' => Point { x: 1, y: 2 }
}"#,
    ),
    (
        "a cycle keeps its ref marker",
        r#"<ref *1> Tree { name: 'root', parent: null, children: [ Tree { name: 'kid', parent: [Circular *1], children: [] } ] }"#,
        r#"<ref *1> Tree {
  name: 'root',
  parent: null,
  children: [ Tree { name: 'kid', parent: [Circular *1], children: [] } ]
}"#,
    ),
    (
        "long string splits at line breaks",
        r#"{ text: 'first line\nsecond line that is quite a bit longer than the first one\nthird line' }"#,
        r#"{
  text: 'first line\n' +
    'second line that is quite a bit longer than the first one\n' +
    'third line'
}"#,
    ),
    (
        "wide characters count two columns",
        r#"[ '日本語', '中文', '한국어', 'abc', 'défini', 'x', 'y', 'z' ]"#,
        r#"[
  '日本語', '中文',
  '한국어', 'abc',
  'défini', 'x',
  'y',      'z'
]"#,
    ),
    (
        "array of objects",
        r#"[ { id: 1, label: 'one' }, { id: 2, label: 'two' }, { id: 3, label: 'three' }, { id: 4, label: 'four' } ]"#,
        r#"[
  { id: 1, label: 'one' },
  { id: 2, label: 'two' },
  { id: 3, label: 'three' },
  { id: 4, label: 'four' }
]"#,
    ),
];

#[test]
fn breaks_lines_like_node() {
    for (what, line, want) in CASES {
        let got = layout(line.as_bytes()).unwrap_or_else(|| panic!("{what}: does not parse"));
        assert_eq!(String::from_utf8(got).unwrap(), *want, "{what}");
    }
}

#[test]
fn rewrites_only_the_value_in_the_builder() {
    let (line, want) = (CASES[2].1, CASES[2].2);
    unsafe {
        let mut b = VeltStr::from_bytes(format!("label: {line}").as_bytes());
        velt_rt_strbuf_inspect_layout(&mut b, 7);
        assert_eq!(b.as_bytes(), format!("label: {want}").as_bytes());
        b.release();
        // A short value is left alone without being parsed.
        let mut b = VeltStr::from_bytes(b"{ a: 1, b: [ 2 ] }");
        velt_rt_strbuf_inspect_layout(&mut b, 0);
        assert_eq!(b.as_bytes(), b"{ a: 1, b: [ 2 ] }");
        b.release();
    }
}

#[test]
fn other_text_is_printed_as_is() {
    // Unbalanced or unterminated text is not the glue's: no layout.
    for text in ["{ a: 1", "[ 'x ]", "Ok(1))", "[Circular *1"] {
        assert!(layout(text.as_bytes()).is_none(), "{text}");
    }
    // Velt's own forms (`Ok(…)`, enum variants) keep their entries together.
    let long = "Ok({ first: 'aaaaaaaaaaaaaaaa', second: 'bbbbbbbbbbbbbbbb', third: E.V(1, 2) })";
    let want =
        "Ok({\n  first: 'aaaaaaaaaaaaaaaa',\n  second: 'bbbbbbbbbbbbbbbb',\n  third: E.V(1, 2)\n})";
    assert_eq!(layout(long.as_bytes()).unwrap(), want.as_bytes());
}
