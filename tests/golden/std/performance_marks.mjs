// Node twin of performance_marks.velt: the same program with its types removed.
function report(f) {
  try {
    f();
  } catch (e) {
    console.log(e.name + ": " + e.message);
  }
}
const m = performance.mark("a", { startTime: 5, detail: { x: 1 } });
console.log(m);
console.log(JSON.stringify(m), m instanceof PerformanceEntry, m instanceof PerformanceMark);
const m2 = performance.mark("b", { startTime: 10 });
console.log(m2.detail, m2.duration);
const ms = performance.measure("ab", "a", "b");
console.log(ms);
console.log(performance.measure("o", { start: 2, end: 9, detail: "d" }));
console.log(performance.measure("o2", { start: "a", duration: 3 }));
const o3 = performance.measure("o3", { start: 20 });
console.log(o3.startTime, o3.startTime + o3.duration <= performance.now());
console.log(performance.measure("o4", { end: "b", duration: 4 }).startTime);
console.log(performance.measure("o5", "a").startTime);
console.log(performance.measure("o6").startTime);
const d1 = performance.measure("od", { duration: 1 }, "b");
console.log(d1.startTime, d1.duration, d1.detail);
const d2 = performance.measure("odet", { detail: { q: 1 } }, "b");
console.log(d2.startTime, d2.duration, d2.detail);
console.log(performance.getEntries().map((e) => e.name + ":" + e.entryType + ":" + e.startTime));
console.log(performance.getEntriesByType("mark").length, performance.getEntriesByName("a").length, performance.getEntriesByName("a", "measure").length);
console.log(performance.getEntriesByType("resource"), performance.getEntriesByType("bogus"));
performance.clearMarks("a");
console.log(performance.getEntriesByType("mark").map((e) => e.name));
report(() => performance.measure("gone", "a"));
performance.clearMarks();
performance.clearMeasures("ab");
console.log(performance.getEntries().map((e) => e.name));
performance.clearMeasures();
console.log(performance.getEntries().length);
report(() => performance.measure("x", "nope"));
report(() => performance.mark("c", { startTime: -1 }));
report(() => performance.measure("x", { start: 1, end: 2, duration: 1 }));
report(() => performance.measure("x", { start: 1 }, "b"));
report(() => performance.measure("x", { detail: 1 }));
report(() => performance.measure("x", { start: -2 }));
console.log(performance.measure("empty", {}).startTime);
console.log(typeof performance.timeOrigin, performance.timeOrigin > 1.7e12, performance.timeOrigin === performance.timeOrigin);
console.log(Math.abs(performance.timeOrigin + performance.now() - Date.now()) < 50);
console.log(performance.mark("z").detail, performance.mark("y", { detail: undefined }).detail);
const t = performance.now();
console.log(performance.mark("now").startTime >= t, typeof performance.now());
console.log(String(performance));
const own = new PerformanceMark("own", { startTime: 1 });
console.log(own.name, own.startTime, performance.getEntriesByName("own").length);
