// String methods on short ASCII text: the same program as bench/stdport/string_methods.velt, on
// `str`'s methods with JS's positions (../js_str.rs).
use std::hint::black_box;

#[path = "../js_str.rs"]
mod js;
use js::{flag, mix};

const LINES: [&str; 12] = [
    "GET /api/v1/users?id=42 HTTP/1.1",
    "content-type: text/html; charset=utf-8",
    "alice,30,engineer,stockholm",
    "  /static/css/site.css  ",
    "Accept-Encoding: gzip, br",
    "bob,41,designer,oslo,",
    "POST /login HTTP/1.1",
    "x-request-id: 7f3a9c",
    "\tcarol,25,,berlin\n",
    "/a/b/c/d/e/f/g/h",
    "Host: example.com",
    "dave,33,manager,paris",
];

const NEEDLES: [&str; 8] = ["/", "e", "1.1", ",", "api", "zz", ": ", "o"];

fn main() {
    let lines = black_box(LINES);
    let needles = black_box(NEEDLES);
    let mut sum: i64 = 0;
    for i in 0..200000usize {
        let s = lines[i % 12];
        let v = needles[i % 8];
        sum = mix(sum, js::index_of(s, ",", 0));
        sum = mix(sum, js::index_of(s, ": ", 0));
        sum = mix(sum, js::index_of(s, "HTTP/", 0));
        sum = mix(sum, js::index_of(s, "/", 2));
        sum = mix(sum, js::last_index_of(s, "/"));
        sum = mix(sum, js::last_index_of(s, ","));
        sum = mix(sum, flag(s.contains("xq")));
        sum = mix(sum, flag(s.contains("tml")));
        sum = mix(sum, flag(s.starts_with("GET ")));
        sum = mix(sum, flag(s.starts_with('/')));
        sum = mix(sum, flag(s.ends_with("1.1")));
        sum = mix(sum, flag(s.ends_with(',')));
        sum = mix(sum, js::index_of(s, v, 0));
        sum = mix(sum, js::last_index_of(s, v));
        sum = mix(sum, flag(s.contains(v)));
        sum = mix(sum, flag(s.starts_with(v)));
        sum = mix(sum, flag(s.ends_with(v)));
        sum = mix(sum, js::char_code_at(js::slice(s, 2, 6), 1));
        sum = mix(sum, js::len(js::slice(s, -5, i64::MAX)) as i64);
        sum = mix(sum, js::len(js::substring(s, 4, i64::MAX)) as i64);
        sum = mix(sum, js::char_code_at(js::char_at(s, 3), 0));
        sum = mix(sum, s.split(',').count() as i64);
        sum = mix(sum, s.split(v).count() as i64);
        sum = mix(sum, js::len(js::trim(s)) as i64);
        sum = mix(sum, js::char_code_at(js::trim_start(s), 0));
        sum = mix(sum, js::len(js::trim_end(s)) as i64);
        sum = mix(sum, js::char_code_at(&s.to_uppercase(), 5));
        sum = mix(sum, js::char_code_at(&s.to_lowercase(), 1));
        sum = mix(sum, js::char_code_at(&s.replacen(' ', "_", 1), 4));
        sum = mix(sum, js::len(&s.replace('/', "::")) as i64);
        sum = mix(sum, js::char_code_at(&js::pad(s, 40, ".", true), 2));
        sum = mix(sum, js::len(&js::pad(s, 40, " ", false)) as i64);
        sum = mix(sum, js::len(&v.repeat(i % 5)) as i64);
    }
    println!("{sum}");
}
