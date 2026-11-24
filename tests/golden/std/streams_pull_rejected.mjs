// Node twin of streams_pull_rejected.velt.
class Box {
  v = 1;
}

function untyped() {
  return undefined;
}

function rejects() {
  return Promise.reject(new Error("never widened"));
}

const s = new ReadableStream({
  pull: (c) =>
    Promise.resolve().then(() => {
      throw new Error("pull boom");
    }),
});
try {
  await s.getReader().read();
} catch (e) {
  console.log("pull caught " + e.message);
}

const t = new ReadableStream({
  start: (c) =>
    Promise.resolve().then(() => {
      throw new Error("start boom");
    }),
});
try {
  await t.getReader().read();
} catch (e) {
  console.log("start caught " + e.message);
}

const u = new ReadableStream({
  pull: (c) => {
    c.enqueue(1);
  },
  cancel: (r) =>
    Promise.resolve().then(() => {
      throw new Error("cancel boom");
    }),
});
try {
  await u.cancel("why");
  console.log("cancel resolved");
} catch (e) {
  console.log("cancel caught " + e.message);
}

const p = Promise.resolve().then(() => {
  throw new Error("void");
});
const q = Promise.reject(new Error("number"));
const w = Promise.resolve().then(() => {
  throw new Error("void, its error inferred");
});
const n = rejects();
const t2 = rejects();
const b = rejects();
for (const x of [p, q, w, n, t2, b]) {
  try {
    await x;
  } catch (e) {
    console.log("awaited " + e.message);
  }
}

const v = new ReadableStream({
  pull: (c) => {
    c.enqueue(1);
  },
  start: (c) => untyped(),
});
console.log("any start read " + (await v.getReader().read()).value);
