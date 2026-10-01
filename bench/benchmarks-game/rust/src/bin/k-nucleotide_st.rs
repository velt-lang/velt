// Single-threaded k-nucleotide: the Rust #7 program (k-nucleotide.rs) with the seven frame
// lengths counted one after another instead of on seven threads.

#[path = "k-nucleotide.rs"]
mod knucleotide;

fn main() {
    knucleotide::calc(std::io::stdin().lock(), false);
}
