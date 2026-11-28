// Node twin of any_call_kept_message.velt: the same program with its types removed.
function check(x) {
  if (x < 0) throw new RangeError("negative");
  return x;
}
const table = { check, show: () => "shown" };
console.log(table.check(1));

class Failed extends Error {
  constructor(data) {
    super();
    this.data = data;
  }
  get message() {
    return "failed: " + this.data.show();
  }
}
try {
  throw new Failed(table);
} catch (e) {
  console.log(e.message);
}
