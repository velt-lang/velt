// Recursive Fibonacci: call overhead and integer arithmetic (same algorithm as fib.vlt).
fn fib(n: i64) -> i64 {
    if n < 2 {
        return n;
    }
    fib(n - 1).wrapping_add(fib(n - 2))
}

fn main() {
    println!("{}", fib(35));
}
