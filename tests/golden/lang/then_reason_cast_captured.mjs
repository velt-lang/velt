// The Node twin of then_reason_cast_captured.velt.
function later(n) {
  return new Promise((r) => setTimeout(() => r(n), 1));
}

async function main() {
  let settled = null;
  queueMicrotask(() => {
    settled = later(1).then(
      (v) => {
        console.log("v", v);
      },
      (e) => {
        const x = e;
        console.log("e", x.message);
      },
    );
  });
  await later(0);
  if (settled !== null) {
    await settled;
  }
  console.log("done");
}
main();
