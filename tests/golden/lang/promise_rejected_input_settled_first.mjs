// The Node twin of tests/golden/lang/promise_rejected_input_settled_first.velt; its .out is what
// Node 22 prints.
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function s()                  {
  await sleep(1);
  return "s";
}

async function main() {
  try {
    await Promise.all([
      Promise.reject        (new Error("a1")),
      Promise.reject        (new Error("a2")),
    ]);
  } catch (e) {
    console.log("all first", e.message);
  }
  try {
    await Promise.all([
      s(),
      Promise.reject        (new Error("b1")),
      Promise.reject        (new Error("b2")),
    ]);
  } catch (e) {
    console.log("all middle", e.message);
  }
  const r1 = await Promise.race([Promise.resolve("ok"), Promise.reject        (new Error("r"))]);
  console.log("race resolved first", r1);
  try {
    await Promise.race([
      Promise.reject        (new Error("r1")),
      Promise.reject        (new Error("r2")),
    ]);
  } catch (e) {
    console.log("race two rejected", e.message);
  }
  const a1 = await Promise.any([Promise.resolve("ok"), Promise.reject        (new Error("y"))]);
  console.log("any resolved first", a1);
  try {
    await Promise.any([
      Promise.reject        (new Error("z1")),
      Promise.reject        (new Error("z2")),
    ]);
  } catch (e) {
    console.log("any all rejected", e.message);
  }
  const st = await Promise.allSettled([
    Promise.reject        (new Error("q1")),
    s(),
    Promise.reject        (new Error("q2")),
  ]);
  console.log("allSettled", st.map((r) => r.status).join(","));
  const st2 = await Promise.allSettled([s(), s(), Promise.reject        (new Error("q3"))]);
  console.log("allSettled late", st2.map((r) => r.status).join(","));
  await sleep(5);
  console.log("done");
}

main();
