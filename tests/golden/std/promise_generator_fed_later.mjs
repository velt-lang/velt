// The Node twin of tests/golden/std/promise_generator_fed_later.velt; its .out is what Node 22 prints.

const log           = [];

function say(s        )       {
  log.push(s);
}

/** Logs `t0` … `t{n-1}`, one microtask apart, from the next microtask on. */
function ticks(n        )                {
  let p = Promise.resolve();
  for (let i = 0; i < n; i++) {
    p = p.then(() => say(`t${i}`));
  }
  return p;
}

/** Not called (`WHICH`): the four combinators over the same three pending promises. */
async function combinators()                {
  const rs                          = [];
  const ps = [0, 1, 2].map(
    (i) => new Promise        ((r) => {
      rs.push(r);
    }),
  );
  const c = ticks(8);
  const all = Promise.all(ps).then((v) => say(`all:${v.join("")}`));
  const race = Promise.race(ps).then((v) => say(`race:${v}`));
  const any = Promise.any(ps).then((v) => say(`any:${v}`));
  const settled = Promise.allSettled(ps).then((v) => say(`settled:${v.length}`));
  queueMicrotask(() => {
    rs[2](2);
    say("r2");
  });
  queueMicrotask(() => {
    rs[0](0);
    rs[1](1);
    say("r01");
  });
  await all;
  await race;
  await any;
  await settled;
  await c;
}

/** The generator's three promises, each resolved three microtasks after the last was. */
async function fedLater()                {
  const rs                          = [];
  async function* feed()                         {
    for (let i = 0; i < 3; i++) {
      yield await new Promise        ((r) => {
        rs.push(r);
      });
    }
  }
  const c = ticks(10);
  const reader = (async () => {
    for await (const v of feed()) {
      say(`item${v}`);
    }
    say("end");
  })();
  for (let i = 0; i < 3; i++) {
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
    rs[i](i);
    say(`fed${i}`);
  }
  await reader;
  await c;
  console.log(log.join(" "));
}

const WHICH         = "fed";
const TABLE                                  = [
  ["combinators", combinators],
  ["fed", fedLater],
];

async function main()                {
  for (const [k, f] of TABLE) {
    if (WHICH == k) {
      await f();
    }
  }
}

main();
