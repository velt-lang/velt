// Edit distance between every pair of 500 generated words (same algorithm as
// edit_distance.vlt): the words as byte vectors, the two-row DP over `usize`.
fn distance(a: &[u8], b: &[u8]) -> usize {
    let m = a.len();
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr: Vec<usize> = vec![0; m + 1];
    for j in 1..=b.len() {
        curr[0] = j;
        let bj = b[j - 1];
        for i in 1..=m {
            let cost = usize::from(a[i - 1] != bj);
            curr[i] = (prev[i] + 1).min(curr[i - 1] + 1).min(prev[i - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

fn main() {
    let letters = b"abcdefgh";
    let mut words: Vec<Vec<u8>> = Vec::new();
    let mut state: i64 = 42;
    for _ in 0..500 {
        state = (state * 16807) % 2147483647;
        let len = 20 + (state % 41);
        let mut s = Vec::new();
        for _ in 0..len {
            state = (state * 16807) % 2147483647;
            s.push(letters[(state % 8) as usize]);
        }
        words.push(s);
    }
    let mut smallest: i64 = -1;
    let mut total: usize = 0;
    for i in 0..words.len() {
        for j in i + 1..words.len() {
            let d = distance(&words[i], &words[j]);
            if smallest < 0 || (d as i64) < smallest {
                smallest = d as i64;
            }
            total += d;
        }
    }
    println!("{smallest} {total}");
}
