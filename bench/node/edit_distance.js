// Edit distance between every pair of 500 generated words (same algorithm as
// edit_distance.vlt).
function distance(a, b) {
  const m = a.length;
  const n = b.length;
  let prev = [];
  let curr = [];
  for (let i = 0; i <= m; i++) {
    prev.push(i);
    curr.push(0);
  }
  for (let j = 1; j <= n; j++) {
    curr[0] = j;
    const bj = b[j - 1];
    for (let i = 1; i <= m; i++) {
      const cost = a[i - 1] === bj ? 0 : 1;
      curr[i] = Math.min(prev[i] + 1, curr[i - 1] + 1, prev[i - 1] + cost);
    }
    const tmp = prev;
    prev = curr;
    curr = tmp;
  }
  return prev[m];
}

const letters = "abcdefgh";
const encoder = new TextEncoder();
const words = [];
let state = 42;
for (let w = 0; w < 500; w++) {
  state = (state * 16807) % 2147483647;
  const len = 20 + (state % 41);
  let s = "";
  for (let k = 0; k < len; k++) {
    state = (state * 16807) % 2147483647;
    s += letters[state % 8];
  }
  words.push(encoder.encode(s));
}
let smallest = -1;
let total = 0;
for (let i = 0; i < words.length; i++) {
  for (let j = i + 1; j < words.length; j++) {
    const d = distance(words[i], words[j]);
    if (smallest < 0 || d < smallest) smallest = d;
    total += d;
  }
}
console.log(smallest, total);
