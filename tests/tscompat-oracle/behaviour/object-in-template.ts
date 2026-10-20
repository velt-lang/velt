// Velt writes a promise in a template literal as `console.log` does; JavaScript calls its
// `toString()`, which gives `[object Promise]`.

export async function main() {
  const p = Promise.resolve(42);
  console.log(`got: ${p}`);
  console.log(await p);
}
