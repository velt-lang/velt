// Node twin of streams_pull_value.velt.
function maybe(n) {
  if (n > 0) {
    return Promise.reject(new Error("maybe boom"));
  }
  return undefined;
}

const t = new ReadableStream({ pull: (c) => maybe(1) });
try {
  await t.getReader().read();
} catch (e) {
  console.log("optional pull caught " + e.message);
}

const st = new ReadableStream({ start: (c) => maybe(2) });
try {
  await st.getReader().read();
} catch (e) {
  console.log("optional start caught " + e.message);
}

const cs = new ReadableStream({
  pull: (c) => {
    c.enqueue(1);
  },
  cancel: (reason) => maybe(3),
});
try {
  await cs.cancel("why");
  console.log("cancel resolved");
} catch (e) {
  console.log("optional cancel caught " + e.message);
}

let k = 0;
const u = new ReadableStream({
  pull: (c) => {
    k++;
    if (k > 2) {
      c.close();
    } else {
      c.enqueue(k);
    }
    return maybe(0);
  },
});
const r = u.getReader();
for (;;) {
  const x = await r.read();
  if (x.done) {
    break;
  }
  console.log("optional pull read " + x.value);
}
console.log("done");
