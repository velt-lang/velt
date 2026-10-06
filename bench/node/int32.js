// 32-bit integer mixing on numbers (same as int32.vlt): the issue #521 loop as written, then with
// Math.imul.
function mix32(x, i) {
  let y = (x ^ (x >>> 15)) | 0;
  y = (y * 0x2c1b3c6d) | 0;
  y = (y + i) | 0;
  y = (y ^ (y >>> 12)) | 0;
  y = (y * 0x297a2d39) | 0;
  y = (y ^ (y >>> 15)) | 0;
  return y;
}

function mix32imul(x, i) {
  let y = (x ^ (x >>> 15)) | 0;
  y = Math.imul(y, 0x2c1b3c6d);
  y = (y + i) | 0;
  y = (y ^ (y >>> 12)) | 0;
  y = Math.imul(y, 0x297a2d39);
  y = (y ^ (y >>> 15)) | 0;
  return y;
}

function run(iterations, imul) {
  let checksum = 0;
  for (let sample = 0; sample < 2; sample++) {
    let state = (0x12345678 + sample) | 0;
    let acc = 0;
    if (imul) {
      for (let i = 0; i < iterations; i++) {
        state = mix32imul(state, i);
        acc = (acc + state) | 0;
      }
    } else {
      for (let i = 0; i < iterations; i++) {
        state = mix32(state, i);
        acc = (acc + state) | 0;
      }
    }
    checksum = (checksum + acc) | 0;
  }
  return checksum;
}

console.log(run(50000000, false));
console.log(run(50000000, true));
