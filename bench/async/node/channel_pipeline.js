// 4 producers send 250k numbers each through one bounded queue (capacity 1024) to a consumer
// that sums them. JS has no channels: a minimal promise-based bounded queue.
class Channel {
  constructor(capacity) {
    this.capacity = capacity;
    this.items = [];
    this.head = 0;
    this.closed = false;
    this.receivers = [];
    this.senders = [];
  }
  get length() {
    return this.items.length - this.head;
  }
  async send(v) {
    while (this.length >= this.capacity) {
      await new Promise((r) => this.senders.push(r));
    }
    this.items.push(v);
    const r = this.receivers.shift();
    if (r) r();
  }
  async receive() {
    while (this.length == 0) {
      if (this.closed) return null;
      await new Promise((r) => this.receivers.push(r));
    }
    const v = this.items[this.head++];
    if (this.head > 4096) {
      this.items = this.items.slice(this.head);
      this.head = 0;
    }
    const s = this.senders.shift();
    if (s) s();
    return v;
  }
  close() {
    this.closed = true;
    for (const r of this.receivers.splice(0)) r();
  }
}

async function produce(ch, id) {
  for (let i = 0; i < 250000; i++) {
    await ch.send(id * 250000 + i);
  }
}

async function main() {
  const ch = new Channel(1024);
  const producers = [];
  for (let id = 0; id < 4; id++) producers.push(produce(ch, id));
  const closer = Promise.all(producers).then(() => ch.close());
  let total = 0;
  let count = 0;
  while (true) {
    const v = await ch.receive();
    if (v == null) break;
    total += v;
    count++;
  }
  await closer;
  console.log(count, total);
}
main();
