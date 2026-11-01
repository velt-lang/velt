// Dynamic parse of a corpus with sonic, `rounds` times; prints the top-level member count.
fn count(text: &str) -> usize {
    use sonic_rs::JsonContainerTrait;
    let v: sonic_rs::Value = sonic_rs::from_str(text).unwrap();
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
