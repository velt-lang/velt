// Node twin of performance_observer.velt: the same program with its types removed.
function report(f) {
  try {
    f();
  } catch (e) {
    console.log(e.name + ": " + e.message);
  }
}

function turn() {
  return new Promise((resolve) => setImmediate(() => resolve()));
}

async function main() {
  performance.mark("early");
  const obs = new PerformanceObserver((list, o) => {
    const names = list.getEntries().map((e) => e.name);
    const marks = list.getEntriesByType("mark").length;
    console.log("cb", names, marks, list.getEntriesByName("m3").length, o === obs);
  });
  obs.observe({ type: "mark", buffered: true });
  performance.mark("m1");
  performance.mark("m2");
  console.log("sync", obs.takeRecords().map((e) => e.name));
  performance.mark("m3");
  performance.measure("not observed");
  Promise.resolve().then(() => console.log("micro"));
  await turn();
  console.log("next turn");
  performance.mark("m4");
  obs.disconnect();
  performance.mark("m5");
  await turn();
  const o2 = new PerformanceObserver((l) => {
    console.log("o2", l.getEntries().map((e) => e.entryType + ":" + e.name));
  });
  o2.observe({ entryTypes: ["mark", "measure", "bogus"] });
  performance.mark("m6");
  performance.measure("me", "m1", "m6");
  await turn();
  o2.disconnect();
  performance.mark("after");
  await turn();

  const o3 = new PerformanceObserver((l) => console.log("o3 never"));
  report(() => o3.observe({}));
  report(() => o3.observe({ type: "mark", entryTypes: ["mark"] }));
  o3.observe({ type: "bogus" });
  console.log("bogus ok");
  const o4 = new PerformanceObserver((l) => console.log("o4 never"));
  o4.observe({ type: "mark" });
  o4.observe({ type: "measure" });
  report(() => o4.observe({ entryTypes: ["mark"] }));
  o4.disconnect();
  const o5 = new PerformanceObserver((l) => console.log("o5 never"));
  o5.observe({ entryTypes: ["mark"] });
  report(() => o5.observe({ type: "mark" }));
  o5.disconnect();
  performance.mark("unseen");
  await turn();
  console.log("end");
}

main();
