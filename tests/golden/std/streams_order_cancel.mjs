// Node twin of streams_order_cancel.velt.
async function noneCase() {
  const log = [];
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
      return new Promise((r) => setTimeout(r, 1));
    },
  });
  const r = s.getReader();
  r.read().then((v) => {
    log.push("read done=" + v.done);
  });
  r.closed.then(() => {
    log.push("closed");
  });
  queueMicrotask(() => {
    log.push("cancel");
    r.cancel("x").then(
      () => {
        log.push("cancel resolved");
      },
      (e) => {
        log.push("cancel rejected " + e.message);
      },
    );
  });
  await new Promise((r) => setTimeout(r, 20));
  console.log("none: " + log.join(" "));
}

async function syncCase() {
  const log = [];
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
      return new Promise((r) => setTimeout(r, 1));
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
  queueMicrotask(() => {
    log.push("cancel");
    r.cancel("x").then(
      () => {
        log.push("cancel resolved");
      },
      (e) => {
        log.push("cancel rejected " + e.message);
      },
    );
  });
  await new Promise((r) => setTimeout(r, 20));
  console.log("sync: " + log.join(" "));
}

async function asyncCase() {
  const log = [];
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
      return new Promise((r) => setTimeout(r, 1));
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
  queueMicrotask(() => {
    log.push("cancel");
    r.cancel("x").then(
      () => {
        log.push("cancel resolved");
      },
      (e) => {
        log.push("cancel rejected " + e.message);
      },
    );
  });
  await new Promise((r) => setTimeout(r, 20));
  console.log("async: " + log.join(" "));
}

async function throwsCase() {
  const log = [];
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
      return new Promise((r) => setTimeout(r, 1));
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
  queueMicrotask(() => {
    log.push("cancel");
    r.cancel("x").then(
      () => {
        log.push("cancel resolved");
      },
      (e) => {
        log.push("cancel rejected " + e.message);
      },
    );
  });
  await new Promise((r) => setTimeout(r, 20));
  console.log("throws: " + log.join(" "));
}

await noneCase();
await syncCase();
await asyncCase();
await throwsCase();
