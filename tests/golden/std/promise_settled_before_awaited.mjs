// The Node twin of tests/golden/std/promise_settled_before_awaited.velt (same output).
function chain(log          , n        )                {
  let p = Promise.resolve();
  for (let i = 0; i < n; i++) {
    const k = i;
    p = p.then(() => {
      log.push("t" + k);
    });
  }
  return p;
}

// Runs `f` in the job `after` jobs after the next one (0: the next job).
function later(after        , f            )       {
  queueMicrotask(() => {
    if (after == 0) {
      f();
    } else {
      later(after - 1, f);
    }
  });
}

// How the promise is awaited: 0 `await`, 1 `then`, 2 `then` with a second callback, 3 `catch`,
// 4 `finally`, 5 an async function that returns it, 6 a `then` callback that returns it,
// 7 `Promise.all`, 8 `Promise.race`, 9 `Promise.allSettled`, 10 `Promise.any`.
const HOW = [
  "await",
  "then",
  "then, 2 callbacks",
  "catch",
  "finally",
  "async return",
  "then returns it",
  "all",
  "race",
  "allSettled",
  "any",
];

function attach(log          , how        , p                 )       {
  log.push("attach");
  const seen = (v        ) => {
    log.push("v" + v);
  };
  const failed = (e         ) => {
    log.push("e:" + (e         ).message);
  };
  if (how == 0) {
    const f = async () => {
      try {
        seen(await p);
      } catch (e) {
        failed(e);
      }
    };
    f().then(() => {});
  } else if (how == 1) {
    p.then(seen).catch(failed);
  } else if (how == 2) {
    p.then(seen, failed);
  } else if (how == 3) {
    p.catch((e) => {
      failed(e);
      return -1;
    }).then(seen);
  } else if (how == 4) {
    p.finally(() => {
      log.push("finally");
    }).then(seen, failed);
  } else if (how == 5) {
    const f = async () => p;
    f().then(seen, failed);
  } else if (how == 6) {
    Promise.resolve().then(() => p).then(seen, failed);
  } else if (how == 7) {
    Promise.all([p]).then((vs) => seen(vs[0]), failed);
  } else if (how == 8) {
    Promise.race([p]).then(seen, failed);
  } else if (how == 9) {
    Promise.allSettled([p]).then((rs) => {
      log.push(rs[0].status);
    });
  } else {
    Promise.any([p]).then(seen, (e) => {
      log.push("any failed");
    });
  }
}

// The promise is settled in the next job and awaited in a job `gap` turns later (0: the job
// right behind it; -1: the job before it, so the awaiter is waiting already).
async function settledThenAwaited(how        , gap        , reject         )                  {
  const log           = [];
  const { promise, resolve, reject: fail } = Promise.withResolvers        ();
  const settle = () => {
    log.push(reject ? "reject" : "resolve");
    if (reject) {
      fail(new Error("no"));
    } else {
      resolve(1);
    }
  };
  let t               ;
  if (gap < 0) {
    later(0, () => attach(log, how, promise));
    later(0, settle);
    t = chain(log, 9);
  } else if (gap == 0) {
    later(0, settle);
    later(0, () => attach(log, how, promise));
    t = chain(log, 9);
  } else {
    // The chain's first turn is queued between the two jobs: what is queued behind the
    // `resolve` and ahead of the awaiter.
    later(0, settle);
    t = chain(log, 9);
    later(gap - 1, () => attach(log, how, promise));
  }
  await t;
  return log.join(" ");
}

// Awaiters before and after the `resolve`, on one promise: the reactions of those waiting are
// queued inside `resolve`, in their order; the one that comes later is queued when it comes.
async function several(reject         )                  {
  const log           = [];
  const { promise, resolve, reject: fail } = Promise.withResolvers        ();
  const watcher = async (name        ) => {
    log.push(name + " waits");
    try {
      log.push(name + " got " + (await promise));
    } catch (e) {
      log.push(name + " caught " + (e         ).message);
    }
  };
  const t = chain(log, 6);
  const a = watcher("a");
  const b = watcher("b");
  promise.then(
    (v) => {
      log.push("then " + v);
    },
    (e) => {
      log.push("then caught");
    },
  );
  log.push("settle");
  if (reject) {
    fail(new Error("no"));
  } else {
    resolve(1);
  }
  log.push("settled");
  const c = watcher("c");
  await t;
  await Promise.all([a, b, c]);
  return log.join(" ");
}

