// The Node twin of tests/golden/std/promise_settle_shared.velt; its .out is what Node 22 prints.

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

async function flush(title        )                {
  await ticks(8);
  console.log(`${title}: ${log.join(" ")}`);
  log.length = 0;
}

/** A pending promise and its resolver. */
class Gate {
  open                      = (n        ) => {};
  p                        ;

  constructor() {
    this.p = new Promise        ((resolve) => {
      this.open = resolve;
    });
  }
}

function widen(p                 )                  {
  return p;
}

/**
 * Resolves `g` with `n` now, or in a reaction (`inReaction`), and logs `o{n}` after: no code that
 * waits for the promise runs inside `resolve`, so that comes first.
 */
function settle(g      , n        , inReaction         )       {
  if (inReaction) {
    Promise.resolve().then(() => {
      g.open(n);
      say(`o${n}`);
    });
  } else {
    g.open(n);
    say(`o${n}`);
  }
}

async function shared(inReaction         )                {
  const how = inReaction ? "in a reaction" : "at once";
  {
    const g = new Gate();
    const chain = ticks(5);
    g.p.then((n) => say(`a${n}`));
    g.p.then((n) => say(`b${n}`));
    settle(g, 1, inReaction);
    await chain;
    await flush(`two thens, ${how}`);
  }
  {
    const g = new Gate();
    const chain = ticks(5);
    const waiter = (async () => {
      const n = await g.p;
      say(`x${n}`);
    })();
    g.p.then((n) => say(`a${n}`));
    settle(g, 2, inReaction);
    await chain;
    await waiter;
    await flush(`await and then, ${how}`);
  }
  {
    const g = new Gate();
    const alias                  = g.p;
    const chain = ticks(5);
    g.p.then((n) => say(`a${n}`));
    alias.then((n) => say(`A${n}`));
    g.p.then((n) => say(`b${n}`));
    widen(g.p).then((n) => say(`W${n}`));
    settle(g, 3, inReaction);
    await chain;
    await flush(`alias and promise, ${how}`);
  }
  {
    const g = new Gate();
    const twice = ()                  => widen(widen(g.p));
    const chain = ticks(5);
    twice().then((n) => say(`S${n}`));
    g.p.then((n) => say(`s${n}`));
    settle(g, 4, inReaction);
    await chain;
    await flush(`converted twice, ${how}`);
  }
  {
    // One `.then`: never shared, on time before too.
    const g = new Gate();
    const chain = ticks(5);
    g.p.then((n) => say(`a${n}`));
    settle(g, 5, inReaction);
    await chain;
    await flush(`one then, ${how}`);
  }
}

/** An async function awaiting a gate, reacted to twice: it goes on a step after `resolve`. */
async function stepAfter()                {
  const g = new Gate();
  const r = (async () => {
    const n = await g.p;
    say(`r${n}`);
    return n;
  })();
  const chain = ticks(5);
  r.then((n) => say(`a${n}`));
  r.then((n) => say(`b${n}`));
  settle(g, 6, true);
  await chain;
  await flush("async function awaiting it");
}

/** The same, the async function awaiting a promise of its own, made where it awaits it. */
async function stepAfterOwn()                {
  let open                      = (n        ) => {};
  const r = (async () => {
    const n = await new Promise        ((resolve) => {
      open = resolve;
    });
    say(`r${n}`);
    return n;
  })();
  const chain = ticks(5);
  r.then((n) => say(`a${n}`));
  r.then((n) => say(`b${n}`));
  Promise.resolve().then(() => {
    open(9);
    say("o9");
  });
  await chain;
  await flush("async function awaiting its own");
}

/** An async function kept and awaited by one place, awaiting a gate: as before. */
async function stepAfterOne()                {
  let open                      = (n        ) => {};
  const r = (async () => {
    const n = await new Promise        ((resolve) => {
      open = resolve;
    });
    say(`r${n}`);
    return n;
  })();
  const chain = ticks(5);
  r.then((n) => say(`a${n}`));
  Promise.resolve().then(() => {
    open(10);
    say("o10");
  });
  await chain;
  await flush("async function awaiting its own, one then");
}

/** `main`'s root awaiting a promise of its own: its step comes after `resolve`, as before. */
async function rootOwn()                {
  let open                      = (n        ) => {};
  const chain = ticks(5);
  Promise.resolve().then(() => {
    open(11);
    say("o11");
  });
  const n = await new Promise        ((resolve) => {
    open = resolve;
  });
  say(`r${n}`);
  await chain;
  await flush("the root awaiting its own");
}

async function* numbers(g      )                         {
  yield await g.p;
  yield 8;
}

/** An async generator's calls queue behind each other: each goes on a step after the last. */
async function generatorCalls()                {
  const g = new Gate();
  const it = numbers(g);
  const chain = ticks(6);
  it.next().then((r) => say(`n${r.value}`));
  it.next().then((r) => say(`m${r.value}`));
  settle(g, 7, true);
  await chain;
  await flush("async generator calls");
}

async function main() {
  await shared(false);
  await shared(true);
  await stepAfter();
  await stepAfterOwn();
  await stepAfterOne();
  await rootOwn();
  await generatorCalls();
}
main();
