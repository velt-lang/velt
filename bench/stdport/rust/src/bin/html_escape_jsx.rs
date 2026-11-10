// HTML escaping in a server-rendered page: the same program as bench/stdport/html_escape_jsx.velt,
// on the escaper the runtime ran (crates/velt_rt/src/html.rs before #2196's port, as in
// html_escape.rs) with `'` as `&#39;` and, for the page, as `&#x27;` (react-dom's, as std/jsx
// writes it); the page written into one string, lengths in UTF-16 units as Velt's.
use std::fmt::Write;
use std::hint::black_box;

fn entity(b: u8, apos: &'static str) -> Option<&'static str> {
    match b {
        b'&' => Some("&amp;"),
        b'<' => Some("&lt;"),
        b'>' => Some("&gt;"),
        b'"' => Some("&quot;"),
        b'\'' => Some(apos),
        _ => None,
    }
}

// Appends `s` escaped to `out`: as the runtime did, nothing but a copy when `s` has nothing to
// escape, else the room for the entities reserved once.
fn escape(s: &str, apos: &'static str, out: &mut String) {
    let bytes = s.as_bytes();
    let Some(first) = bytes.iter().position(|&b| entity(b, apos).is_some()) else {
        out.push_str(s);
        return;
    };
    let extra: usize = bytes[first..]
        .iter()
        .filter_map(|&b| entity(b, apos))
        .map(|e| e.len() - 1)
        .sum();
    out.reserve(bytes.len() + extra);
    out.push_str(&s[..first]);
    let mut start = first;
    for (i, &b) in bytes.iter().enumerate().skip(first) {
        if let Some(e) = entity(b, apos) {
            out.push_str(&s[start..i]);
            out.push_str(e);
            start = i + 1;
        }
    }
    out.push_str(&s[start..]);
}

fn escape_html(s: &str) -> String {
    let mut out = String::new();
    escape(s, "&#39;", &mut out);
    out
}

fn page(fortunes: &[(i64, &str)]) -> String {
    let mut out = String::from("<ul>");
    for &(id, message) in fortunes {
        out.push_str("<li title=\"");
        escape(message, "&#x27;", &mut out);
        write!(out, "\">{id}: ").unwrap();
        escape(message, "&#x27;", &mut out);
        out.push_str("</li>");
    }
    out.push_str("</ul>");
    out
}

fn units(s: &str) -> usize {
    s.encode_utf16().count()
}

fn main() {
    let mut fortunes = vec![
        (1, "fortune: No such file or directory"),
        (
            2,
            "A computer scientist is someone who fixes things that aren't broken.",
        ),
        (3, "After enough decimal places, nobody gives a damn."),
        (
            4,
            "A bad random number generator: 1, 1, 1, 1, 1, 4.33e+67, 1, 1, 1",
        ),
        (
            5,
            "A computer program does what you tell it to do, not what you want it to do.",
        ),
        (
            6,
            "Emacs is a nice operating system, but I prefer UNIX. — Tom Christaensen",
        ),
        (7, "Any program that runs right is obsolete."),
        (
            8,
            "A list is only as strong as its weakest link. — Donald Knuth",
        ),
        (9, "Feature: A bug with seniority."),
        (10, "Computers make very fast, very accurate mistakes."),
        (
            11,
            "<script>alert(\"This should not be displayed in a browser alert box.\");</script>",
        ),
        (12, "フレームワークのベンチマーク"),
        (0, "Additional fortune added at request time."),
    ];
    // Every message is in the Basic Multilingual Plane, so UTF-8 order is UTF-16 order.
    fortunes.sort_by(|a, b| a.1.cmp(b.1));
    println!("{}", page(&fortunes));
    let mut total = 0;
    for _ in 0..20_000 {
        total += units(&page(black_box(&fortunes)));
        for &(_, message) in black_box(&fortunes) {
            total += units(&escape_html(message));
        }
    }
    println!("{total}");
}
