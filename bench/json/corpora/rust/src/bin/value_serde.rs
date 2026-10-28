// Dynamic parse of a corpus with serde, `rounds` times; prints the top-level member count.
fn count(text: &str) -> usize {
    let v: serde_json::Value = serde_json::from_str(text).unwrap();
    v.as_array().map_or_else(|| v.as_object().map_or(0, |o| o.len()), |a| a.len())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let text = std::fs::read_to_string(&args[1]).unwrap();
    let rounds: usize = args[2].parse().unwrap();
    let mut n = 0;
    for _ in 0..rounds {
        n = count(&text);
    }
    println!("{n}");
}
