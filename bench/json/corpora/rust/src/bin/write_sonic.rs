// A corpus parsed once with sonic and written back `rounds` times; prints the
// last text written (run.sh checks that it parses to the same value as Node's).
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let text = std::fs::read_to_string(&args[1]).unwrap();
    let rounds: usize = args[2].parse().unwrap();
    let v: sonic_rs::Value = sonic_rs::from_str(&text).unwrap();
    let mut out = String::new();
    for _ in 0..rounds {
        out = sonic_rs::to_string(&v).unwrap();
    }
    println!("{out}");
}
