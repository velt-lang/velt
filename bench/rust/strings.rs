// Strings (same workload as strings.vlt): `format!` for the template literal, `join`, and a
// byte scan (`charCodeAt` in the Velt/JS versions).
fn main() {
    let mut lines: Vec<String> = Vec::new();
    for i in 0..1_000_000i64 {
        let kind = if i % 3 == 0 { "fizz" } else { "buzz" };
        lines.push(format!("line {}: {} {} ok", i, kind, (i * i) % 1000));
    }
    let text = lines.join("\n");
    let mut digits = 0i64;
    for &c in text.as_bytes() {
        if c.is_ascii_digit() {
            digits += 1;
        }
    }
    println!("{} {} {} {}", lines.len(), text.len(), digits, lines[123456]);
}