// Settled with nobody waiting, awaited at once, and awaited after other work was queued.
async function settledAlone()                  {
  const log           = [];
  const first = Promise.withResolvers        ();
  first.resolve(1);
  const t = chain(log, 4);
  log.push("first " + (await first.promise));
  const second = Promise.withResolvers        ();
  second.reject(new Error("no"));
  try {
    await second.promise;
  } catch (e) {
    log.push("second caught");
  }
  await t;
  return log.join(" ");
}

// The promise a `then` callback returns is settled by a job queued before the callback ran
// (the issue's program): by a job queued by a job, or by an async function after one `await`.
async function adoptedEarlier(viaAsync         , adopt         )                  {
  const log           = [];
  let answer                               = null;
  const settle = () => {
    log.push("p1");
    const r = answer;
    if (r != null) {
      r(1);
    }
  };
  queueMicrotask(() => {
    if (viaAsync) {
      const f = async () => {
        await null;
        settle();
      };
      f().then(() => {});
    } else {
      queueMicrotask(settle);
    }
  });
  const t = chain(log, 10);
  let p                 ;
  if (adopt) {
    p = Promise.resolve().then(() => {
      const { promise, resolve } = Promise.withResolvers        ();
      answer = resolve;
      return promise;
    });
  } else {
    const { promise, resolve } = Promise.withResolvers        ();
    answer = resolve;
    p = promise;
  }
  p.then((v) => {
    log.push("v" + v);
  });
  await t;
  return log.join(" ");
}

// One function waits for a promise only it holds, and nobody waits for the function: `resolve`
// queues the function's next step and returns, it does not run the function on. `nested` puts
// the wait one call down, in a function the first one awaits.
async function innerWait(log          , p                 )                  {
  const v = await p;
  log.push("inner got " + v);
  return v;
}

async function waiter(log          , p                 , nested         )                {
  try {
    const v = nested ? await innerWait(log, p) : await p;
    log.push("got " + v);
  } catch (e) {
    log.push("caught " + (e         ).message);
  }
  log.push("goes on");
}

async function oneWaiting(nested         , reject         )                  {
  const log           = [];
  const t = chain(log, 4);
  const { promise, resolve, reject: fail } = Promise.withResolvers        ();
  const w = waiter(log, promise, nested);
  log.push("settle");
  if (reject) {
    fail(new Error("no"));
  } else {
    resolve(1);
  }
  log.push("settled");
  await t;
  await w;
  return log.join(" ");
}

// `resolve` is called by a function its caller awaits (the caller runs it on from its own
// `await`), for a promise another function waits for: that function gets its turn, and whoever
// awaits it hears of it.
async function resolver(log          , resolve                     )                {
  await null;
  log.push("resolve");
  resolve(1);
  log.push("resolved");
}

async function callsResolver(log          , resolve                     )                {
  const running = resolver(log, resolve);
  log.push("resolver started");
  await running;
  log.push("resolver done");
}

async function resolvedInsideAwaitedCall(nested         )                  {
  const log           = [];
  const t = chain(log, 5);
  const { promise, resolve } = Promise.withResolvers        ();
  const w = waiter(log, promise, nested);
  const c = callsResolver(log, resolve);
  await t;
  await w;
  await c;
  return log.join(" ");
}

