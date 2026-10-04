// A destructuring default replaces `null` in Velt (prints 1) but only `undefined` in JavaScript
// (prints null). The fix reads the property with `??` (#431).

type Pt = { x: number | null };

export function main() {
  const p: Pt = { x: null };
  const { x = 1 } = p;
  console.log(x);
}
