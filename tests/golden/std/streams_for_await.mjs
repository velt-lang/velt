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

async function loop(name, mk) {
  const log = [];
  const t = ticks(log);
  const s = mk();
  for await (const v of s) {
    log.push("v" + v);
  }
  log.push("end");
  await t;
  console.log(name + ": " + log.join(" "));
}

// A chain of `n` microtasks, each logged.
function chain(log, n) {
  let p = Promise.resolve();
  for (let i = 0; i < n; i++) {
    const k = i;
    p = p.then(() => {
      log.push("t" + k);
    });
  }
  return p;
}

// A pull that settles after `waits` microtasks: the first value waits for it, in Node's turn.
async function slow(name, waits) {
  const log = [];
  const t = chain(log, 30);
  let d = 0;
  const s = new ReadableStream({
    pull: async (c) => {
      for (let i = 0; i < waits; i++) {
        await null;
      }
      d++;
      if (d > 3) {
        c.close();
      } else {
        c.enqueue(d);
      }
    },
  });
  for await (const v of s) {
    log.push("v" + v);
  }
  log.push("end");
  await t;
  console.log(name + ": " + log.join(" "));
}

// Three `next()` calls at once: each waits for the one before it, as Node chains them.
async function concurrent() {
  const log = [];
  const t = chain(log, 16);
  let n = 0;
  const s = new ReadableStream({
    pull: (c) => {
      n++;
      if (n > 3) {
        c.close();
      } else {
        c.enqueue(n);
      }
    },
  });
  const it = s[Symbol.asyncIterator]();
  const a = it.next();
  const b = it.next();
  const c = it.next();
  a.then((r) => {
    log.push("a " + r.value);
  });
  b.then((r) => {
    log.push("b " + r.value);
  });
  c.then((r) => {
    log.push("c " + r.value);
  });
  await Promise.all([a, b, c]);
  await t;
  console.log("concurrent: " + log.join(" "));
}

// Three chunks and a `cancel` that logs, at once or after an `await`.
function three(log, asyncCancel) {
  if (asyncCancel) {
    return new ReadableStream({
      start: (c) => {
        c.enqueue(1);
        c.enqueue(2);
        c.enqueue(3);
      },
      cancel: async (reason) => {
        log.push("cancel");
        await null;
        log.push("done");
      },
    });
  }
  return new ReadableStream({
    start: (c) => {
      c.enqueue(1);
      c.enqueue(2);
      c.enqueue(3);
    },
    cancel: (reason) => {
      log.push("cancel");
    },
  });
}

// Leaving the loop by a throw at value `at`: the code after it runs in Node's turn.
async function throwAt(name, at, asyncCancel) {
  const log = [];
  const t = chain(log, 16);
  const s = three(log, asyncCancel);
  try {
    for await (const v of s) {
      log.push("v" + v);
      if (v == at) {
        throw new Error("x");
      }
    }
  } catch (e) {
    log.push("caught");
  }
  await t;
  console.log(name + ": " + log.join(" "));
}

// Leaving the loop by `break` at value `at`, with an async `cancel`.
async function breakAt(name, at) {
  const log = [];
  const t = chain(log, 16);
  const s = three(log, true);
  for await (const v of s) {
    log.push("v" + v);
    if (v == at) {
      break;
    }
  }
  log.push("after");
  await t;
  console.log(name + ": " + log.join(" "));
}

// `return v` from inside the loop, in an async function the caller awaits.
async function firstOf(s) {
  for await (const v of s) {
    return v;
  }
  return 0;
}

async function returnInside() {
  const log = [];
  const t = chain(log, 16);
  const v = await firstOf(three(log, false));
  log.push("returned " + v);
  await t;
  console.log("return inside: " + log.join(" "));
}

async function main() {
  await loop(
    "start",
    () => new ReadableStream({
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
    () => new ReadableStream({
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
    () => new ReadableStream({
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

  await slow("pull after 0 microtasks, an await not reached", 0);
  await slow("pull after 1 microtask", 1);
  await slow("pull after 2 microtasks", 2);
  await slow("pull after 3 microtasks", 3);
  await concurrent();
  await throwAt("throw at 2", 2, false);
  await throwAt("throw at 2, async cancel", 2, true);
  await breakAt("break at 1, async cancel", 1);
  await breakAt("break at 2, async cancel", 2);
  await returnInside();
}

main();
