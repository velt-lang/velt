// Node twin of streams_pull_returned.velt.
function none() {
  return undefined;
}

let n = 0;
const s = new ReadableStream({
  pull(c) {
    return Promise.resolve().then(() => {
      n++;
      c.enqueue(n);
      if (n == 3) {
        c.close();
      }
    });
  },
});
const r = s.getReader();
for (;;) {
  const x = await r.read();
  if (x.done) {
    break;
  }
  console.log("method pull read " + x.value + ", pulls " + n);
}

const t = new ReadableStream({
  pull: (c) => {
    return Promise.resolve().then(() => {
      throw new Error("then boom");
    });
  },
});
try {
  await t.getReader().read();
} catch (e) {
  console.log("arrow pull caught " + e.message);
}

const st = new ReadableStream({
  start(c) {
    return Promise.resolve().then(() => {
      throw new Error("start boom");
    });
  },
});
try {
  await st.getReader().read();
} catch (e) {
  console.log("method start caught " + e.message);
}

const cs = new ReadableStream({
  pull(c) {
    c.enqueue(1);
  },
  cancel(reason) {
    return Promise.resolve().then(() => {
      throw new Error("cancel boom");
    });
  },
});
try {
  await cs.cancel("why");
  console.log("cancel resolved");
} catch (e) {
  console.log("method cancel caught " + e.message);
}

let m = 0;
const u = new ReadableStream({
  pull(c) {
    [1].forEach((x) => {
      return none();
    });
    m++;
    if (m > 2) {
      c.close();
    } else {
      c.enqueue(m);
    }
  },
});
const ur = u.getReader();
for (;;) {
  const x = await ur.read();
  if (x.done) {
    break;
  }
  console.log("plain pull read " + x.value);
}
console.log("done");
