// String methods on text that is not ASCII: the same program as
// bench/stdport/string_methods_wide.velt, on `str`'s methods with JS's positions (../js_str.rs).
use std::hint::black_box;

#[path = "../js_str.rs"]
mod js;
use js::{flag, mix};

const LINES: [&str; 10] = [
    "café,crème brûlée,résumé",
    "  Ångström à la carte, naïve façade ",
    "Москва, Санкт-Петербург, Новосибирск",
    "東京都,大阪府,京都府,北海道",
    "ユーザー: 山田太郎; 年齢: 42",
    "I 😀 Velt, 🚀 fast, 🎉 done",
    "👍🏽 ok 👍🏽 ok 👍🏽",
    "Zürich → Genève → Lausanne, Schweiz",
    "\u{3000}全角スペース付き,テキスト\u{3000}",
    "naïve,coöperate,über,señor,piñata,jalapeño",
];

const NEEDLES: [&str; 8] = [",", "é", "ok", "府", "→", "😀", " ", "ü"];

fn main() {
    let lines = black_box(LINES);
    let needles = black_box(NEEDLES);
    let mut sum: i64 = 0;
    for i in 0..100000usize {
        let s = lines[i % 10];
        let v = needles[i % 8];
        let comma = js::index_of(s, ",", 0);
        sum = mix(sum, comma);
        sum = mix(sum, js::index_of(s, "→", 0));
        sum = mix(sum, js::index_of(s, v, 0));
        sum = mix(sum, js::index_of(s, v, 3));
        sum = mix(sum, js::last_index_of(s, ","));
        sum = mix(sum, js::last_index_of(s, v));
        sum = mix(sum, flag(s.contains("xq")));
        sum = mix(sum, flag(s.contains(v)));
        sum = mix(sum, flag(s.starts_with("café")));
        sum = mix(sum, flag(s.starts_with(v)));
        sum = mix(sum, flag(s.ends_with('府')));
        sum = mix(sum, flag(s.ends_with(v)));
        let cut = comma.max(0);
        sum = mix(sum, js::len(js::slice(s, 0, cut)) as i64);
        sum = mix(sum, js::char_code_at(js::slice(s, cut, i64::MAX), 1));
        let after = if comma < 0 { 0 } else { comma + 1 };
        sum = mix(
            sum,
            js::len(js::substring(s, after, js::len(s) as i64)) as i64,
        );
        sum = mix(sum, js::char_code_at(s, (i % 12) as i64));
        sum = mix(sum, s.split(',').count() as i64);
        sum = mix(sum, s.split(v).count() as i64);
        sum = mix(sum, js::len(js::trim(s)) as i64);
        sum = mix(sum, js::char_code_at(js::trim_end(s), 0));
        sum = mix(sum, js::char_code_at(&s.to_uppercase(), 1));
        sum = mix(sum, js::len(&s.to_lowercase()) as i64);
        sum = mix(sum, js::char_code_at(&s.replacen(',', "、", 1), cut));
        sum = mix(sum, js::len(&s.replace(v, "··")) as i64);
        sum = mix(sum, js::char_code_at(&js::pad(s, 48, "·", true), 3));
        sum = mix(sum, js::len(&v.repeat(i % 5)) as i64);
    }
    println!("{sum}");
}
