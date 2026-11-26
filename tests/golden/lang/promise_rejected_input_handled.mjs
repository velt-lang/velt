// The Node twin of tests/golden/lang/promise_rejected_input_handled.velt; its .out is what
// Node 22 prints.
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function s() {
  await sleep(1);
  return "s";
}

async function late() {
  await sleep(2);
  throw new Error("late");
}

async function main() {
  const settled = await Promise.allSettled([s(), Promise.reject(new Error("no"))]);
  console.log("settled", settled.length, settled[0].status, settled[1].status);
  const both = await Promise.allSettled([late(), Promise.reject(new Error("b"))]);
  console.log("settled again", both.map((r) => r.status).join(","));
  const three = await Promise.allSettled([s(), late(), Promise.reject(new Error("c"))]);
  console.log("settled three", three.map((r) => r.status).join(","));

  const any = await Promise.any([Promise.reject(new Error("a")), s()]);
  console.log("any", any);
  const anyLate = await Promise.any([s(), late()]);
  console.log("any before late", anyLate);

  try {
    await Promise.all([s(), Promise.reject(new Error("all"))]);
  } catch (e) {
    console.log("all caught", e.message);
  }
  await Promise.all([late(), s()]).catch((e) => console.log("all .catch", e.message));

  try {
    await Promise.race([s(), Promise.reject(new Error("race"))]);
  } catch (e) {
    console.log("race caught", e.message);
  }
  const won = await Promise.race([s(), late()]);
  console.log("race won", won);

  await sleep(5);
  console.log("done");
}

main();
