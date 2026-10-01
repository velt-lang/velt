// Hash maps (same workload as hashmap.vlt).
function intMap(n) {
  const m = new Map();
  let x = 1;
  for (let i = 0; i < n; i++) {
    x = (x * 48271) % 2147483647;
    m.set(x % 2000000, i);
  }
  let hits = 0;
  let sum = 0;
  for (let k = 0; k < n; k++) {
    const v = m.get(k * 2);
    if (v !== undefined) {
      hits++;
      sum += v;
    }
  }
  console.log(m.size, hits, sum);
  return hits;
}

function wordCount(n) {
  const counts = new Map();
  let x = 7;
  for (let i = 0; i < n; i++) {
    x = (x * 48271) % 2147483647;
    const word = `w${x % 50000}`;
    counts.set(word, (counts.get(word) ?? 0) + 1);
  }
  let best = 0;
  let total = 0;
  for (const [, c] of counts) {
    total += c;
    if (c > best) {
      best = c;
    }
  }
  console.log(counts.size, total, best, counts.get("w123") ?? 0);
}

intMap(1000000);
wordCount(1000000);
