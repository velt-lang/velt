// Node twin of promise_race_async_call_timer.velt: the same program with its types removed.
// `Promise.race` of an async function's promise and a timer's: the async call settles first.
// On wasm32-wasip1 the race trapped (indirect call type mismatch) under the runtime's executor
// (#3919); the executor in Velt runs it there (#3905).
async function double(n) {
  return n * 2;
}

function later(ms, v) {
  return new Promise((r) => setTimeout(() => r(v), ms));
}

async function main() {
  console.log(await Promise.race([double(1), later(50, 9)]));
}

main();
