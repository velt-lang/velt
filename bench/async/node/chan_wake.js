// 64 tasks in a ring, each with its own channel, pass 8 tokens along (800k hops): every receive
// waits and is woken by the send of the task before it (chan_wake.velt). JS has no channels: a
// minimal promise-based unbounded queue.
class Channel {
  constructor() {
    this.items = [];
    this.closed = false;
    this.receivers = [];
  }
  trySend(v) {
    if (this.closed) return false;
    this.items.push(v);
    const r = this.receivers.shift();
    if (r) r();
    return true;
  }
  async receive() {
    while (this.items.length == 0) {
      if (this.closed) return null;
      await new Promise((r) => this.receivers.push(r));
    }
    return this.items.shift();
  }
  close() {
    this.closed = true;
    for (const r of this.receivers.splice(0)) r();
  }
}

const TASKS = 64;
const TOKENS = 8;
const HOPS = 100000;

async function member(inbox, next, done) {
  let seen = 0;
  while (true) {
    const v = await inbox.receive();
    if (v == null) return seen;
    seen++;
    if (v + 1 >= HOPS) {
      done.trySend(seen);
    } else {
      next.trySend(v + 1);
    }
  }
}

async function main() {
  const inboxes = [];
  for (let i = 0; i < TASKS; i++) inboxes.push(new Channel());
  const done = new Channel();
  const members = [];
  for (let i = 0; i < TASKS; i++) {
    members.push(member(inboxes[i], inboxes[(i + 1) % TASKS], done));
  }
  for (let t = 0; t < TOKENS; t++) inboxes[t * (TASKS / TOKENS)].trySend(0);
  for (let t = 0; t < TOKENS; t++) await done.receive();
  for (const inbox of inboxes) inbox.close();
  let hops = 0;
  for (const n of await Promise.all(members)) hops += n;
  console.log(hops);
}

main();
