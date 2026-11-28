// Node twin of object_literal_method_steps.velt: prints its .out.
const log = [];
async function tick(tag) {
  log.push(tag + ":start");
  if (tag == "never") {
    throw new Error("x");
  }
  return 1;
}
const job = {
  async run(tag) {
    log.push(tag + ":a");
    const x = await tick(tag);
    log.push(tag + ":b");
    await tick(tag + "2");
    log.push(tag + ":c");
    return x;
  },
};
const plain = {
  async run(tag) {
    log.push(tag + ":a");
    await tick(tag);
    log.push(tag + ":b");
    return 2;
  },
};
async function other(tag) {
  log.push(tag + ":1");
  await null;
  log.push(tag + ":2");
  await null;
  log.push(tag + ":3");
  await null;
  log.push(tag + ":4");
}
async function main() {
  await Promise.all([
    job.run("J").then(() => log.push("done")),
    other("O").then(() => log.push("done")),
    plain.run("P").then(() => log.push("done")),
    other("Q").then(() => log.push("done")),
  ]);
  const j2 = job;
  await Promise.all([j2.run("K").then(() => log.push("K done")), other("R")]);
  console.log(log.join(" "));
}
main();
