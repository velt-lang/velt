// HTML escaping: the same program as bench/stdport/html_escape.velt, on the escaper the runtime
// ran (crates/velt_rt/src/html.rs before #2196's port), lengths in UTF-16 units as Velt's.
use std::hint::black_box;

fn entity(b: u8) -> Option<&'static [u8]> {
    match b {
        b'&' => Some(b"&amp;"),
        b'<' => Some(b"&lt;"),
        b'>' => Some(b"&gt;"),
        b'"' => Some(b"&quot;"),
        b'\'' => Some(b"&#39;"),
        _ => None,
    }
}

fn escape(s: &str) -> std::borrow::Cow<'_, str> {
    let bytes = s.as_bytes();
    let Some(first) = bytes
        .iter()
        .position(|&b| matches!(b, b'&' | b'<' | b'>' | b'"' | b'\''))
    else {
        return s.into();
    };
    let extra: usize = bytes[first..]
        .iter()
        .filter_map(|&b| entity(b))
        .map(|e| e.len() - 1)
        .sum();
    let mut out = Vec::with_capacity(bytes.len() + extra);
    out.extend_from_slice(&bytes[..first]);
    let mut start = first;
    for (i, &b) in bytes.iter().enumerate().skip(first) {
        if let Some(e) = entity(b) {
            out.extend_from_slice(&bytes[start..i]);
            out.extend_from_slice(e);
            start = i + 1;
        }
    }
    out.extend_from_slice(&bytes[start..]);
    String::from_utf8(out).expect("UTF-8 in, UTF-8 out").into()
}

fn units(s: &str) -> usize {
    s.encode_utf16().count()
}

fn main() {
    let messages = [
        "fortune: No such file or directory",
        "A computer scientist is someone who fixes things that aren't broken.",
        "After enough decimal places, nobody gives a damn.",
        "A bad random number generator: 1, 1, 1, 1, 1, 4.33e+67, 1, 1, 1",
        "A computer program does what you tell it to do, not what you want it to do.",
        "Emacs is a nice operating system, but I prefer UNIX. — Tom Christaensen",
        "Any program that runs right is obsolete.",
        "A list is only as strong as its weakest link. — Donald Knuth",
        "Feature: A bug with seniority.",
        "Computers make very fast, very accurate mistakes.",
        "<script>alert(\"This should not be displayed in a browser alert box.\");</script>",
        "フレームワークのベンチマーク",
    ];
    let mut total = 0;
    for i in 0..300_000 {
        total += units(&escape(black_box(messages[i % 12])));
    }
    let clean = "the quick brown fox jumps over the lazy dog ".repeat(1000);
    let mixed = "a < b && c > d, said \"the 'fox'\" over the lazy dog ".repeat(1000);
    for _ in 0..3000 {
        total += units(&escape(black_box(&clean))) + units(&escape(black_box(&mixed)));
    }
    println!("{total}");
}
