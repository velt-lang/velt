// Node twin of drop_queue_drain.velt: the same program with its types removed. Node has no
// drops, so each structure's hooks run where Velt drops it (the end of its scope, or of the
// dispose that made it), by `dropAll`: a node's hook before its children's, children in order,
// with a stack of its own (no recursion, so no depth limit either).

class Log {
  count = 0;
  sum = 0;
  first = "";

  note(id) {
    this.count++;
    this.sum += id;
    if (this.count <= 8) {
      this.first += `${id} `;
    }
  }
}

class Branch {
  kids = [];

  constructor(id, log) {
    this.id = id;
    this.log = log;
  }

  [Symbol.dispose]() {
    this.log.note(this.id);
  }
}

/** What Velt does when the last reference to `root` goes: every hook, the parent's first. */
function dropAll(root, children) {
  const stack = [root];
  while (stack.length > 0) {
    const n = stack.pop();
    n[Symbol.dispose]();
    const kids = children(n);
    for (let i = kids.length - 1; i >= 0; i--) {
      stack.push(kids[i]);
    }
  }
}

function chain(start, n, log) {
  let head = new Branch(start + n - 1, log);
  for (let i = n - 2; i >= 0; i--) {
    const b = new Branch(start + i, log);
    b.kids.push(head);
    head = b;
  }
  return head;
}

function last(b) {
  let at = b;
  while (at.kids.length > 0) {
    at = at.kids[0];
  }
  return at;
}

class Spawner {
  next = [];

  constructor(n, log, inner) {
    this.n = n;
    this.log = log;
    this.inner = inner;
  }

  [Symbol.dispose]() {
    this.log.note(this.n);
    if (this.n % 50 == 0) {
      const c = chain(0, 3000, this.inner);
      this.inner.note(-1 - last(c).id);
      dropAll(c, (b) => b.kids);
    }
  }
}

function spawners(n, log, inner) {
  let head = new Spawner(n - 1, log, inner);
  for (let i = n - 2; i >= 0; i--) {
    const s = new Spawner(i, log, inner);
    s.next.push(head);
    head = s;
  }
  return head;
}

function main() {
  const wide = new Log();
  {
    const root = chain(0, 300, wide);
    const end = last(root);
    for (let k = 0; k < 100; k++) {
      end.kids.push(chain(1000 + k * 1000, 1000, wide));
    }
    dropAll(root, (b) => b.kids);
  }
  console.log(`fan-out: ${wide.count} disposed, id sum ${wide.sum}`);

  const outer = new Log();
  const inner = new Log();
  {
    const s = spawners(400, outer, inner);
    dropAll(s, (x) => x.next);
  }
  console.log(`spawners: ${outer.count} disposed, id sum ${outer.sum}`);
  console.log(`their chains: ${inner.count} disposed, id sum ${inner.sum}`);

  const one = new Log();
  {
    const c = chain(0, 100000, one);
    dropAll(c, (b) => b.kids);
  }
  console.log(`chain: ${one.count} disposed, first ${one.first}`);
}

main();
