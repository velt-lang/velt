// Node twin of streams_order_cancel_later.velt.
// at: the tick that settles the callback's promise; -1: settled already; -2: the callback throws.
async function laterCase(reject, at, pending, viaStream) {
  const log = [];
  let open = () => {};
  const gate = new Promise((r) => {
    open = r;
  });
  let late = () => {};
  let fail = (e) => {};
  const callback = new Promise((r, j) => {
    late = r;
    fail = j;
  });
  let p0 = Promise.resolve();
  for (let i = 0; i < 10; i++) {
    const k = i;
    p0 = p0.then(() => {
      log.push("t" + k);
      if (k === at) {
        if (reject) {
          fail(new Error("late"));
        } else {
          late();
        }
      }
    });
  }
  const s = new ReadableStream({
    pull: (c) => {
      log.push("pull");
      return pending ? gate : Promise.resolve();
    },
    cancel: (reason) => {
      log.push("cancel callback");
      if (at === -2) {
        throw new Error("thrown");
      }
      if (at === -1) {
        const now = new Promise((r, j) => {
          if (reject) {
            j(new Error("now"));
          } else {
            r();
          }
        });
        return now;
      }
      return callback;
    },
  });
  let settled = Promise.resolve();
  if (viaStream) {
    queueMicrotask(() => {
      log.push("cancel");
      settled = s.cancel("x").then(
        () => {
          log.push("cancel resolved");
        },
        (e) => {
          log.push("cancel rejected " + (e).message);
        },
      );
    });
  } else {
    const r = s.getReader();
    r.read().then((v) => {
      log.push("read done=" + v.done);
    });
    r.closed.then(() => {
      log.push("closed");
    });
    queueMicrotask(() => {
      log.push("cancel");
      settled = r.cancel("x").then(
        () => {
          log.push("cancel resolved");
        },
        (e) => {
          log.push("cancel rejected " + (e).message);
        },
      );
    });
  }
  await p0;
  await settled;
  open();
  console.log(
    `${reject ? "rej" : "res"} at=${at} pending=${pending} stream=${viaStream}: ${log.join(" ")}`,
  );
}

async function main() {
  for (const viaStream of [false, true]) {
    for (const pending of [true, false]) {
      for (const at of [-2, -1, 0, 1, 2, 3, 5]) {
        for (const reject of [false, true]) {
          if (at === -2 && reject) {
            continue;
          }
          await laterCase(reject, at, pending, viaStream);
        }
      }
    }
  }
}

main();
