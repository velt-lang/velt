// String methods on long ASCII text: the same program as bench/stdport/string_methods_long.velt,
// on `str`'s methods with JS's positions (../js_str.rs).
use std::fmt::Write;
use std::hint::black_box;

#[path = "../js_str.rs"]
mod js;
use js::{flag, mix};

fn main() {
    let mut text = String::new();
    for i in 0..100 {
        writeln!(
            text,
            "row {i}, the quick brown fox, jumps over the lazy dog"
        )
        .unwrap();
    }
    let mut padded = String::from("  \t");
    for _ in 0..30 {
        padded.push_str("lorem ipsum dolor sit amet consectetur ");
    }
    padded.push_str(" \n\n ");
    let (text, padded) = (black_box(text), black_box(padded));
    let needles = black_box(["lazy dog\nrow 99", "Z", "row 7", "fox, jumps"]);
    let mut sum: i64 = 0;
    for i in 0..10000usize {
        let v = needles[i % 4];
        let ii = i as i64;
        sum = mix(sum, js::index_of(&text, "Z", 0));
        sum = mix(sum, js::index_of(&text, "row 99", 0));
        sum = mix(sum, js::index_of(&text, v, ii % 100));
        sum = mix(sum, js::last_index_of(&text, "row 1,"));
        sum = mix(sum, js::last_index_of(&text, v));
        sum = mix(sum, flag(text.contains("xq")));
        sum = mix(sum, flag(text.contains(v)));
        sum = mix(sum, flag(text.starts_with("row 0")));
        sum = mix(sum, flag(text.ends_with("dog\n")));
        sum = mix(sum, js::len(js::slice(&text, 100, 3000)) as i64);
        sum = mix(
            sum,
            js::char_code_at(js::substring(&text, ii % 4000, i64::MAX), 0),
        );
        sum = mix(sum, text.split('\n').count() as i64);
        sum = mix(sum, text.split(", ").count() as i64);
        sum = mix(sum, js::len(js::trim(&padded)) as i64);
        sum = mix(sum, js::char_code_at(js::trim_start(&padded), 0));
        sum = mix(sum, js::len(js::trim_end(&padded)) as i64);
        sum = mix(sum, js::char_code_at(&text.to_uppercase(), ii % 4000));
        sum = mix(sum, js::len(&padded.to_lowercase()) as i64);
        sum = mix(sum, js::len(&text.replacen("lazy", "sleepy", 1)) as i64);
        sum = mix(sum, js::char_code_at(&text.replace(',', ";"), 5));
        sum = mix(sum, js::len(&js::pad(&padded, 2000, "-", false)) as i64);
        sum = mix(sum, js::len(&"abc ".repeat(i % 500)) as i64);
    }
    println!("{sum}");
}
