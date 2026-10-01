// Sorting (same workload as sort.vlt). Numbers need a comparator (the default sort is textual).
function checksum(xs) {
  let h = 0;
  for (let i = 0; i < xs.length; i++) {
    h = (h * 31 + xs[i]) % 1000000007;
  }
  return h;
}

const nums = [];
let x = 1;
for (let i = 0; i < 1000000; i++) {
  x = (x * 48271) % 2147483647;
  nums.push(x % 1000000000);
}
nums.sort((a, b) => a - b);
console.log(nums[0], nums[500000], nums[999999], checksum(nums));

const words = [];
for (let i = 0; i < 200000; i++) {
  x = (x * 48271) % 2147483647;
  words.push(`k${x % 10000000}`);
}
words.sort();
console.log(words[0], words[100000], words[199999]);
