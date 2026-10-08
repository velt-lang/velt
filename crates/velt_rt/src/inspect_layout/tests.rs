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
    (
        "101 numbers group without the more-items entry",
        r#"[ 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, ... 1 more item ]"#,
        r#"[
   0,  1,  2,  3,  4,  5,  6,  7,  8,  9, 10, 11,
  12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
  24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35,
  36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47,
  48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59,
  60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71,
  72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83,
  84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95,
  96, 97, 98, 99,
  ... 1 more item
]"#,
    ),
    (
        "strings left-aligned, then the more-items entry",
        r#"[ 'w', 'wx', 'wxx', 'wxxx', 'wxxxx', 'wxxxxx', 'wxxxxxx', 'w', 'wx', 'wxx', 'wxxx', 'wxxxx', 'wxxxxx', 'wxxxxxx', 'w', 'wx', 'wxx', 'wxxx', 'wxxxx', 'wxxxxx', 'wxxxxxx', 'w', 'wx', 'wxx', 'wxxx', 'wxxxx', 'wxxxxx', 'wxxxxxx', 'w', 'wx', 'wxx', 'wxxx', 'wxxxx', 'wxxxxx', 'wxxxxxx', 'w', 'wx', 'wxx', 'wxxx', 'wxxxx', 'wxxxxx', 'wxxxxxx', 'w', 'wx', 'wxx', 'wxxx', 'wxxxx', 'wxxxxx', 'wxxxxxx', 'w', 'wx', 'wxx', 'wxxx', 'wxxxx', 'wxxxxx', 'wxxxxxx', 'w', 'wx', 'wxx', 'wxxx', 'wxxxx', 'wxxxxx', 'wxxxxxx', 'w', 'wx', 'wxx', 'wxxx', 'wxxxx', 'wxxxxx', 'wxxxxxx', 'w', 'wx', 'wxx', 'wxxx', 'wxxxx', 'wxxxxx', 'wxxxxxx', 'w', 'wx', 'wxx', 'wxxx', 'wxxxx', 'wxxxxx', 'wxxxxxx', 'w', 'wx', 'wxx', 'wxxx', 'wxxxx', 'wxxxxx', 'wxxxxxx', 'w', 'wx', 'wxx', 'wxxx', 'wxxxx', 'wxxxxx', 'wxxxxxx', 'w', 'wx', ... 3 more items ]"#,
        r#"[
  'w',       'wx',      'wxx',     'wxxx',    'wxxxx',   'wxxxxx',
  'wxxxxxx', 'w',       'wx',      'wxx',     'wxxx',    'wxxxx',
  'wxxxxx',  'wxxxxxx', 'w',       'wx',      'wxx',     'wxxx',
  'wxxxx',   'wxxxxx',  'wxxxxxx', 'w',       'wx',      'wxx',
  'wxxx',    'wxxxx',   'wxxxxx',  'wxxxxxx', 'w',       'wx',
  'wxx',     'wxxx',    'wxxxx',   'wxxxxx',  'wxxxxxx', 'w',
  'wx',      'wxx',     'wxxx',    'wxxxx',   'wxxxxx',  'wxxxxxx',
  'w',       'wx',      'wxx',     'wxxx',    'wxxxx',   'wxxxxx',
  'wxxxxxx', 'w',       'wx',      'wxx',     'wxxx',    'wxxxx',
  'wxxxxx',  'wxxxxxx', 'w',       'wx',      'wxx',     'wxxx',
  'wxxxx',   'wxxxxx',  'wxxxxxx', 'w',       'wx',      'wxx',
  'wxxx',    'wxxxx',   'wxxxxx',  'wxxxxxx', 'w',       'wx',
  'wxx',     'wxxx',    'wxxxx',   'wxxxxx',  'wxxxxxx', 'w',
  'wx',      'wxx',     'wxxx',    'wxxxx',   'wxxxxx',  'wxxxxxx',
  'w',       'wx',      'wxx',     'wxxx',    'wxxxx',   'wxxxxx',
  'wxxxxxx', 'w',       'wx',      'wxx',     'wxxx',    'wxxxx',
  'wxxxxx',  'wxxxxxx', 'w',       'wx',
  ... 3 more items
]"#,
    ),
    (
        "long entries one per line with the more-items entry",
        r#"[ 'a fairly long string number 0', 'a fairly long string number 1', 'a fairly long string number 2', 'a fairly long string number 3', 'a fairly long string number 4', 'a fairly long string number 5', 'a fairly long string number 6', 'a fairly long string number 7', 'a fairly long string number 8', 'a fairly long string number 9', 'a fairly long string number 10', 'a fairly long string number 11', 'a fairly long string number 12', 'a fairly long string number 13', 'a fairly long string number 14', 'a fairly long string number 15', 'a fairly long string number 16', 'a fairly long string number 17', 'a fairly long string number 18', 'a fairly long string number 19', 'a fairly long string number 20', 'a fairly long string number 21', 'a fairly long string number 22', 'a fairly long string number 23', 'a fairly long string number 24', 'a fairly long string number 25', 'a fairly long string number 26', 'a fairly long string number 27', 'a fairly long string number 28', 'a fairly long string number 29', 'a fairly long string number 30', 'a fairly long string number 31', 'a fairly long string number 32', 'a fairly long string number 33', 'a fairly long string number 34', 'a fairly long string number 35', 'a fairly long string number 36', 'a fairly long string number 37', 'a fairly long string number 38', 'a fairly long string number 39', 'a fairly long string number 40', 'a fairly long string number 41', 'a fairly long string number 42', 'a fairly long string number 43', 'a fairly long string number 44', 'a fairly long string number 45', 'a fairly long string number 46', 'a fairly long string number 47', 'a fairly long string number 48', 'a fairly long string number 49', 'a fairly long string number 50', 'a fairly long string number 51', 'a fairly long string number 52', 'a fairly long string number 53', 'a fairly long string number 54', 'a fairly long string number 55', 'a fairly long string number 56', 'a fairly long string number 57', 'a fairly long string number 58', 'a fairly long string number 59', 'a fairly long string number 60', 'a fairly long string number 61', 'a fairly long string number 62', 'a fairly long string number 63', 'a fairly long string number 64', 'a fairly long string number 65', 'a fairly long string number 66', 'a fairly long string number 67', 'a fairly long string number 68', 'a fairly long string number 69', 'a fairly long string number 70', 'a fairly long string number 71', 'a fairly long string number 72', 'a fairly long string number 73', 'a fairly long string number 74', 'a fairly long string number 75', 'a fairly long string number 76', 'a fairly long string number 77', 'a fairly long string number 78', 'a fairly long string number 79', 'a fairly long string number 80', 'a fairly long string number 81', 'a fairly long string number 82', 'a fairly long string number 83', 'a fairly long string number 84', 'a fairly long string number 85', 'a fairly long string number 86', 'a fairly long string number 87', 'a fairly long string number 88', 'a fairly long string number 89', 'a fairly long string number 90', 'a fairly long string number 91', 'a fairly long string number 92', 'a fairly long string number 93', 'a fairly long string number 94', 'a fairly long string number 95', 'a fairly long string number 96', 'a fairly long string number 97', 'a fairly long string number 98', 'a fairly long string number 99', ... 2 more items ]"#,
        r#"[
  'a fairly long string number 0',
  'a fairly long string number 1',
  'a fairly long string number 2',
  'a fairly long string number 3',
  'a fairly long string number 4',
  'a fairly long string number 5',
  'a fairly long string number 6',
  'a fairly long string number 7',
  'a fairly long string number 8',
  'a fairly long string number 9',
  'a fairly long string number 10',
  'a fairly long string number 11',
  'a fairly long string number 12',
  'a fairly long string number 13',
  'a fairly long string number 14',
  'a fairly long string number 15',
  'a fairly long string number 16',
  'a fairly long string number 17',
  'a fairly long string number 18',
  'a fairly long string number 19',
  'a fairly long string number 20',
  'a fairly long string number 21',
  'a fairly long string number 22',
  'a fairly long string number 23',
  'a fairly long string number 24',
  'a fairly long string number 25',
  'a fairly long string number 26',
  'a fairly long string number 27',
  'a fairly long string number 28',
  'a fairly long string number 29',
  'a fairly long string number 30',
  'a fairly long string number 31',
  'a fairly long string number 32',
  'a fairly long string number 33',
  'a fairly long string number 34',
  'a fairly long string number 35',
  'a fairly long string number 36',
  'a fairly long string number 37',
  'a fairly long string number 38',
  'a fairly long string number 39',
  'a fairly long string number 40',
  'a fairly long string number 41',
  'a fairly long string number 42',
  'a fairly long string number 43',
  'a fairly long string number 44',
  'a fairly long string number 45',
  'a fairly long string number 46',
  'a fairly long string number 47',
  'a fairly long string number 48',
  'a fairly long string number 49',
  'a fairly long string number 50',
  'a fairly long string number 51',
  'a fairly long string number 52',
  'a fairly long string number 53',
  'a fairly long string number 54',
  'a fairly long string number 55',
  'a fairly long string number 56',
  'a fairly long string number 57',
  'a fairly long string number 58',
  'a fairly long string number 59',
  'a fairly long string number 60',
  'a fairly long string number 61',
  'a fairly long string number 62',
  'a fairly long string number 63',
  'a fairly long string number 64',
  'a fairly long string number 65',
  'a fairly long string number 66',
  'a fairly long string number 67',
  'a fairly long string number 68',
  'a fairly long string number 69',
  'a fairly long string number 70',
  'a fairly long string number 71',
  'a fairly long string number 72',
  'a fairly long string number 73',
  'a fairly long string number 74',
  'a fairly long string number 75',
  'a fairly long string number 76',
  'a fairly long string number 77',
  'a fairly long string number 78',
  'a fairly long string number 79',
  'a fairly long string number 80',
  'a fairly long string number 81',
  'a fairly long string number 82',
  'a fairly long string number 83',
  'a fairly long string number 84',
  'a fairly long string number 85',
  'a fairly long string number 86',
  'a fairly long string number 87',
  'a fairly long string number 88',
  'a fairly long string number 89',
  'a fairly long string number 90',
  'a fairly long string number 91',
  'a fairly long string number 92',
  'a fairly long string number 93',
  'a fairly long string number 94',
  'a fairly long string number 95',
  'a fairly long string number 96',
  'a fairly long string number 97',
  'a fairly long string number 98',
  'a fairly long string number 99',
  ... 2 more items
]"#,
    ),
    (
        "a Map's more-items entry is one more line",
        r#"Map(101) { 0 => 0, 1 => 1, 2 => 2, 3 => 3, 4 => 4, 5 => 5, 6 => 6, 7 => 7, 8 => 8, 9 => 9, 10 => 10, 11 => 11, 12 => 12, 13 => 13, 14 => 14, 15 => 15, 16 => 16, 17 => 17, 18 => 18, 19 => 19, 20 => 20, 21 => 21, 22 => 22, 23 => 23, 24 => 24, 25 => 25, 26 => 26, 27 => 27, 28 => 28, 29 => 29, 30 => 30, 31 => 31, 32 => 32, 33 => 33, 34 => 34, 35 => 35, 36 => 36, 37 => 37, 38 => 38, 39 => 39, 40 => 40, 41 => 41, 42 => 42, 43 => 43, 44 => 44, 45 => 45, 46 => 46, 47 => 47, 48 => 48, 49 => 49, 50 => 50, 51 => 51, 52 => 52, 53 => 53, 54 => 54, 55 => 55, 56 => 56, 57 => 57, 58 => 58, 59 => 59, 60 => 60, 61 => 61, 62 => 62, 63 => 63, 64 => 64, 65 => 65, 66 => 66, 67 => 67, 68 => 68, 69 => 69, 70 => 70, 71 => 71, 72 => 72, 73 => 73, 74 => 74, 75 => 75, 76 => 76, 77 => 77, 78 => 78, 79 => 79, 80 => 80, 81 => 81, 82 => 82, 83 => 83, 84 => 84, 85 => 85, 86 => 86, 87 => 87, 88 => 88, 89 => 89, 90 => 90, 91 => 91, 92 => 92, 93 => 93, 94 => 94, 95 => 95, 96 => 96, 97 => 97, 98 => 98, 99 => 99, ... 1 more item }"#,
        r#"Map(101) {
  0 => 0,
  1 => 1,
  2 => 2,
  3 => 3,
  4 => 4,
  5 => 5,
  6 => 6,
  7 => 7,
  8 => 8,
  9 => 9,
  10 => 10,
  11 => 11,
  12 => 12,
  13 => 13,
  14 => 14,
  15 => 15,
  16 => 16,
  17 => 17,
  18 => 18,
  19 => 19,
  20 => 20,
  21 => 21,
  22 => 22,
  23 => 23,
  24 => 24,
  25 => 25,
  26 => 26,
  27 => 27,
  28 => 28,
  29 => 29,
  30 => 30,
  31 => 31,
  32 => 32,
  33 => 33,
  34 => 34,
  35 => 35,
  36 => 36,
  37 => 37,
  38 => 38,
  39 => 39,
  40 => 40,
  41 => 41,
  42 => 42,
  43 => 43,
  44 => 44,
  45 => 45,
  46 => 46,
  47 => 47,
  48 => 48,
  49 => 49,
  50 => 50,
  51 => 51,
  52 => 52,
  53 => 53,
  54 => 54,
  55 => 55,
  56 => 56,
  57 => 57,
  58 => 58,
  59 => 59,
  60 => 60,
  61 => 61,
  62 => 62,
  63 => 63,
  64 => 64,
  65 => 65,
  66 => 66,
  67 => 67,
  68 => 68,
  69 => 69,
  70 => 70,
  71 => 71,
  72 => 72,
  73 => 73,
  74 => 74,
  75 => 75,
  76 => 76,
  77 => 77,
  78 => 78,
  79 => 79,
  80 => 80,
  81 => 81,
  82 => 82,
  83 => 83,
  84 => 84,
  85 => 85,
  86 => 86,
  87 => 87,
  88 => 88,
  89 => 89,
  90 => 90,
  91 => 91,
  92 => 92,
  93 => 93,
  94 => 94,
  95 => 95,
  96 => 96,
  97 => 97,
  98 => 98,
  99 => 99,
  ... 1 more item
}"#,
    ),
    (
        "short arrays with more items stay grouped",
        r#"{ list: [ 0, 1000, 2000, 3000, 4000, 5000, 6000, 7000, 8000, 9000, 10000, 11000, 12000, 13000, 14000, 15000, 16000, 17000, 18000, 19000, 20000, 21000, 22000, 23000, 24000, 25000, 26000, 27000, 28000, 29000, 30000, 31000, 32000, 33000, 34000, 35000, 36000, 37000, 38000, 39000, 40000, 41000, 42000, 43000, 44000, 45000, 46000, 47000, 48000, 49000, 50000, 51000, 52000, 53000, 54000, 55000, 56000, 57000, 58000, 59000, 60000, 61000, 62000, 63000, 64000, 65000, 66000, 67000, 68000, 69000, 70000, 71000, 72000, 73000, 74000, 75000, 76000, 77000, 78000, 79000, 80000, 81000, 82000, 83000, 84000, 85000, 86000, 87000, 88000, 89000, 90000, 91000, 92000, 93000, 94000, 95000, 96000, 97000, 98000, 99000, ... 50 more items ] }"#,
        r#"{
  list: [
        0,  1000,  2000,  3000,  4000,  5000,  6000,  7000,
     8000,  9000, 10000, 11000, 12000, 13000, 14000, 15000,
    16000, 17000, 18000, 19000, 20000, 21000, 22000, 23000,
    24000, 25000, 26000, 27000, 28000, 29000, 30000, 31000,
    32000, 33000, 34000, 35000, 36000, 37000, 38000, 39000,
    40000, 41000, 42000, 43000, 44000, 45000, 46000, 47000,
    48000, 49000, 50000, 51000, 52000, 53000, 54000, 55000,
    56000, 57000, 58000, 59000, 60000, 61000, 62000, 63000,
    64000, 65000, 66000, 67000, 68000, 69000, 70000, 71000,
    72000, 73000, 74000, 75000, 76000, 77000, 78000, 79000,
    80000, 81000, 82000, 83000, 84000, 85000, 86000, 87000,
    88000, 89000, 90000, 91000, 92000, 93000, 94000, 95000,
    96000, 97000, 98000, 99000,
    ... 50 more items
  ]
}"#,
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
