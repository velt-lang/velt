// Node twin of streams_order_cancel.velt.
async function noneCase() {
  const log = [];
  // The pending pull's promise, settled at the end.
  let open = () => {};
  const gate = new Promise((r) => {
    open = r;
  });
  let p0 = Promise.resolve();
  for (let i = 0; i < 8; i++) {
    const k = i;
    p0 = p0.then(() => {
      log.push("t" + k);
    });
  }
  const s = new ReadableStream({
    pull: (c) => {
      log.push("pull");
      return gate;
    },
  });
  const r = s.getReader();
  r.read().then((v) => {
    log.push("read done=" + v.done);
  });
  r.closed.then(() => {
    log.push("closed");
  });
  let settled = Promise.resolve();
  queueMicrotask(() => {
    log.push("cancel");
    settled = r.cancel("x").then(
      () => {
        log.push("cancel resolved");
      },
      (e) => {
        log.push("cancel rejected " + e.message);
      },
    );
  });
  await p0;
  await settled;
  open();
  console.log("none: " + log.join(" "));
}

async function syncCase() {
  const log = [];
  // The pending pull's promise, settled at the end.
  let open = () => {};
  const gate = new Promise((r) => {
    open = r;
  });
  let p0 = Promise.resolve();
  for (let i = 0; i < 8; i++) {
    const k = i;
    p0 = p0.then(() => {
      log.push("t" + k);
    });
  }
  const s = new ReadableStream({
    pull: (c) => {
      log.push("pull");
      return gate;
    },
    cancel: (reason) => {
      log.push("cancelFn");
    },
  });
  const r = s.getReader();
  r.read().then((v) => {
    log.push("read done=" + v.done);
  });
  r.closed.then(() => {
    log.push("closed");
  });
  let settled = Promise.resolve();
  queueMicrotask(() => {
    log.push("cancel");
    settled = r.cancel("x").then(
      () => {
        log.push("cancel resolved");
      },
      (e) => {
        log.push("cancel rejected " + e.message);
      },
    );
  });
  await p0;
  await settled;
  open();
  console.log("sync: " + log.join(" "));
}

async function asyncCase() {
  const log = [];
  // The pending pull's promise, settled at the end.
  let open = () => {};
  const gate = new Promise((r) => {
    open = r;
  });
  let p0 = Promise.resolve();
  for (let i = 0; i < 8; i++) {
    const k = i;
    p0 = p0.then(() => {
      log.push("t" + k);
    });
  }
  const s = new ReadableStream({
    pull: (c) => {
      log.push("pull");
      return gate;
    },
    cancel: async (reason) => {
      log.push("cancelFn");
    },
  });
  const r = s.getReader();
  r.read().then((v) => {
    log.push("read done=" + v.done);
  });
  r.closed.then(() => {
    log.push("closed");
  });
  let settled = Promise.resolve();
  queueMicrotask(() => {
    log.push("cancel");
    settled = r.cancel("x").then(
      () => {
        log.push("cancel resolved");
      },
      (e) => {
        log.push("cancel rejected " + e.message);
      },
    );
  });
  await p0;
  await settled;
  open();
  console.log("async: " + log.join(" "));
}

async function throwsCase() {
  const log = [];
  // The pending pull's promise, settled at the end.
  let open = () => {};
  const gate = new Promise((r) => {
    open = r;
  });
  let p0 = Promise.resolve();
  for (let i = 0; i < 8; i++) {
    const k = i;
    p0 = p0.then(() => {
      log.push("t" + k);
    });
  }
  const s = new ReadableStream({
    pull: (c) => {
      log.push("pull");
      return gate;
    },
    cancel: (reason) => {
      log.push("cancelFn");
      throw new Error("cancel boom");
    },
  });
  const r = s.getReader();
  r.read().then((v) => {
    log.push("read done=" + v.done);
  });
  r.closed.then(() => {
    log.push("closed");
  });
  let settled = Promise.resolve();
  queueMicrotask(() => {
    log.push("cancel");
    settled = r.cancel("x").then(
      () => {
        log.push("cancel resolved");
      },
      (e) => {
        log.push("cancel rejected " + e.message);
      },
    );
  });
  await p0;
  await settled;
  open();
  console.log("throws: " + log.join(" "));
}

await noneCase();
await syncCase();
await asyncCase();
await throwsCase();
