// The Node twin of print_promise_unawaited.velt.

async function double(n) {
  return n * 2;
}

function later(ms, v) {
  return new Promise((resolve) => setTimeout(() => resolve(v), ms));
}

async function main() {
  const r = Promise.race([double(1), double(9)]);
  console.log(r);
  const a = Promise.all([double(2), double(3)]);
  console.log(a);
  const t = later(1, 4);
  console.log(t);
  const any = Promise.any([double(5), double(1)]);
  console.log(any);
  console.log(await r, await a, await t, await any);
  console.log(r, a, t, any);
}

main();
