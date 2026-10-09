// Velt formats an object in a template literal like `console.log`; JavaScript writes
// `[object Object]`.

export function main() {
  const p = { x: 1, y: 2 };
  console.log(`point: ${p}`);
}
