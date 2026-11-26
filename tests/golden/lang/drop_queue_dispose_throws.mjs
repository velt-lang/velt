// Node twin of drop_queue_dispose_throws.velt: the same program with its types removed. Node has
// no drops: the chain's hooks run where Velt drops it, from its head on, and the throw out of
// node 300's is uncaught (exit 1).

class Node {
  kids = [];

  constructor(id) {
    this.id = id;
  }

  [Symbol.dispose]() {
    if (this.id % 100 == 0) {
      console.log(`disposing ${this.id}`);
    }
    if (this.id == 300) {
      throw new Error(`node ${this.id} failed`);
    }
  }
}

function chain(n) {
  let head = new Node(n - 1);
  for (let i = n - 2; i >= 0; i--) {
    const b = new Node(i);
    b.kids.push(head);
    head = b;
  }
  return head;
}

function main() {
  {
    const c = chain(1000);
    console.log(`made ${c.id}`);
    for (let at = c; at !== undefined; at = at.kids[0]) {
      at[Symbol.dispose]();
    }
  }
  console.log("not reached");
}

main();
