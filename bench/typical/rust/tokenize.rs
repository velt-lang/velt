// Same algorithm on &str with byte access; tokens are owned Strings like JS slices.
fn is_digit(c: u8) -> bool { c.is_ascii_digit() }
fn is_alpha(c: u8) -> bool { c.is_ascii_alphabetic() || c == b'_' }
fn tokenize(src: &str) -> Vec<String> {
    let b = src.as_bytes(); let mut out = Vec::new(); let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b' ' || c == b'\n' { i += 1; }
        else if is_digit(c) { let s = i; while i < b.len() && is_digit(b[i]) { i += 1; } out.push(src[s..i].to_string()); }
        else if is_alpha(c) { let s = i; while i < b.len() && (is_alpha(b[i]) || is_digit(b[i])) { i += 1; } out.push(src[s..i].to_string()); }
        else { out.push(src[i..i + 1].to_string()); i += 1; }
    }
    out
}
fn main() {
    let mut parts = Vec::new();
    for i in 0..20000 { parts.push(format!("let x{} = foo({}, bar_{}) + {};\nif (x{} > 10) {{ y = x{}; }}\n", i, i, i % 10, i * 3, i, i)); }
    let src = parts.join("");
    let (mut count, mut idents) = (0, 0);
    for _ in 0..5 { let toks = tokenize(&src); count += toks.len(); for t in &toks { if t == "let" || t == "if" { idents += 1; } } }
    let codes: u64 = src.bytes().map(|b| b as u64).sum();
    println!("{} {} {} {}", src.len(), count, idents, codes);
}
