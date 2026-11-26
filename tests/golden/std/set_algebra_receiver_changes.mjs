class H extends Set {
  onHas = (x) => {};
  onKeys = () => {};
  has(x) {
    this.onHas(x);
    return super.has(x);
  }
  keys() {
    this.onKeys();
    return super.keys();
  }
}

function show(r) {
  return [...r].join(",");
}

/** `other` is bigger, so `has` is asked; asking about 1 swaps 2 for 9 in the receiver. */
function hasHook(run) {
  const a = new Set([1, 2, 3, 4]);
  const b = new H([1, 2, 3, 4, 9, 10]);
  b.onHas = (x) => {
    if (x == 1) {
      a.delete(2);
      a.add(9);
    }
  };
  return run(a, b);
}

/** `other` is smaller, so `keys` is read; reading it swaps 2 for 9 in the receiver. */
function keysHook(run) {
  const a = new Set([1, 2, 3, 4]);
  const b = new H([2, 9]);
  b.onKeys = () => {
    a.delete(2);
    a.add(9);
  };
  return run(a, b);
}

/** `other` is bigger; its first `has` empties the receiver. */
function clearing(run) {
  const a = new Set([1, 2, 3]);
  const b = new H([1, 2, 3, 4]);
  b.onHas = (x) => {
    a.clear();
  };
  return run(a, b);
}

function late(run) {
  const a = new Set([1, 2, 3, 4, 5]);
  const b = new H([2, 4, 5, 6, 7, 8, 20]);
  b.onHas = (x) => {
    if (x == 3) {
      a.delete(5);
      a.add(20);
      a.add(6);
    }
  };
  return run(a, b);
}

function row(name, run) {
  console.log(`${name}: has-hook ${hasHook(run)} | keys-hook ${keysHook(run)} | clearing ${clearing(run)} | late ${late(run)}`);
}

row("union", (a, b) => show(a.union(b)));
row("intersection", (a, b) => show(a.intersection(b)));
row("difference", (a, b) => show(a.difference(b)));
row("symmetricDifference", (a, b) => show(a.symmetricDifference(b)));
row("isSubsetOf", (a, b) => `${a.isSubsetOf(b)}`);
row("isSupersetOf", (a, b) => `${a.isSupersetOf(b)}`);
row("isDisjointFrom", (a, b) => `${a.isDisjointFrom(b)}`);