// Combinators over promises settled by `resolve` while nothing else is queued: each settles in
// the reaction queued on its input inside `resolve`, among the turns of `await null`, in the
// order of the `resolve` calls. Run twice: the second time the task's queue is empty and its
// code runs straight on, where a settling left to a later poll was noticed only once awaited.
async function combinatorsWhenQuiet()                  {
  const log           = [];
  const a = Promise.withResolvers        ();
  const b = Promise.withResolvers        ();
  const c = Promise.withResolvers        ();
  const d = Promise.withResolvers        ();
  const all = Promise.all([a.promise]).then(() => {
    log.push("all");
  });
  const race = Promise.race([b.promise]).then(() => {
    log.push("race");
  });
  const settled = Promise.allSettled([c.promise]).then(() => {
    log.push("allSettled");
  });
  const any = Promise.any([d.promise]).then(() => {
    log.push("any");
  });
  d.resolve(4);
  c.resolve(3);
  b.resolve(2);
  a.resolve(1);
  for (let i = 1; i <= 4; i++) {
    log.push("r" + i);
    await null;
  }
  await all;
  await race;
  await settled;
  await any;
  return log.join(" ");
}

// Two ways of waiting over the same promises, resolved in one go: which finishes first, for every
// ordered pair of six ways (`A` the first one asked, `B` the second). `allSettled` attaches to
// every promise at the call, as `all` does.
const WAYS = ["all", "allSettled", "race", "any", "then", "loop"];

async function loopOver(ps                   )                {
  for (const p of ps) {
    await p;
  }
}

function wait(way        , ps                   , log          , tag        )                {
  const mark = () => {
    log.push(tag);
  };
  if (way == 0) {
    return Promise.all(ps).then(mark);
  }
  if (way == 1) {
    return Promise.allSettled(ps).then(mark);
  }
  if (way == 2) {
    return Promise.race(ps).then(mark);
  }
  if (way == 3) {
    return Promise.any(ps).then(mark, mark);
  }
  if (way == 4) {
    return ps[ps.length - 1].then(mark);
  }
  return loopOver(ps).then(mark);
}

async function pair(n        , a        , b        )                  {
  const log           = [];
  const rs                          = [];
  const ps                    = [];
  for (let i = 0; i < n; i++) {
    const { promise, resolve } = Promise.withResolvers        ();
    ps.push(promise);
    rs.push(resolve);
  }
  const x = wait(a, ps, log, "A");
  const y = wait(b, ps, log, "B");
  for (let i = 0; i < n; i++) {
    rs[i](i);
  }
  await x;
  await y;
  return log.join("");
}

async function main()                {
  for (let how = 0; how < HOW.length; how++) {
    for (const reject of [false, true]) {
      for (let gap = -1; gap < 4; gap++) {
        const what = `${HOW[how]}, ${reject ? "rejected" : "resolved"}, gap ${gap}`;
        console.log(what + ": " + (await settledThenAwaited(how, gap, reject)));
      }
    }
  }
  console.log("several, resolved: " + (await several(false)));
  console.log("several, rejected: " + (await several(true)));
  console.log("settled alone: " + (await settledAlone()));
  for (const nested of [false, true]) {
    for (const reject of [false, true]) {
      const what = `one waiting${nested ? ", nested" : ""}, ${reject ? "rejected" : "resolved"}`;
      console.log(what + ": " + (await oneWaiting(nested, reject)));
    }
  }
  console.log("resolved inside an awaited call: " + (await resolvedInsideAwaitedCall(false)));
  console.log("resolved inside an awaited call, nested: " + (await resolvedInsideAwaitedCall(true)));
  console.log("combinators: " + (await combinatorsWhenQuiet()));
  for (let n = 1; n <= 3; n++) {
    const line           = [];
    for (let a = 0; a < WAYS.length; a++) {
      for (let b = 0; b < WAYS.length; b++) {
        if (a != b) {
          line.push(`${WAYS[a]}/${WAYS[b]} ${await pair(n, a, b)}`);
        }
      }
    }
    console.log(`pairs over ${n}: ` + line.join(", "));
  }
  console.log("combinators again: " + (await combinatorsWhenQuiet()));
  console.log("async fn, adopted: " + (await adoptedEarlier(true, true)));
  console.log("job, adopted: " + (await adoptedEarlier(false, true)));
  console.log("async fn, direct: " + (await adoptedEarlier(true, false)));
  console.log("job, direct: " + (await adoptedEarlier(false, false)));
}

main();
