// Node twin of int_array_number_store.velt: the same stores into typed arrays.
const VALUES = [
  0, 1, 127, 128, 255, 256, 257, -1, -128, -129, 127.9, -0.5, 300.7, 65535, 65536, -32769,
  2147483648, 4294967296, 4294967301, -2147483649, 9007199254740992, 9007199254740994,
  18446744073709551616, 1e300, -1e300, NaN, Infinity, -Infinity, -0,
];
const n = VALUES.length;
const u8s = new Uint8Array(n);
const i8s = new Int8Array(n);
const u16s = new Uint16Array(n);
const i16s = new Int16Array(n);
const u32s = new Uint32Array(n);
const i32s = new Int32Array(n);
for (let i = 0; i < n; i++) {
  const v = VALUES[i];
  u8s[i] = v;
  i8s[i] = v;
  u16s[i] = v;
  i16s[i] = v;
  u32s[i] = v;
  i32s[i] = v;
}
console.log(`u8: ${u8s.join(",")}`);
console.log(`i8: ${i8s.join(",")}`);
console.log(`u16: ${u16s.join(",")}`);
console.log(`i16: ${i16s.join(",")}`);
console.log(`u32: ${u32s.join(",")}`);
console.log(`i32: ${i32s.join(",")}`);

const b = Buffer.alloc(4);
const ten = 10;
const big = 1000.5;
b[0] = 250;
b[0] += ten;
b[1] -= ten;
b[2] = 7;
b[2] *= big;
b[3] = 1;
b[3] |= big;
console.log(`Buffer op=: ${b.join(",")}`);
const s = new Int16Array([32767, -32768]);
const one = 1;
s[0] += one;
s[1] -= one;
console.log(`i16 op=: ${s.join(",")}`);

const nums = [257, -1, 300.7, NaN];
console.log(`new Uint8Array: ${new Uint8Array([257, -1, 300.7]).join(",")}`);
console.log(`Uint8Array.from: ${Uint8Array.from(nums).join(",")}`);
const t = new Uint8Array(4);
t.set(nums);
console.log(`set: ${t.join(",")}`);
