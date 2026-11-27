// The Node twin of then_reason_cast_never.velt.
function later(n) {
  return new Promise((r) => setTimeout(() => r(n), 1));
}

async function main() {
  const s = later(1).then(
    (v) => {
      console.log("v", v);
    },
    (e) => {
      const x = e;
      console.log("e", x.message);
    },
  );
  await s;
  let m = "none";
  const t = later(2).then(
    (v) => {
      console.log("v", v);
    },
    (e) => {
      m = e.message;
    },
  );
  await t;
  console.log("m", m);
  await later(3).then(
    (v) => console.log("v", v),
    (e) => {
      const x = e;
      console.log("e", x.name);
    },
  );
  await later(4).catch((e) => {
    const x = e;
    console.log("caught", x.message);
    return 0;
  });
  console.log("done");
}
main();
