// Floating point: Mandelbrot iteration counts and midpoint-rule integration (same as floats.vlt).
function mandelbrot(width, height, maxIter) {
  let total = 0;
  for (let py = 0; py < height; py++) {
    for (let px = 0; px < width; px++) {
      const x0 = (px / width) * 3.5 - 2.5;
      const y0 = (py / height) * 2.0 - 1.0;
      let x = 0.0;
      let y = 0.0;
      let i = 0;
      while (x * x + y * y <= 4.0 && i < maxIter) {
        const xt = x * x - y * y + x0;
        y = 2.0 * x * y + y0;
        x = xt;
        i++;
      }
      total += i;
    }
  }
  return total;
}

function integratePi(steps) {
  const h = 1.0 / steps;
  let sum = 0.0;
  for (let k = 0; k < steps; k++) {
    const x = (k + 0.5) * h;
    sum += 4.0 / (1.0 + x * x);
  }
  return sum * h;
}

console.log(mandelbrot(600, 400, 200));
console.log(integratePi(20000000));
