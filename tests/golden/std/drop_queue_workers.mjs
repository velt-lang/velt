// Node twin of drop_queue_workers.velt: the same program with its types removed, `spawn` an
// ordinary call and `yieldNow` a resolved promise. Node has no drops: each structure's hooks run
// where Velt drops it (the end of its block), by `dropAll`.

class Log {
  count = 0;
  sum = 0;
}

class Link {
  next = [];

  constructor(id, log) {
    this.id = id;
    this.log = log;
  }

  [Symbol.dispose]() {
    this.log.count++;
    this.log.sum += this.id;
  }
}

class Entry {
  rest = new Map();

  constructor(id, log) {
    this.id = id;
    this.log = log;
  }

  [Symbol.dispose]() {
    this.log.count++;
    this.log.sum += this.id;
  }
}

/** Every hook of the structure from `root`, the parent's before its children's. */
function dropAll(root, children) {
  const stack = [root];
  while (stack.length > 0) {
    const n = stack.pop();
    n[Symbol.dispose]();
    for (const k of children(n).reverse()) {
      stack.push(k);
    }
  }
}

function links(n, log) {
  let head = new Link(n - 1, log);
  for (let i = n - 2; i >= 0; i--) {
    const l = new Link(i, log);
    l.next.push(head);
    head = l;
  }
  return head;
}

function entries(n, log) {
  let head = new Entry(n - 1, log);
  for (let i = n - 2; i >= 0; i--) {
    const e = new Entry(i, log);
    e.rest.set("next", head);
    head = e;
  }
  return head;
}

const yieldNow = () => Promise.resolve();
const spawn = (p) => p;

async function work(t) {
  const log = new Log();
  const n = 20000 + t * 5000;
  for (let round = 0; round < 5; round++) {
    {
      const l = links(n, log);
      dropAll(l, (x) => [...x.next]);
    }
    {
      const e = entries(n / 2, log);
      dropAll(e, (x) => [...x.rest.values()]);
    }
    await yieldNow();
  }
  return `task ${t}: ${log.count} disposed, id sum ${log.sum}`;
}

async function main() {
  const tasks = [];
  for (let t = 0; t < 4; t++) {
    tasks.push(spawn(work(t)));
  }
  for (const r of await Promise.all(tasks)) {
    console.log(r);
  }
}

main();
