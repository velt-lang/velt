// Velt writes an error in a template literal as `console.log` does; JavaScript calls its
// `toString()`, which gives `Error: message`.

export function main() {
  const e = new Error("boom");
  console.log(`failed: ${e}`);
}
