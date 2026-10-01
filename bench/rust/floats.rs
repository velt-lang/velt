// Floating point: Mandelbrot iteration counts and midpoint-rule integration (same as floats.vlt).
fn mandelbrot(width: i64, height: i64, max_iter: i64) -> i64 {
    let mut total = 0i64;
    for py in 0..height {
        for px in 0..width {
            let x0 = (px as f64) / (width as f64) * 3.5 - 2.5;
            let y0 = (py as f64) / (height as f64) * 2.0 - 1.0;
            let (mut x, mut y, mut i) = (0.0f64, 0.0f64, 0i64);
            while x * x + y * y <= 4.0 && i < max_iter {
                let xt = x * x - y * y + x0;
                y = 2.0 * x * y + y0;
                x = xt;
                i += 1;
            }
            total += i;
        }
    }
    total
}

fn integrate_pi(steps: i64) -> f64 {
    let h = 1.0 / (steps as f64);
    let mut sum = 0.0f64;
    for k in 0..steps {
        let x = ((k as f64) + 0.5) * h;
        sum += 4.0 / (1.0 + x * x);
    }
    sum * h
}

fn main() {
    println!("{}", mandelbrot(600, 400, 200));
    println!("{}", integrate_pi(20_000_000));
}
