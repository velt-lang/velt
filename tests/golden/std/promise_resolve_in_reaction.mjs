// The Node twin of tests/golden/std/promise_resolve_in_reaction.velt; its .out is what Node 22 prints.

const log           = [];

function say(s        )       {
  log.push(s);
}

/** `t0` … `t{n-1}`, one microtask apart; `at3` runs in `t3`'s callback after it logs. */
function ticks(n        , at3            )                {
  let p = Promise.resolve();
  for (let i = 0; i < n; i++) {
    const k = i;
    p = p.then(() => {
      say(`t${k}`);
      if (k === 3) {
        at3();
      }
    });
  }
  return p;
}

function flush(title        )       {
  console.log(`${title}: ${log.join(" ")}`);
  log.length = 0;
}

async function main() {
  {
    // The issue's repro.
    let late = () => {};
    const q = new Promise      ((r) => {
      late = r;
    });
    const p = ticks(6, () => late());
    q.then(() => say("direct"));
    await p;
    flush("resolved in a reaction");
  }
  {
    let fail = (e       ) => {};
    const q = new Promise             ((r, j) => {
      fail = j;
    });
    const p = ticks(6, () => fail(new Error("x")));
    q.catch((e) => say(`caught ${e.message}`));
    await p;
    flush("rejected in a reaction");
  }
  {
    const w = Promise.withResolvers               ();
    const p = ticks(6, () => w.resolve(1));
    w.promise.then((n) => say(`then ${n}`));
    const waiter = (async () => {
      say(`awaited ${await w.promise}`);
    })();
    await p;
    await waiter;
    flush("withResolvers, a then and an await");
  }
  {
    let late = (n        ) => {};
    const q = new Promise        ((r) => {
      late = r;
    });
    const p = ticks(6, () => {
      late(1);
      late(2);
      say("after");
    });
    q.then((n) => say(`got ${n}`));
    await p;
    flush("resolved twice in a reaction");
  }
  {
    let late = () => {};
    const q = new Promise      ((r) => {
      late = r;
    });
    const p = ticks(6, () => {
      queueMicrotask(() => {
        late();
        say("queued");
      });
    });
    q.then(() => say("direct"));
    await p;
    flush("resolved in a queued microtask");
  }
  {
    let late = () => {};
    const q = new Promise      ((r) => {
      late = r;
    });
    let go = () => {};
    const gate = new Promise      ((r) => {
      go = r;
    });
    const resolver = (async () => {
      await gate;
      late();
      say("resolver");
    })();
    const p = ticks(6, () => go());
    q.then(() => say("direct"));
    await p;
    await resolver;
    flush("resolved in an async function after its await");
  }
}
main();
