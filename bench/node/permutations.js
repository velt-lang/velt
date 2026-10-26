// Permutations (same workload as permutations.vlt): fannkuch-redux's algorithm, n = 9.
function fannkuch(n) {
  const perm = [];
  const perm1 = [];
  const count = [];
  for (let i = 0; i < n; i++) {
    perm.push(0);
    perm1.push(i);
    count.push(0);
  }
  let maxFlips = 0;
  let checksum = 0;
  let permIndex = 0;
  let r = n;
  while (true) {
    while (r !== 1) {
      count[r - 1] = r;
      r--;
    }
    for (let i = 0; i < n; i++) perm[i] = perm1[i];
    let flips = 0;
    let k = perm[0];
    while (k !== 0) {
      let i = 0;
      let j = k;
      while (i < j) {
        const t = perm[i];
        perm[i] = perm[j];
        perm[j] = t;
        i++;
        j--;
      }
      flips++;
      k = perm[0];
    }
    if (flips > maxFlips) maxFlips = flips;
    checksum += permIndex % 2 === 0 ? flips : -flips;
    while (true) {
      if (r === n) return `${checksum}\nPfannkuchen(${n}) = ${maxFlips}`;
      const first = perm1[0];
      for (let i = 0; i < r; i++) perm1[i] = perm1[i + 1];
      perm1[r] = first;
      count[r] = count[r] - 1;
      if (count[r] > 0) break;
      r++;
    }
    permIndex++;
  }
}

console.log(fannkuch(9));
