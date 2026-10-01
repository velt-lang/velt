// Sorting (same workload as sort.vlt), with the unstable sort like Velt' `sort()`.
fn checksum(xs: &[i64]) -> i64 {
    let mut h = 0i64;
    for &v in xs {
        h = (h * 31 + v) % 1000000007;
    }
    h
}

fn main() {
    let mut nums: Vec<i64> = Vec::new();
    let mut x = 1i64;
    for _ in 0..1_000_000 {
        x = (x * 48271) % 2147483647;
        nums.push(x % 1000000000);
    }
    nums.sort_unstable();
    println!("{} {} {} {}", nums[0], nums[500000], nums[999999], checksum(&nums));

    let mut words: Vec<String> = Vec::new();
    for _ in 0..200_000 {
        x = (x * 48271) % 2147483647;
        words.push(format!("k{}", x % 10000000));
    }
    words.sort_unstable();
    println!("{} {} {}", words[0], words[100000], words[199999]);
}
