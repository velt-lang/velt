fn matmul(a: &Vec<Vec<f64>>, b: &Vec<Vec<f64>>, n: usize) -> Vec<Vec<f64>> {
    let mut c = Vec::new();
    for i in 0..n { let mut row = Vec::new(); for j in 0..n { let mut s = 0.0; for k in 0..n { s += a[i][k] * b[k][j]; } row.push(s); } c.push(row); }
    c
}
fn life(g: &Vec<Vec<bool>>, n: i64) -> Vec<Vec<bool>> {
    let mut out = Vec::new();
    for y in 0..n { let mut row = Vec::new(); for x in 0..n {
        let mut c = 0;
        for dy in -1..=1i64 { for dx in -1..=1i64 { if dx == 0 && dy == 0 { continue; }
            let yy = (y + dy + n) % n; let xx = (x + dx + n) % n; if g[yy as usize][xx as usize] { c += 1; } } }
        let cell = g[y as usize][x as usize];
        row.push(if cell { c == 2 || c == 3 } else { c == 3 });
    } out.push(row); }
    out
}
fn main() {
    let n = 300usize;
    let mut a = Vec::new(); let mut b = Vec::new();
    for i in 0..n { let mut ra = Vec::new(); let mut rb = Vec::new();
        for j in 0..n { ra.push(((i * j) % 7) as f64 - 3.0); rb.push(((i + j) % 5) as f64 - 2.0); } a.push(ra); b.push(rb); }
    let c = matmul(&a, &b, n);
    let mut tr = 0.0; for i in 0..n { tr += c[i][i]; }
    let m = 256i64;
    let mut g: Vec<Vec<bool>> = (0..m).map(|y| (0..m).map(|x| (x * 7 + y * 13) % 5 == 0).collect()).collect();
    for _ in 0..100 { g = life(&g, m); }
    let alive: usize = g.iter().map(|r| r.iter().filter(|&&c| c).count()).sum();
    println!("{} {}", tr, alive);
}
