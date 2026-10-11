// 4 owners each keep a Map; 64 callers make 5000 request/reply calls each to the owner of their
// key, through an async queue plus a promise per call (pingpong_pinned.vlt).
class Queue {
  constructor() { this.items = []; this.waiting = null; this.closed = false; }
  push(v) { if (this.waiting) { const w = this.waiting; this.waiting = null; w(v); } else this.items.push(v); }
  close() { this.closed = true; if (this.waiting) { const w = this.waiting; this.waiting = null; w(null); } }
  receive() {
    if (this.items.length) return Promise.resolve(this.items.shift());
    if (this.closed) return Promise.resolve(null);
    return new Promise((r) => { this.waiting = r; });
  }
}

async function owner(inbox) {
  const counts = new Map();
  for (;;) {
    const c = await inbox.receive();
    if (c === null) return;
    const n = (counts.get(c.key) ?? 0) + 1;
    counts.set(c.key, n);
    c.reply(n);
  }
}

async function caller(inbox, key, calls) {
  let sum = 0;
  for (let i = 0; i < calls; i++) {
    const { promise, resolve } = Promise.withResolvers();
    inbox.push({ key, reply: resolve });
    sum += await promise;
  }
  return sum;
}

async function main() {
  const inboxes = [], running = [];
  for (let i = 0; i < 4; i++) { const q = new Queue(); inboxes.push(q); running.push(owner(q)); }
  const callers = [];
  for (let c = 0; c < 64; c++) callers.push(caller(inboxes[c % 4], `k${c % 16}`, 5000));
  let total = 0;
  for (const r of await Promise.all(callers)) total += r;
  for (const q of inboxes) q.close();
  await Promise.all(running);
  console.log(total);
}
main();
