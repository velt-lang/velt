// 1M tasks, each doing trivial work plus one yield to the event loop, then join them all.
// Node has no spawn: a task is a started async call, and yieldNow is a macrotask yield
// (setImmediate), the closest analogue of yielding to a scheduler.
const yieldNow = () => new Promise((resolve) => setImmediate(resolve));

async function work(i) {
  const v = (i * 17 + 3) % 1009;
  await yieldNow();
  return v;
}

async function main() {
  const handles = [];
  for (let i = 0; i < 1000000; i++) {
    handles.push(work(i));
  }
  const rs = await Promise.all(handles);
  let total = 0;
  for (const r of rs) {
    total += r;
  }
  console.log(total);
}
main();
