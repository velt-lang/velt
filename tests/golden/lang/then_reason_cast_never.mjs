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
  let n = 0;
  await later(5).then(
    (v) => {
      n += v;
    },
    (e) => {
      const x = e;
      n += x.message.length;
    },
  );
  const c = await later(6).catch((e) => e.message.length);
  await later(7)
    .then((v) => {
      n += v;
    })
    .catch((e) => {
      console.log("e", e.message);
    });
  console.log("n", n, "c", c);
  console.log("done");
}
main();
