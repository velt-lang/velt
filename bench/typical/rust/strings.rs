fn main() {
    let mut lines: Vec<String> = Vec::new();
    for i in 0..200000i64 {
        lines.push(format!("{},user{},{},{}", i, i % 1000, (i * 37) % 10007, if i % 2 == 0 { "yes" } else { "no" }));
    }
    let csv = lines.join("\n");
    let (mut sum, mut yes, mut name_len) = (0i64, 0i64, 0usize);
    for _ in 0..5 {
        let rows: Vec<String> = csv.split('\n').map(|s| s.to_string()).collect();
        for row in &rows {
            let cols: Vec<String> = row.split(',').map(|s| s.to_string()).collect();
            sum += cols[2].parse::<i64>().unwrap();
            if cols[3] == "yes" { yes += 1; }
            name_len += cols[1].to_uppercase().len();
        }
    }
    let mut found = 0;
    for i in 0..2000 {
        if csv.contains(&format!("user{}x", i)) { found += 1; }
        if csv.find(&format!(",{},", i)).is_some() { found += 1; }
    }
    let mut out = String::new();
    for i in 0..100000 { out += &(i % 10).to_string(); }
    let words: Vec<&str> = "the quick brown fox jumps over the lazy dog".split(' ').collect();
    let mut replaced = 0;
    for i in 0..200000 {
        let w = words[i % words.len()];
        let t = w.replacen("o", "0", 1);
        let t = t.trim();
        replaced += t.len() + if t.starts_with('t') { 1 } else { 0 };
    }
    println!("{} {} {} {} {} {} {}", csv.len(), sum, yes, name_len, found, out.len(), replaced);
}
