// Node twin of streams_order_async_start.velt.
  let n = 0;
  const s = new ReadableStream({
    start: async (c) => {
      console.log("start");
    },
    pull: (c) => {
      n++;
      console.log("pull " + n);
      if (n <= 2) {
        c.enqueue(n);
      } else {
        c.close();
      }
    },
  });
  let p = Promise.resolve();
  for (let i = 0; i < 12; i++) {
    const k = i;
    p = p.then(() => {
      console.log("t" + k);
    });
  }
  const r = s.getReader();
  r.read().then((v) => console.log("read0 " + v.value));
  r.read().then((v) => console.log("read1 " + v.value));
  r.read().then((v) => console.log("read2 " + v.done));
  r.closed.then(() => console.log("closed"));
