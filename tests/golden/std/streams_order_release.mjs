// Node twin of streams_order_release.velt.
  const s = new ReadableStream({
    pull: (c) => {
      console.log("pull");
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
  r.read().then(() => console.log("read ok"), () => console.log("read0 rejected"));
  r.closed.then(() => console.log("closed ok"), () => console.log("closed rejected"));
  r.read().then(() => console.log("read ok"), () => console.log("read1 rejected"));
  r.releaseLock();
