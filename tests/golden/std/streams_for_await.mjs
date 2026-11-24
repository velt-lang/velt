// Node twin of streams_for_await.velt.
function ticks(log) {
  let p = Promise.resolve();
  for (let i = 0; i < 20; i++) {
    const k = i;
    p = p.then(() => {
      log.push("t" + k);
    });
  }
  return p;
}

async function loop(name, s) {
  const log = [];
  const t = ticks(log);
  for await (const v of s) {
    log.push("v" + v);
  }
  log.push("end");
  await t;
  console.log(name + ": " + log.join(" "));
}

async function main() {
  await loop(
    "start",
    new ReadableStream({
      start: (c) => {
        c.enqueue(1);
        c.enqueue(2);
        c.enqueue(3);
        c.close();
      },
    }),
  );

  let n = 0;
  await loop(
    "sync pull",
    new ReadableStream({
      pull: (c) => {
        n++;
        if (n > 3) {
          c.close();
        } else {
          c.enqueue(n);
        }
      },
    }),
  );

  let m = 0;
  await loop(
    "async pull",
    new ReadableStream({
      pull: async (c) => {
        m++;
        if (m > 3) {
          c.close();
        } else {
          c.enqueue(m);
        }
      },
    }),
  );

  // Leaving early: the stream is let go at once, before the source's `cancel` runs out and
  // before the loop goes on (#3717).
  const log = [];
  const s = new ReadableStream({
    start: (c) => {
      c.enqueue(1);
      c.enqueue(2);
    },
    cancel: (reason) => {
      log.push("cancel");
    },
  });
  for await (const v of s) {
    log.push("v" + v);
    queueMicrotask(() => {
      log.push("locked " + s.locked);
    });
    break;
  }
  log.push("after locked " + s.locked);
  await null;
  console.log("break: " + log.join(" "));

  // An error after a value: the loop throws it in Node's turn, and the stream is let go.
  const elog = [];
  const et = ticks(elog);
  const es = new ReadableStream({
    start: (c) => {
      c.enqueue(1);
    },
    pull: (c) => {
      c.error(new Error("boom"));
    },
  });
  try {
    for await (const v of es) {
      elog.push("v" + v);
    }
  } catch (e) {
    elog.push(`caught ${e} locked ${es.locked}`);
  }
  await et;
  console.log("error: " + elog.join(" "));

  // A `return()` is the current promise: a second `return()` and a `next()` after it settle
  // once its cancel has.
  const rlog = [];
  const rs = new ReadableStream({
    start: (c) => {
      c.enqueue(1);
    },
    cancel: (reason) => {
      rlog.push("cancel");
    },
  });
  const it = rs[Symbol.asyncIterator]();
  const r1 = it.return();
  const r2 = it.return();
  const r3 = it.next();
  r3.then((r) => {
    rlog.push("next done " + r.done);
  });
  r2.then((r) => {
    rlog.push("return 2");
  });
  r1.then((r) => {
    rlog.push("return 1");
  });
  await Promise.all([r1, r2, r3]);
  console.log("return twice: " + rlog.join(" "));
}

main();
