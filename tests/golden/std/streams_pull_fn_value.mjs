// Node twin of streams_pull_fn_value.velt.
function maybe(n) {
  if (n > 2) {
    return Promise.reject(new Error("maybe " + n));
  }
  if (n > 0) {
    return Promise.resolve();
  }
  return undefined;
}

let declaredPulls = 0;
function declaredPull(c) {
  declaredPulls++;
  c.enqueue(declaredPulls);
  return maybe(declaredPulls);
}

function declaredStart(c) {
  return maybe(5);
}

function declaredCancel(reason) {
  return maybe(6);
}

async function drain(name, s) {
  const r = s.getReader();
  try {
    for (;;) {
      const x = await r.read();
      if (x.done) {
        console.log(name + " done");
        return;
      }
      console.log(name + " read " + x.value);
    }
  } catch (e) {
    console.log(name + " caught " + e.message);
  }
}

let arrowPulls = 0;
const arrowPull = (c) => {
  arrowPulls++;
  c.enqueue(arrowPulls);
  return maybe(arrowPulls);
};
await drain("const pull", new ReadableStream({ pull: arrowPull }));
await drain("declared pull", new ReadableStream({ pull: declaredPull }));

const arrowStart = (c) => maybe(4);
await drain("const start", new ReadableStream({ start: arrowStart }));
await drain("declared start", new ReadableStream({ start: declaredStart }));

async function cancelled(name, s) {
  try {
    await s.cancel("why");
    console.log(name + " resolved");
  } catch (e) {
    console.log(name + " caught " + e.message);
  }
}

const arrowCancel = (reason) => maybe(7);
await cancelled("const cancel", new ReadableStream({ cancel: arrowCancel }));
await cancelled("declared cancel", new ReadableStream({ cancel: declaredCancel }));
console.log("done");
