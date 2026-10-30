// TypeScript may declare types named like Velt's SIMD types: those are only reachable through
// `velt/simd`, so a declaration here is plain TypeScript and has no finding.
type f32x4 = [number, number, number, number];
class u8x16 {
  n = 1;
}
interface mask8x16 {
  bits: number;
}
const a: f32x4 = [1, 2, 3, 4];
const m: mask8x16 = { bits: 3 };
console.log(a[3], new u8x16().n, m.bits);
