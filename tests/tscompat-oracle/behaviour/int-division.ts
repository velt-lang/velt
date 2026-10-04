// A union of integer literal types is an integer type in Velt, so `/` divides integers: Velt
// prints 1, JavaScript 1.5. The fix divides numbers in both.

export function main() {
  const xs: (1 | 3)[] = [3];
  console.log(`${xs[0] / 2}`);
}
