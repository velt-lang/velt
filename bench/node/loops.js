// Integer loops: Collatz chain lengths and a nested multiply/modulo loop (same as loops.vlt).
// All intermediate values stay below 2^53, so doubles compute the same integers.
function collatzSteps(start) {
  let n = start;
  let steps = 0;
  while (n !== 1) {
    if (n % 2 === 0) {
      n = n / 2;
    } else {
      n = 3 * n + 1;
    }
    steps++;
  }
  return steps;
}

let best = 0;
let bestStart = 0;
for (let i = 1; i < 1000000; i++) {
  const s = collatzSteps(i);
  if (s > best) {
    best = s;
    bestStart = i;
  }
}
console.log(bestStart, best);

let sum = 0;
for (let i = 0; i < 4000; i++) {
  for (let j = 0; j < 4000; j++) {
    sum = (sum + i * j + (i + j)) % 1000000007;
  }
}
console.log(sum);
